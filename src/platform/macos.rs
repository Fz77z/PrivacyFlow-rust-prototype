use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, EventField, KeyCode,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2_app_kit::{NSCursor, NSEvent, NSScreen, NSWorkspace};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use objc2_foundation::MainThreadMarker;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const V_KEYCODE: u16 = 0x09;
// Device-dependent right-Option bit from IOKit's NXEvent.h. The generic
// Alternate flag is set for either Option key and cannot detect release while
// the left Option key remains held.
const DEVICE_RIGHT_OPTION_FLAG: u64 = 0x40;

fn right_option_is_down(flags: CGEventFlags) -> bool {
    flags.bits() & DEVICE_RIGHT_OPTION_FLAG != 0
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    /// Whether this process may post synthetic keyboard events, which is what
    /// System Settings calls Accessibility.
    ///
    /// Declared as the `Boolean` it actually returns, which is an unsigned
    /// char, rather than as a Rust `bool`. A `bool` holding any byte other
    /// than 0 or 1 is undefined behaviour, and nothing in the C contract
    /// promises the API will only ever produce those two.
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: *const c_void,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(element: *const c_void, seconds: f32) -> i32;
}

/// How long an application is given to answer a question about its own
/// focus.
///
/// Measured rather than chosen: the slowest first answer observed across the
/// applications on this machine was 413 ms, from a Finder window whose
/// accessibility tree had not been built yet. Later answers from the same
/// application come back in single figures. A second is therefore slack
/// rather than a budget, and it is only ever spent immediately before a paste
/// that would otherwise go somewhere it should not.
const AX_TIMEOUT_SECONDS: f32 = 1.0;

/// Whether the thing with the keyboard in a given application can take typed
/// text.
///
/// Three answers rather than two, because applications differ in what they
/// will say. Most name their focused element and describe it accurately.
/// Some expose no accessibility tree at all and answer nothing, and their
/// silence is not a refusal: it means the question could not be asked, not
/// that the answer is no. Collapsing `Unknown` into `NotEditable` would stop
/// PrivacyFlow pasting into every such application, which is a large and silent
/// regression for the applications least able to report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedField {
    Editable,
    NotEditable,
    Unknown,
}

/// Ask an accessibility element for one attribute. The caller owns what comes
/// back and must release it.
unsafe fn ax_attribute(element: *const c_void, name: &str) -> Option<CFTypeRef> {
    let key = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    let status = AXUIElementCopyAttributeValue(element, key.as_concrete_TypeRef(), &mut value);
    if status != 0 || value.is_null() {
        return None;
    }
    Some(value)
}

/// What the focused element of `pid` says about itself.
///
/// The one question asked is whether the element's value can be set, which is
/// what "you can type here" means in accessibility terms. It was chosen from
/// what applications on this machine actually report: every real input says
/// yes, while a Finder window's file list, a browser's page body and a chat
/// application's channel list all say no. The obvious alternative, the
/// presence of a selected text range, was rejected because static message
/// text and non-editable lists both carry one.
pub fn focused_field(pid: i32) -> FocusedField {
    unsafe {
        let application = AXUIElementCreateApplication(pid);
        if application.is_null() {
            return FocusedField::Unknown;
        }
        AXUIElementSetMessagingTimeout(application, AX_TIMEOUT_SECONDS);
        let Some(focused) = ax_attribute(application, "AXFocusedUIElement") else {
            CFRelease(application as CFTypeRef);
            return FocusedField::Unknown;
        };
        let key = CFString::new("AXValue");
        let mut settable: u8 = 0;
        let status = AXUIElementIsAttributeSettable(
            focused as *const c_void,
            key.as_concrete_TypeRef(),
            &mut settable,
        );
        CFRelease(focused);
        CFRelease(application as CFTypeRef);
        // A non-zero status means the application could not answer, which is
        // not the same as it answering no.
        if status != 0 {
            return FocusedField::Unknown;
        }
        if settable != 0 {
            FocusedField::Editable
        } else {
            FocusedField::NotEditable
        }
    }
}

