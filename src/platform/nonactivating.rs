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
//! property to set: the only way to answer differently is for the window to
//! be an instance of a class that answers differently.
//!
//! This is what Electron does for `focusable: false`, which is worth saying
//! because it was read out of the shipped framework rather than guessed at.
//! Its class `ElectronNSWindow` carries a `disableKeyOrMainWindow` flag and
//! overrides exactly those two methods. Electron can do that at construction
//! because it creates its own window. LocalFlow's window belongs to winit, so
//! the subclass is built at runtime and the existing window is pointed at it.
//!
//! The subclass adds no instance variables and overrides no layout or drawing
//! behaviour, so an existing window can be moved into it safely. It still
//! receives mouse events: a window that cannot become key can be clicked and
//! dragged, it simply does not take focus away from whatever has it.

use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;
use std::ffi::{c_char, c_void, CString};

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_allocateClassPair(
        superclass: *const AnyClass,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut AnyClass;
    fn objc_registerClassPair(class: *mut AnyClass);
    fn objc_getClass(name: *const c_char) -> *const AnyClass;
    fn class_addMethod(
        class: *mut AnyClass,
        name: Sel,
        implementation: *const c_void,
        types: *const c_char,
    ) -> Bool;
    fn object_setClass(object: *mut AnyObject, class: *const AnyClass) -> *const AnyClass;
}

const SUBCLASS_NAME: &str = "LocalFlowNonActivatingWindow";
/// Objective-C type encoding for a method returning BOOL and taking the two
/// implicit arguments every method takes, self and the selector.
const BOOL_METHOD_TYPES: &str = "B@:";

/// Always NO. This is the whole behaviour of the subclass.
extern "C" fn refuse(_this: *mut AnyObject, _cmd: Sel) -> Bool {
    Bool::NO
}

/// Make LocalFlow's own windows refuse keyboard focus.
///
/// Returns whether it succeeded, so the caller can report the failure rather
/// than leave the user wondering why clicking the capsule still steals focus.
/// Failing here costs only the improvement: the app works exactly as it did
/// before, so this is deliberately not fatal.
pub fn make_windows_non_activating() -> bool {
    let Some(marker) = MainThreadMarker::new() else {
        return false;
    };
    let Some(class) = non_activating_class() else {
        return false;
    };

    // Every window this process owns at startup is LocalFlow's own, and the
    // console does not exist yet. The console is a normal window and is left
    // alone: it has a title bar, and a window you can type in should take
    // focus when you click it.
    let application = NSApplication::sharedApplication(marker);
    let windows = application.windows();
    let mut changed = false;
    for window in windows.iter() {
        let pointer: *const _ = &*window;
        unsafe { object_setClass(pointer as *mut AnyObject, class) };
        changed = true;
    }
    changed
}

/// Build the subclass, or fetch it if a previous call already did.
///
/// Registering the same class name twice returns null rather than failing
/// loudly, which is why the existing one is looked up first.
fn non_activating_class() -> Option<*const AnyClass> {
    let name = CString::new(SUBCLASS_NAME).ok()?;
    let existing = unsafe { objc_getClass(name.as_ptr()) };
    if !existing.is_null() {
        return Some(existing);
    }

    let superclass_name = CString::new("NSWindow").ok()?;
    let superclass = unsafe { objc_getClass(superclass_name.as_ptr()) };
    if superclass.is_null() {
        return None;
    }

    let class = unsafe { objc_allocateClassPair(superclass, name.as_ptr(), 0) };
    if class.is_null() {
        return None;
    }

    let types = CString::new(BOOL_METHOD_TYPES).ok()?;
    let implementation = refuse as extern "C" fn(*mut AnyObject, Sel) -> Bool;
    for selector in [sel!(canBecomeKeyWindow), sel!(canBecomeMainWindow)] {
        let added = unsafe {
            class_addMethod(
                class,
                selector,
                implementation as *const c_void,
                types.as_ptr(),
            )
        };
        if !added.as_bool() {
            return None;
        }
    }

    unsafe { objc_registerClassPair(class) };
    Some(class as *const AnyClass)
}

/// Whether the capsule's window currently refuses focus.
///
/// Reported in the console so the answer is observable rather than assumed,
/// since the whole point is a thing that silently does not happen.
pub fn windows_are_non_activating() -> bool {
    let Some(marker) = MainThreadMarker::new() else {
        return false;
    };
    let application = NSApplication::sharedApplication(marker);
    application.windows().iter().any(|window| {
        let can_become_key: Bool = unsafe { msg_send![&*window, canBecomeKeyWindow] };
        !can_become_key.as_bool()
    })
}
