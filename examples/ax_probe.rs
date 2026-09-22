//! Throwaway probe: what does the Accessibility API say about the focused
//! element of whatever app is frontmost?
//!
//! Run it, then click into a text field in each app you dictate into. It
//! prints one line whenever the answer changes. Delete this file once the
//! focus-detection rule is chosen.

use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::NSWorkspace;
use std::ffi::c_void;

type AXUIElementRef = *const c_void;
type AXError = i32;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, seconds: f32) -> AXError;
}

fn error_name(code: AXError) -> String {
    match code {
        0 => "ok".to_owned(),
        -25200 => "failure".to_owned(),
        -25201 => "illegalArgument".to_owned(),
        -25202 => "invalidUIElement".to_owned(),
        -25204 => "cannotComplete".to_owned(),
        -25205 => "attributeUnsupported".to_owned(),
        -25211 => "apiDisabled".to_owned(),
        -25212 => "noValue".to_owned(),
        other => format!("error {other}"),
    }
}

/// Copy one attribute as an opaque CF object. The caller owns what comes back.
unsafe fn attribute(element: AXUIElementRef, name: &str) -> Result<CFTypeRef, AXError> {
    let key = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    let status = AXUIElementCopyAttributeValue(element, key.as_concrete_TypeRef(), &mut value);
    if status != 0 || value.is_null() {
        return Err(status);
    }
    Ok(value)
}

/// An attribute read as a string, for the descriptive ones like AXRole.
unsafe fn string_attribute(element: AXUIElementRef, name: &str) -> Result<String, AXError> {
    let value = attribute(element, name)?;
    let text = CFString::wrap_under_create_rule(value as CFStringRef).to_string();
    Ok(text)
}

/// Whether an attribute exists at all, which is the question for markers like
/// AXSelectedTextRange whose value we do not care about.
unsafe fn has_attribute(element: AXUIElementRef, name: &str) -> bool {
    match attribute(element, name) {
        Ok(value) => {
            CFRelease(value);
            true
        }
        Err(_) => false,
    }
}

unsafe fn settable(element: AXUIElementRef, name: &str) -> Result<bool, AXError> {
    let key = CFString::new(name);
    let mut answer: u8 = 0;
    let status = AXUIElementIsAttributeSettable(element, key.as_concrete_TypeRef(), &mut answer);
    if status != 0 {
        return Err(status);
    }
    Ok(answer != 0)
}

/// One line describing the focused element of the given process.
///
/// Times the query, because how long an application takes to answer is the
/// thing that decides what messaging timeout PrivacyFlow can afford.
unsafe fn describe(pid: i32) -> String {
    let application = AXUIElementCreateApplication(pid);
    if application.is_null() {
        return "could not create an AX element for the application".to_owned();
    }
    // Deliberately generous, so slow applications are measured rather than
    // cut off. The real timeout is chosen from what this reports.
    AXUIElementSetMessagingTimeout(application, 2.0);
    let started = std::time::Instant::now();
    let focused = match attribute(application, "AXFocusedUIElement") {
        Ok(value) => value as AXUIElementRef,
        Err(status) => {
            let waited = started.elapsed().as_millis();
            CFRelease(application as CFTypeRef);
            return format!("no focused element ({}) after {waited} ms", error_name(status));
        }
    };
    let waited = started.elapsed().as_millis();
    let role = string_attribute(focused, "AXRole").unwrap_or_else(error_name);
    let subrole = string_attribute(focused, "AXSubrole").unwrap_or_else(|_| "-".to_owned());
    let description =
        string_attribute(focused, "AXRoleDescription").unwrap_or_else(|_| "-".to_owned());
    let value_settable = match settable(focused, "AXValue") {
        Ok(true) => "yes",
        Ok(false) => "no",
        Err(_) => "?",
    };
    let selected_range = has_attribute(focused, "AXSelectedTextRange");
    let selected_settable = match settable(focused, "AXSelectedText") {
        Ok(true) => "yes",
        Ok(false) => "no",
        Err(_) => "?",
    };
    CFRelease(focused as CFTypeRef);
    CFRelease(application as CFTypeRef);
    format!(
        "{waited:>4} ms  role {role:<16} subrole {subrole:<18} AXValue settable {value_settable:<3} \
         AXSelectedTextRange {:<5} AXSelectedText settable {selected_settable:<3} ({description})",
        selected_range
    )
}

fn main() {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            println!(
                "This process is NOT trusted for Accessibility, so every answer below \
                 would be meaningless. Grant the terminal you launched this from \
                 Accessibility permission and run it again."
            );
            return;
        }
    }
    println!("Probing. Click into a text field in each app you dictate into.");
    println!("Also click somewhere with no text field, to see what that looks like.");
    println!("Ctrl-C to stop.\n");
    let mut last = String::new();
    loop {
        let line = match NSWorkspace::sharedWorkspace().frontmostApplication() {
            Some(application) => {
                let pid = application.processIdentifier();
                let name = application
                    .localizedName()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| format!("pid {pid}"));
                format!("{name:<16} {}", unsafe { describe(pid) })
            }
            None => "no frontmost application".to_owned(),
        };
        if line != last {
            println!("{line}");
            last = line;
        }
        // Serviced rather than slept through. NSWorkspace learns about
        // application switches from notifications delivered on the run loop,
        // so a probe that only sleeps answers with whatever was frontmost
        // when it started, forever.
        unsafe {
            CFRunLoop::run_in_mode(
                kCFRunLoopDefaultMode,
                std::time::Duration::from_millis(400),
                false,
            );
        }
    }
}
