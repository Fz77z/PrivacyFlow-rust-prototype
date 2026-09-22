//! LocalFlow's icon in the menu bar.
//!
//! In minimal mode the capsule hides whenever it is not in use, so it can no
//! longer be the app's only way in. The menu bar icon is the permanent home
//! instead: it opens the console, quits, and turns red when a dictation has
//! failed and the user has not yet looked at why.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use std::sync::mpsc::{self, Receiver, Sender};

/// What the user picked from the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    /// Bring the hidden capsule up for a moment, so the user can see where
    /// it lives and move it.
    ShowCapsule,
    OpenConsole,
    Quit,
}

/// What the menu's target needs: somewhere to send the choice, and a way to
/// wake the UI so it acts on it now rather than on its next unrelated frame.
struct Ivars {
    choices: Sender<MenuChoice>,
    wake_ui: Box<dyn Fn()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LocalFlowMenuTarget"]
    #[ivars = Ivars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(showCapsule:))]
        fn show_capsule(&self, _sender: Option<&AnyObject>) {
            self.choose(MenuChoice::ShowCapsule);
        }

        #[unsafe(method(openConsole:))]
        fn open_console(&self, _sender: Option<&AnyObject>) {
            self.choose(MenuChoice::OpenConsole);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            self.choose(MenuChoice::Quit);
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// The UI owns the receiving end for as long as the app runs, so a failed
    /// send can only happen during shutdown, when there is nothing left to do.
    fn choose(&self, choice: MenuChoice) {
        let _ = self.ivars().choices.send(choice);
        (self.ivars().wake_ui)();
    }
}

/// One entry in the menu, sending `action` to the target.
fn menu_item(
    mtm: MainThreadMarker,
    target: &MenuTarget,
    title: &str,
    action: objc2::runtime::Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: every action passed here is a method MenuTarget defines, taking
    // the sender as its only argument, which is what AppKit sends.
    let entry = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    };
    let target: &AnyObject = target;
    unsafe { entry.setTarget(Some(target)) };
    entry
}

/// The installed icon. Dropping it removes the icon from the menu bar.
pub struct StatusItem {
    item: Retained<NSStatusItem>,
    /// Menu items hold their target weakly, so the target has to be kept
    /// alive here for as long as the menu exists.
    _target: Retained<MenuTarget>,
    /// The alert last shown, so the icon is only touched when it changes.
    shown_alert: Option<String>,
}

impl StatusItem {
    /// Puts the icon in the menu bar. Must be called on the main thread,
    /// which is where the UI is built; anywhere else is a programming error.
    pub fn install(wake_ui: impl Fn() + 'static) -> (Self, Receiver<MenuChoice>) {
        let mtm = MainThreadMarker::new().expect("the menu bar icon must be made on the main thread");
        let (choices, received) = mpsc::channel();
        let target = MenuTarget::new(mtm, Ivars { choices, wake_ui: Box::new(wake_ui) });

        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let button = item.button(mtm).expect("a new status item always has a button");
        // A template image, so macOS draws it in the menu bar's own colour in
        // light and dark mode and inverts it while the menu is open.
        let icon = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("waveform"),
            Some(&NSString::from_str("LocalFlow")),
        )
        .expect("the waveform symbol ships with every macOS LocalFlow runs on");
        icon.setTemplate(true);
        button.setImage(Some(&icon));
        button.setToolTip(Some(&NSString::from_str("LocalFlow")));

        let menu = NSMenu::new(mtm);
        menu.addItem(&menu_item(mtm, &target, "Show Capsule", sel!(showCapsule:), ""));
        menu.addItem(&menu_item(mtm, &target, "Open Console", sel!(openConsole:), ""));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&menu_item(mtm, &target, "Quit LocalFlow", sel!(quit:), "q"));
        item.setMenu(Some(&menu));

        (Self { item, _target: target, shown_alert: None }, received)
    }

    /// Shows an unread failure on the icon, or clears it with None. The icon
    /// turns red and its tooltip carries the failure's detail, which is the
    /// same message the capsule's cog shows when the capsule is open.
    pub fn show_alert(&mut self, detail: Option<&str>) {
        if self.shown_alert.as_deref() == detail {
            return;
        }
        self.shown_alert = detail.map(str::to_owned);
        let mtm = MainThreadMarker::new().expect("the menu bar icon must be changed on the main thread");
        let button = self.item.button(mtm).expect("a status item always has a button");
        let tint = detail.map(|_| NSColor::systemRedColor());
        button.setContentTintColor(tint.as_deref());
        let tooltip = match detail {
            Some(detail) => format!("LocalFlow: {detail}"),
            None => "LocalFlow".to_owned(),
        };
        button.setToolTip(Some(&NSString::from_str(&tooltip)));
    }
}