/// Whether macOS will actually deliver the synthetic keystrokes PrivacyFlow
/// uses to paste.
///
/// This has to be asked rather than inferred from the result of posting an
/// event. `CGEvent::post` returns nothing at all: without Accessibility the
/// system discards the event silently, so the paste looks like it worked,
/// the timings look healthy, and nothing arrives at the cursor.
pub fn can_synthesize_input() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Whether an observed event could have put the text cursor somewhere other
/// than where PrivacyFlow last left it.
///
/// Only presses count. A pointer crossing the screen moves nothing, and Right
/// Option arrives as a flags change rather than a key press, so holding push
/// to talk does not count as the user having moved.
fn moves_the_cursor(event_type: CGEventType) -> bool {
    matches!(
        event_type,
        CGEventType::KeyDown
            | CGEventType::LeftMouseDown
            | CGEventType::RightMouseDown
            | CGEventType::OtherMouseDown
    )
}

#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    Pressed,
    Released,
    /// The pointer entered or left the capsule's zone. It carries nothing,
    /// because its only job is to wake the UI, which then reads the pointer
    /// for itself.
    PointerCrossed,
}

/// The parts of the screen where the pointer's position matters to the
/// capsule, such as the capsule itself and the area around it, shared between
/// the event tap, which sees every movement, and the UI, which decides where
/// the zones are.
///
/// This is what lets the capsule's window pass clicks through. A window that
/// ignores the mouse also stops hearing it move, so it can no longer notice
/// someone approaching; the tap hears every movement regardless, and wakes
/// the UI only when the pointer crosses a zone's edge, not on every move.
#[derive(Clone, Default)]
pub struct PointerZone(Arc<ZoneState>);

#[derive(Default)]
struct ZoneState {
    /// Each zone's left, top, right and bottom, in window-space points. Empty
    /// while there is nothing to watch, which is whenever minimal mode is off.
    zones: Mutex<Vec<[f64; 4]>>,
    /// Which zones the pointer was last inside, one bit per zone, so there
    /// can be at most eight of them.
    inside: AtomicU8,
}

impl PointerZone {
    /// Sets where the zones are, or stops watching with an empty slice.
    pub fn watch(&self, zones: &[[f64; 4]]) {
        debug_assert!(zones.len() <= 8, "one bit per zone, so at most eight");
        if let Ok(mut current) = self.0.zones.lock() {
            current.clear();
            current.extend_from_slice(zones);
        }
    }

    /// Records the pointer at `x`, `y` and says whether that took it across
    /// any zone's edge. Never waits: the event tap calls this, and a tap that
    /// blocks is disabled by macOS. A reading lost to a busy lock is harmless,
    /// because the next movement is a fresh reading of the same question.
    fn crossed(&self, x: f64, y: f64) -> bool {
        let Ok(zones) = self.0.zones.try_lock() else {
            return false;
        };
        let inside = zones.iter().enumerate().fold(0u8, |inside, (index, zone)| {
            let [left, top, right, bottom] = *zone;
            let contains = x >= left && x <= right && y >= top && y <= bottom;
            if contains { inside | (1 << index) } else { inside }
        });
        self.0.inside.swap(inside, Ordering::Relaxed) != inside
    }
}

/// A passive native event tap for the fixed MVP push-to-talk key: Right Option.
/// It reads raw key codes, avoiding keyboard-layout translation in third-party
/// hotkey crates and leaving the key event untouched for the operating system.
///
/// The same tap also reports whether the user has pressed or clicked anything,
/// which is how PrivacyFlow knows its memory of the cursor has gone stale. The
/// events are counted, never inspected: nothing reads a key code from them.
#[derive(Default)]
pub struct GlobalHotkey {
    moved_cursor: CursorMoved,
    pointer_zone: PointerZone,
}

/// Whether the user has pressed or clicked anything, shared with whoever needs
/// to know. Handed to the pipeline worker, which is where insertions happen
/// and therefore where a stale memory of the cursor would do damage.
#[derive(Clone, Default)]
pub struct CursorMoved(Arc<AtomicBool>);

