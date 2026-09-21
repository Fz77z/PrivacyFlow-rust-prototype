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
//! the subclass is declared here and the existing window is pointed at it.
//!
//! Three things make moving a live window into this class safe, and all three
//! are conditions rather than hopes:
//!
//! 1. The subclass adds no instance variables and overrides nothing to do
//!    with layout or drawing, so the instance size is unchanged and no
//!    behaviour is lost. winit's own window class is likewise plain.
//! 2. Nothing in winit asks the window for its class; it is identified by
//!    pointer.
//! 3. This runs at startup, before anything can have registered a
//!    key-value observer. That matters: KVO works by secretly substituting an
//!    `NSKVONotifying_` subclass, and changing the class afterwards would
//!    discard it.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, ClassType, MainThreadOnly};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

define_class!(
    // SAFETY:
    // - NSWindow has no subclassing requirements beyond being used on the
    //   main thread, which the thread kind below enforces.
    // - The class adds no instance variables, so an existing window can be
    //   moved into it without changing its size.
    // - It does not implement Drop.
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "LocalFlowNonActivatingWindow"]
    struct NonActivatingWindow;

    impl NonActivatingWindow {
        /// Always no. Between them, these two are the entire subclass.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

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
    // AppKit windows may only be touched from the main thread, and this
    // class is declared main-thread-only to say so.
    if MainThreadMarker::new().is_none() {
        return false;
    }
    let Some(window) = capsule_window(handle) else {
        return false;
    };
    let class = NonActivatingWindow::class();
    // SAFETY: the class is a direct subclass of NSWindow, adds no instance
    // variables, and overrides only the two focus predicates. See the module
    // documentation for why doing this to a live window is sound here.
    unsafe {
        let object: &AnyObject = &window;
        AnyObject::set_class(object, class);
    }
    !window.canBecomeKeyWindow()
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
