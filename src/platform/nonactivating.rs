//! Stop the capsule taking keyboard focus when it is clicked or dragged.
//!
//! The capsule floats over whatever you are writing in. Clicking it to drag
//! it, or to open the console, used to make LocalFlow the active application,
//! which meant the next dictation had nowhere to go and failed with "No text
//! field focused". The user had to click back into their document first,
//! every time.
//!
//! macOS decides whether a window may take focus by asking the window itself,
//! through `canBecomeKeyWindow` and `canBecomeMainWindow`. There is no
//! property to set: the only way to answer differently is to change what
//! those two methods do.
//!
//! This is what Electron does for `focusable: false`, which is worth saying
//! because it was read out of the shipped framework rather than guessed at.
//! Its class `ElectronNSWindow` carries a `disableKeyOrMainWindow` flag and
//! overrides exactly those two methods. Electron can do that at construction
//! because it creates its own window. LocalFlow's window belongs to winit.
//!
//! ## Why this does not change the window's class
//!
//! It used to. A subclass was declared and the existing window was pointed at
//! it with `object_setClass`. That worked, and it crashed the application on
//! every quit:
//!
//! ```text
//! Cannot remove an observer <WinitWindowDelegate> for the key path
//! "effectiveAppearance" from <LocalFlowNonActivatingWindow> because it is
//! not registered as an observer.
//! ```
//!
//! Key-value observing works by secretly substituting an `NSKVONotifying_`
//! subclass of the observed object's class. winit registers an observer for
//! `effectiveAppearance` while it is creating the window, and unregisters it
//! when the delegate drops. Changing the class between those two points
//! discards the subclass that carries the registration, so the unregister
//! throws, and an uncaught Objective-C exception aborts the process.
//!
//! The abort had a second symptom that looked unrelated: aborting skips every
//! `Drop`, including `KevWorker`'s, so the Python worker was orphaned and
//! held the terminal until it was interrupted by hand.
//!
//! An earlier review of the `object_setClass` version named this exact risk,
//! and the module said in reply that it ran before anything could have
//! registered an observer. That was simply untrue, and it was never checked.
//!
//! So the methods are replaced on winit's own window class instead, and the
//! replacement answers for one specific window and defers to the original
//! implementation for every other. The console is a normal window and keeps
//! AppKit's behaviour: it has a title bar, and a window you can type in
//! should take focus when you click it. No object's class ever changes, so
//! there is nothing for KVO to lose.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{msg_send, sel};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ffi::{c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn class_replaceMethod(
        class: *const AnyClass,
        name: Sel,
        implementation: *const c_void,
        types: *const c_char,
    ) -> *const c_void;
    fn method_getTypeEncoding(method: *const c_void) -> *const c_char;
}

/// A method taking no arguments and answering yes or no, which is the shape
/// of both methods replaced here.
type Predicate = unsafe extern "C" fn(*mut AnyObject, Sel) -> Bool;

/// The one window that must refuse focus. Every other window reaching the
/// replacements below is answered by AppKit's own implementation.
static CAPSULE: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
static ORIGINAL_KEY: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static ORIGINAL_MAIN: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// The replacements are installed once per process. Installing twice would
/// capture our own implementation as the original and recurse forever.
static INSTALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn can_become_key_window(this: *mut AnyObject, cmd: Sel) -> Bool {
    answer(this, cmd, &ORIGINAL_KEY)
}

extern "C" fn can_become_main_window(this: *mut AnyObject, cmd: Sel) -> Bool {
    answer(this, cmd, &ORIGINAL_MAIN)
}