impl CursorMoved {
    /// Whether anything has been pressed or clicked since this was last asked.
    /// Asking clears it, because each dictation only cares about the interval
    /// since the one before it.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::Relaxed)
    }

    fn record(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl GlobalHotkey {
    /// A handle onto the same flag the tap writes.
    pub fn cursor_moved(&self) -> CursorMoved {
        self.moved_cursor.clone()
    }

    /// A handle onto the zone the tap watches the pointer for.
    pub fn pointer_zone(&self) -> PointerZone {
        self.pointer_zone.clone()
    }

    pub fn right_option(
        wake_ui: impl Fn() + Send + 'static,
    ) -> Result<(Self, Receiver<HotkeyEvent>)> {
        let (event_tx, event_rx) = mpsc::channel();
        let (tap_tx, tap_rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let held = Arc::new(AtomicBool::new(false));
        let active = held.clone();
        let moved_cursor = CursorMoved::default();
        let observed_movement = moved_cursor.clone();
        let pointer_zone = PointerZone::default();
        let watched_zone = pointer_zone.clone();

        // A HID event-tap callback must never wait for egui's repaint lock.
        // Forward events and request a repaint from an ordinary thread instead.
        std::thread::Builder::new()
            .name("privacyflow-hotkey-forwarder".into())
            .spawn(move || {
                while let Ok(event) = tap_rx.recv() {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                    wake_ui();
                }
            })
            .context("Could not start macOS hotkey forwarder")?;

        std::thread::Builder::new()
            .name("privacyflow-hotkey".into())
            .spawn(move || {
                let run_loop = CFRunLoop::get_current();
                // Re-enabling a disabled tap needs the tap's own mach port,
                // and the callback has to exist before the tap does. The
                // port is published here once the tap is built, and the
                // callback reads it back. Passing the proxy instead, which
                // is a different opaque pointer, traps on this machine the
                // moment macOS disables the tap.
                let tap_port = Arc::new(AtomicPtr::<c_void>::new(std::ptr::null_mut()));
                let callback_port = tap_port.clone();
                let tap = CGEventTap::new(
                    CGEventTapLocation::HID,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    vec![
                        CGEventType::FlagsChanged,
                        CGEventType::KeyDown,
                        CGEventType::LeftMouseDown,
                        CGEventType::RightMouseDown,
                        CGEventType::OtherMouseDown,
                        CGEventType::MouseMoved,
                        CGEventType::LeftMouseDragged,
                    ],
                    move |_proxy, event_type, event| {
                        if matches!(
                            event_type,
                            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                        ) {
                            // Apple requires explicitly re-enabling a tap after
                            // macOS disables it. The port is null only if this
                            // somehow fires before the tap finished being built,
                            // in which case there is nothing to re-enable yet.
                            let port = callback_port.load(Ordering::Acquire);
                            if !port.is_null() {
                                unsafe { CGEventTapEnable(port, true) };
                            }
                            return None;
                        }
                        if matches!(
                            event_type,
                            CGEventType::MouseMoved | CGEventType::LeftMouseDragged
                        ) {
                            // The event's location is in the same top-left,
                            // y-down points as window positions.
                            let point = event.location();
                            if watched_zone.crossed(point.x, point.y) {
                                let _ = tap_tx.send(HotkeyEvent::PointerCrossed);
                            }
                            return None;
                        }
                        // Recorded without reading the event: the only thing
                        // PrivacyFlow wants to know is that the user touched
                        // something, never what they touched.
                        if moves_the_cursor(event_type) {
                            observed_movement.record();
                            return None;
                        }
                        if matches!(event_type, CGEventType::FlagsChanged)
                            && event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE)
                                == KeyCode::RIGHT_OPTION as i64
                        {
                            let down = right_option_is_down(event.get_flags());
                            if down && !active.swap(true, Ordering::Relaxed) {
                                let _ = tap_tx.send(HotkeyEvent::Pressed);
                            } else if !down && active.swap(false, Ordering::Relaxed) {
                                let _ = tap_tx.send(HotkeyEvent::Released);
                            }
                        }
                        None
                    },
                );
                let Ok(tap) = tap else {
                    let _ = ready_tx.send(Err(
                        "macOS refused to create the global event tap".to_owned()
                    ));
                    return;
                };
                let Ok(source) = tap.mach_port.create_runloop_source(0) else {
                    let _ = ready_tx.send(Err(
                        "macOS could not create the event-tap run-loop source".to_owned(),
                    ));
                    return;
                };
                // Publish the port before the tap starts delivering events, so
                // a disable arriving immediately still finds something to
                // re-enable.
                tap_port.store(
                    tap.mach_port.as_concrete_TypeRef() as *mut c_void,
                    Ordering::Release,
                );
                // Core Foundation exposes the common-modes constant as an extern static.
                run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });
                tap.enable();
                let _ = ready_tx.send(Ok(()));
                CFRunLoop::run_current();
            })
            .context("Could not start macOS global hotkey listener")?;

        match ready_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => Ok((Self { moved_cursor, pointer_zone }, event_rx)),
            Ok(Err(error)) => Err(anyhow!(error)),
            Err(_) => Err(anyhow!("macOS global event tap did not become ready")),
        }
    }
}

