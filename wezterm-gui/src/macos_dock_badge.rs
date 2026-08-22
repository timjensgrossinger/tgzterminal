#![allow(clippy::missing_safety_doc)]

//! macOS dock badge for the agent waiting-queue count.
//!
//! Only does anything on macOS; on every other platform the function is a
//! no-op so callers can invoke it unconditionally.

#[cfg(target_os = "macos")]
pub fn set_waiting_count(count: usize) {
    use cocoa::appkit::NSApplication;
    use cocoa::base::{id, nil};
    use cocoa::foundation::NSString;
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        let app: id = NSApplication::sharedApplication(nil);
        let dock: id = msg_send![app, dockTile];
        let label: id = if count == 0 {
            nil
        } else {
            NSString::alloc(nil).init_str(&count.to_string())
        };
        let _: () = msg_send![dock, setBadgeLabel: label];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_waiting_count(_count: usize) {}