/// No for the capsule, and whatever AppKit would have said for anything else.
fn answer(this: *mut AnyObject, cmd: Sel, original: &AtomicPtr<c_void>) -> Bool {
    if ptr::eq(this, CAPSULE.load(Ordering::Acquire)) {
        return Bool::NO;
    }
    let original = original.load(Ordering::Acquire);
    if original.is_null() {
        // Unreachable: the originals are stored before the replacements are
        // installed, so nothing can dispatch here without them. Answering no
        // rather than guessing yes keeps a wrong answer safe, since a window
        // that declines focus is recoverable and one that steals it is the
        // bug this module exists to prevent.
        return Bool::NO;
    }
    // SAFETY: the pointer came from the method being replaced, which has
    // exactly this signature, and it is written once before any dispatch.
    let original: Predicate = unsafe { std::mem::transmute(original) };
    unsafe { original(this, cmd) }
}

/// Make the capsule's window refuse keyboard focus, and report whether it
/// worked.
///
/// The answer is verified by asking the window afterwards rather than
/// inferred from the fact that the call was made, because the whole feature
/// is a thing that silently does not happen and an unchecked claim about it
/// would be worth nothing.
///
/// Failing costs only the improvement: the app behaves exactly as it did
/// before, so this is deliberately not fatal. The caller reports it.
pub fn make_capsule_non_activating(handle: &impl HasWindowHandle) -> bool {
    // AppKit windows may only be touched from the main thread.
    if MainThreadMarker::new().is_none() {
        return false;
    }
    let Some(window) = capsule_window(handle) else {
        return false;
    };
    let pointer: *const AnyObject = &*window as &AnyObject;
    CAPSULE.store(pointer as *mut AnyObject, Ordering::Release);

    if !INSTALLED.swap(true, Ordering::AcqRel) {
        // `class` rather than `object_getClass`: if KVO has already inserted
        // its subclass, it hides itself from `class`, and replacing methods
        // on the subclass it owns would be replacing them on a class it may
        // discard.
        let class: *const AnyClass = unsafe { msg_send![&*window, class] };
        let installed = replace(class, sel!(canBecomeKeyWindow), can_become_key_window as *const c_void, &ORIGINAL_KEY)
            && replace(class, sel!(canBecomeMainWindow), can_become_main_window as *const c_void, &ORIGINAL_MAIN);
        if !installed {
            return false;
        }
    }

    !window.canBecomeKeyWindow()
}

/// Replace one method, remembering what was there so the replacement can
/// defer to it for every window that is not the capsule.
///
/// The type encoding is copied from the method being replaced rather than
/// written out by hand. A hand-written `BOOL` encoding is correct only on
/// Apple Silicon, where `BOOL` is `_Bool`; elsewhere it is `signed char`, and
/// the previous version of this file had that bug latent in it.
fn replace(
    class: *const AnyClass,
    selector: Sel,
    implementation: *const c_void,
    original: &AtomicPtr<c_void>,
) -> bool {
    // SAFETY: the class is a live Objective-C class and the selector is one
    // NSWindow defines, so the method is found on it or on a superclass.
    let Some(method) = (unsafe { class.as_ref() }).and_then(|class| class.instance_method(selector))
    else {
        return false;
    };
    let types = unsafe { method_getTypeEncoding(method as *const _ as *const c_void) };
    if types.is_null() {
        return false;
    }
    let previous = method.implementation() as *const c_void;
    original.store(previous as *mut c_void, Ordering::Release);
    // SAFETY: the replacement has the signature the encoding describes, and
    // the encoding is the one the replaced method itself carries.
    unsafe { class_replaceMethod(class, selector, implementation, types) };
    true
}

/// The `NSWindow` behind eframe's window handle.
///
/// Named explicitly rather than found by searching the application's windows.
/// Searching happened to work, because the console does not exist yet at
/// startup, but it reasoned about a set this code does not control.
fn capsule_window(handle: &impl HasWindowHandle) -> Option<Retained<NSWindow>> {
    let handle = handle.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: AppKit window handles carry a pointer to a live NSView owned by
    // the window that is being created around this call.
    let view: &NSView = unsafe { appkit.ns_view.cast::<NSView>().as_ref() };
    unsafe { msg_send![view, window] }
}