/// What became of a finished dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insertion {
    /// Pasted at the cursor, which is the ordinary outcome.
    Pasted,
    /// Left on the pasteboard because the destination was no longer frontmost.
    /// The words survived; they just need a paste.
    CopiedOnly,
    /// Left on the pasteboard because the destination had nothing focused
    /// that could receive text. Pasting anyway is not harmless: a Cmd-V into
    /// a Finder window pastes files, and into an application with its own
    /// binding it fires that instead.
    CopiedNoField,
}

/// Where the pointer is, in the same downward-y coordinates window positions
/// and work areas use.
///
/// This is the one the rest of the app should use. `NSEvent::mouseLocation`
/// measures upwards from the bottom of the primary display, which is the
/// opposite of everything else here, and a second coordinate convention is
/// how sign errors get in.
///
/// Both failure cases below are programming errors, not runtime conditions:
/// this is only ever called from the egui update loop on the main thread,
/// with at least one screen attached. Returning a coordinate in the wrong
/// space if either ever fired would invert the drag silently, so both panic
/// instead of quietly handing back a y-up number to a caller that assumes
/// y-down.
pub fn pointer_in_window_space() -> (f64, f64) {
    let point = NSEvent::mouseLocation();
    let marker = MainThreadMarker::new()
        .expect("pointer position must be read on the main thread");
    let screens = NSScreen::screens(marker);
    let primary = screens.firstObject().expect("no screen is attached");
    (point.x, primary.frame().size.height - point.y)
}

/// Show the ordinary arrow while the pointer is over the capsule.
///
/// The capsule never becomes the key window, so macOS ignores the cursor
/// winit asks for on its behalf: cursor rectangles only apply to the key
/// window. The cursor was left as whatever the app underneath last set,
/// which over a text editor is an I-beam, so the capsule claims it directly.
pub fn show_arrow_cursor() {
    NSCursor::arrowCursor().set();
}

/// A display's usable area: the screen minus the menu bar and the Dock.
///
/// Expressed in the same coordinates window positions use, with the origin at
/// the top left of the primary display and y increasing downwards, so it can
/// be compared against a remembered position without converting either.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where the user can actually see things, one entry per connected display.
///
/// An empty result means the question could not be asked, not that there are
/// no displays. Callers must treat it as "unknown" rather than as "nowhere is
/// valid" or "anywhere is valid".
pub fn work_areas() -> Vec<WorkArea> {
    let Some(marker) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let screens = NSScreen::screens(marker);
    // AppKit's coordinates start at the bottom left of the primary display,
    // which is the first screen, and every other display is placed relative to
    // it. Flipping to downward-y therefore measures from that one screen's
    // height, which is also what winit does when it places a window.
    let Some(primary) = screens.firstObject() else {
        return Vec::new();
    };
    let primary_height = primary.frame().size.height;
    screens
        .iter()
        .map(|screen| {
            let frame = screen.visibleFrame();
            WorkArea {
                x: frame.origin.x,
                y: primary_height - (frame.origin.y + frame.size.height),
                width: frame.size.width,
                height: frame.size.height,
            }
        })
        .collect()
}

/// Return the frontmost application's process ID without activating it.
pub fn frontmost_application_pid() -> Option<i32> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|application| application.processIdentifier())
        .filter(|pid| *pid > 0)
}

/// Paste through the system pasteboard and a session-scoped Cmd-V event.
/// The transcript stays on the pasteboard: restoring it on a timer can race a
/// busy target application's asynchronous paste handling and insert stale data.
/// Accessibility permission is required for the synthesized key event.
/// Put text on the pasteboard and nothing else.
///
/// Used to preserve a dictation whose pipeline failed after the words existed.
/// Deliberately separate from insertion: preserving is not inserting, and text
/// that failed its processing must never be typed into the user's document as
/// though it had succeeded.
pub fn copy_to_pasteboard(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("Could not access macOS pasteboard")?;
    clipboard
        .set_text(text)
        .context("Could not set macOS pasteboard")
}

