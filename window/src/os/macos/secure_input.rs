//! Secure Keyboard Entry (TGZTerminal).
//!
//! Thin wrappers over the Carbon secure event input calls. While a process
//! holds secure input, macOS stops delivering its keystrokes to other
//! processes' event taps and global event monitors.
//!
//! The OS keeps a per-process reference count: every `enable` must be paired
//! with exactly one `disable`, or the hold leaks and other apps' global
//! hotkeys and autofill stay broken until this process exits. These functions
//! do not track that; the caller (`wezterm-gui/src/secure_input.rs`) keeps the
//! count at zero or one.
//!
//! Carbon is linked by `window/build.rs`.

extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
    fn IsSecureEventInputEnabled() -> u8;
}

/// Take one secure-input reference for this process.
pub fn enable() {
    let status = unsafe { EnableSecureEventInput() };
    if status != 0 {
        log::warn!("EnableSecureEventInput failed: OSStatus {status}");
    }
}

/// Drop one secure-input reference for this process.
pub fn disable() {
    let status = unsafe { DisableSecureEventInput() };
    if status != 0 {
        log::warn!("DisableSecureEventInput failed: OSStatus {status}");
    }
}

/// Whether secure input is on anywhere in the login session — not
/// necessarily held by this process.
pub fn is_enabled() -> bool {
    unsafe { IsSecureEventInputEnabled() != 0 }
}
