use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, EventField, KeyCode,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2_app_kit::NSWorkspace;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
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
    fn CGEventTapEnable(tap: *const std::ffi::c_void, enable: bool);
}

#[derive(Debug, Clone, Copy)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

/// A passive native event tap for the fixed MVP push-to-talk key: Right Option.
/// It reads raw key codes, avoiding keyboard-layout translation in third-party
/// hotkey crates and leaving the key event untouched for the operating system.
pub struct GlobalHotkey;

impl GlobalHotkey {
    pub fn right_option(
        wake_ui: impl Fn() + Send + 'static,
    ) -> Result<(Self, Receiver<HotkeyEvent>)> {
        let (event_tx, event_rx) = mpsc::channel();
        let (tap_tx, tap_rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let held = Arc::new(AtomicBool::new(false));
        let active = held.clone();

        // A HID event-tap callback must never wait for egui's repaint lock.
        // Forward events and request a repaint from an ordinary thread instead.
        std::thread::Builder::new()
            .name("localflow-hotkey-forwarder".into())
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
            .name("localflow-hotkey".into())
            .spawn(move || {
                let run_loop = CFRunLoop::get_current();
                let tap = CGEventTap::new(
                    CGEventTapLocation::HID,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    vec![CGEventType::FlagsChanged],
                    move |proxy, event_type, event| {
                        if matches!(
                            event_type,
                            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                        ) {
                            // Apple requires explicitly re-enabling a tap after a timeout.
                            unsafe { CGEventTapEnable(proxy, true) };
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
                // Core Foundation exposes the common-modes constant as an extern static.
                run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });
                tap.enable();
                let _ = ready_tx.send(Ok(()));
                CFRunLoop::run_current();
            })
            .context("Could not start macOS global hotkey listener")?;

        match ready_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(())) => Ok((Self, event_rx)),
            Ok(Err(error)) => Err(anyhow!(error)),
            Err(_) => Err(anyhow!("macOS global event tap did not become ready")),
        }
    }
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
pub fn insert_text(text: &str, target_pid: i32) -> Result<()> {
    if frontmost_application_pid() != Some(target_pid) {
        return Err(anyhow!(
            "The destination app changed while dictating; text was not inserted"
        ));
    }
    let mut clipboard = Clipboard::new().context("Could not access macOS pasteboard")?;
    clipboard
        .set_text(text)
        .context("Could not set macOS pasteboard")?;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_option_does_not_keep_right_option_pressed() {
        assert!(!right_option_is_down(CGEventFlags::CGEventFlagAlternate));
        assert!(right_option_is_down(CGEventFlags::from_bits_retain(
            DEVICE_RIGHT_OPTION_FLAG | CGEventFlags::CGEventFlagAlternate.bits(),
        )));
    }
}