pub fn insert_text(text: &str, target_pid: i32) -> Result<Insertion> {
    // The pasteboard is written first, before anything that can refuse, so
    // that every path from here on leaves the user holding their words. An
    // earlier version checked the permission first and returned without
    // writing, which meant the one case where the user most needed the
    // transcript, the case where PrivacyFlow could not place it for them, was
    // the case that threw it away.
    copy_to_pasteboard(text)?;
    // Reported as a failure rather than as CopiedOnly, even though the words
    // survived both ways. CopiedOnly means the destination moved, which is
    // ordinary and carries no remedy. This is a configuration fault with a
    // specific fix, and showing it as a quiet success would hide the only
    // message that says how to repair it.
    if !can_synthesize_input() {
        return Err(anyhow!(
            "PrivacyFlow is not allowed to send keystrokes, so the text could not be \
             pasted. It is on your clipboard: press Cmd-V to place it. To fix this \
             permanently, add PrivacyFlow to System Settings, Privacy and Security, \
             Accessibility, then quit and relaunch it."
        ));
    }
    // The pasteboard is written before this is checked, so that a dictation
    // whose destination has gone away still leaves the user holding their
    // words. Pasting somewhere the user is no longer looking would put text
    // into the wrong document, which is worse than not pasting at all.
    if frontmost_application_pid() != Some(target_pid) {
        return Ok(Insertion::CopiedOnly);
    }
    // Only a positive "that is not a text field" stops the paste. See
    // `FocusedField` for why the application's silence does not.
    if focused_field(target_pid) == FocusedField::NotEditable {
        return Ok(Insertion::CopiedNoField);
    }
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| anyhow!("Could not create keyboard event source"))?;
    let down = CGEvent::new_keyboard_event(source.clone(), V_KEYCODE, true)
        .map_err(|_| anyhow!("Could not create paste key-down event"))?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::Session);
    let up = CGEvent::new_keyboard_event(source, V_KEYCODE, false)
        .map_err(|_| anyhow!("Could not create paste key-up event"))?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::Session);
    Ok(Insertion::Pasted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tap sees every movement of the pointer anywhere on screen, and
    /// each report wakes the UI, so it must speak only when the pointer
    /// crosses a zone's edge. A zone that reported every move would repaint
    /// the capsule the whole time the user used their mouse. The zones nest,
    /// the capsule inside the area around it, and crossing either counts.
    #[test]
    fn the_pointer_zone_reports_crossings_not_movements() {
        let zone = PointerZone::default();
        let capsule = [100.0, 100.0, 200.0, 150.0];
        let around = [0.0, 0.0, 400.0, 300.0];
        zone.watch(&[capsule, around]);
        assert!(!zone.crossed(500.0, 500.0), "outside to outside is not a crossing");
        assert!(zone.crossed(50.0, 50.0), "coming near is");
        assert!(!zone.crossed(60.0, 55.0), "moving about nearby is not");
        assert!(zone.crossed(150.0, 120.0), "reaching the capsule is");
        assert!(!zone.crossed(160.0, 125.0), "moving about on it is not");
        assert!(zone.crossed(50.0, 50.0), "backing off to nearby is");
        assert!(zone.crossed(500.0, 500.0), "leaving altogether is");
        zone.watch(&[]);
        assert!(!zone.crossed(150.0, 120.0), "zones that are not watched have no inside");
    }

    #[test]
    fn typing_and_clicking_put_the_cursor_somewhere_privacyflow_does_not_know() {
        assert!(moves_the_cursor(CGEventType::KeyDown));
        assert!(moves_the_cursor(CGEventType::LeftMouseDown));
        assert!(moves_the_cursor(CGEventType::RightMouseDown));
    }

    #[test]
    fn holding_push_to_talk_leaves_the_cursor_where_it_was() {
        // Right Option arrives as a flags change rather than a key press, so
        // starting a dictation must not invalidate the dictation before it.
        assert!(!moves_the_cursor(CGEventType::FlagsChanged));
    }

    #[test]
    fn pointing_at_something_without_clicking_leaves_the_cursor_where_it_was() {
        assert!(!moves_the_cursor(CGEventType::MouseMoved));
        assert!(!moves_the_cursor(CGEventType::ScrollWheel));
    }

    #[test]
    fn left_option_does_not_keep_right_option_pressed() {
        assert!(!right_option_is_down(CGEventFlags::CGEventFlagAlternate));
        assert!(right_option_is_down(CGEventFlags::from_bits_retain(
            DEVICE_RIGHT_OPTION_FLAG | CGEventFlags::CGEventFlagAlternate.bits(),
        )));
    }
}
