//! Secure Keyboard Entry (TGZTerminal, macOS).
//!
//! While this process holds secure event input, macOS stops delivering its
//! keystrokes to other processes' event taps and global event monitors
//! (keyloggers, input-monitoring tools, text expanders). There is one hold per
//! process, so the state here is process-wide: at most one window "owns" it,
//! and it is held only while that window is the key window.
//!
//! A hold that leaks keeps other apps' global hotkeys and password-manager
//! autofill broken until this process exits, so every transition goes through
//! [`Holder`], which keeps the OS reference count at exactly zero or one.
//!
//! Off macOS every entry point is inert.

use mux::window::WindowId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

/// Whether a window should hold secure input right now.
///
/// `pinned` is the manual toggle (app menu, sidebar lock, the
/// `ToggleSecureKeyboardEntry` action); `auto` is
/// `macos_secure_keyboard_entry_auto`; `password_input` is the active pane's
/// "local echo off, canonical input on" reading. Never without focus: secure
/// input held by a background window would block every other app for nothing.
pub fn wanted(focused: bool, pinned: bool, auto: bool, password_input: bool) -> bool {
    focused && (pinned || (auto && password_input))
}

/// The two OS calls, injectable so the bookkeeping is testable.
struct Backend {
    enable: fn(),
    disable: fn(),
}

/// Which window holds secure input, if any.
#[derive(Debug, Default)]
struct Holder {
    owner: Option<WindowId>,
}

impl Holder {
    /// Apply one window's wish. Returns whether the held state changed.
    ///
    /// Only the owner can release: macOS normally resigns the old key window
    /// before the new one becomes key, but a stale "no" from a window that
    /// does not hold it must never drop the hold another window took.
    fn request(&mut self, window: WindowId, wants: bool, backend: &Backend) -> bool {
        match (self.owner, wants) {
            (None, true) => {
                (backend.enable)();
                self.owner = Some(window);
                true
            }
            (Some(_), true) => {
                self.owner = Some(window);
                false
            }
            (Some(owner), false) if owner == window => {
                (backend.disable)();
                self.owner = None;
                true
            }
            _ => false,
        }
    }

    /// Drop the hold whoever owns it. Returns whether anything was held.
    fn release_all(&mut self, backend: &Backend) -> bool {
        if self.owner.take().is_some() {
            (backend.disable)();
            true
        } else {
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn os_enable() {
    log::info!("secure keyboard entry: enabled");
    window::os::macos::secure_input::enable();
}

#[cfg(target_os = "macos")]
fn os_disable() {
    window::os::macos::secure_input::disable();
    log::info!("secure keyboard entry: disabled");
}

#[cfg(not(target_os = "macos"))]
fn os_enable() {}

#[cfg(not(target_os = "macos"))]
fn os_disable() {}

const OS_BACKEND: Backend = Backend {
    enable: os_enable,
    disable: os_disable,
};

static HOLDER: Mutex<Holder> = Mutex::new(Holder { owner: None });

static PINNED: LazyLock<AtomicBool> = LazyLock::new(|| {
    AtomicBool::new(crate::termwindow::tgz_ui_state::load_secure_keyboard_entry().unwrap_or(false))
});

fn holder() -> std::sync::MutexGuard<'static, Holder> {
    HOLDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether this platform has Secure Keyboard Entry at all.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// Ask for (or give up) the hold on behalf of `window`. Returns whether the
/// held state changed, so the caller knows to repaint its lock indicator.
pub fn request(window: WindowId, wants: bool) -> bool {
    if !SUPPORTED {
        return false;
    }
    holder().request(window, wants, &OS_BACKEND)
}

/// Drop the hold on the way out of the process.
pub fn release_all() {
    holder().release_all(&OS_BACKEND);
}

/// Whether this process holds secure input right now.
pub fn is_held() -> bool {
    holder().owner.is_some()
}

/// Whether the user pinned Secure Keyboard Entry on.
pub fn pinned() -> bool {
    SUPPORTED && PINNED.load(Ordering::Relaxed)
}

/// Flip the manual pin, persist it, and move the app-menu checkmark.
/// Returns the new value. The caller re-evaluates its own hold.
pub fn toggle_pinned() -> bool {
    if !SUPPORTED {
        return false;
    }
    let value = !PINNED.load(Ordering::Relaxed);
    PINNED.store(value, Ordering::Relaxed);
    crate::termwindow::tgz_ui_state::save_secure_keyboard_entry(value);
    crate::commands::CommandDef::refresh_secure_input_menu_state();
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn needs_focus_whatever_else_is_true() {
        assert!(!wanted(false, true, true, true));
        assert!(wanted(true, true, false, false));
        assert!(wanted(true, false, true, true));
        assert!(!wanted(true, false, false, true));
        assert!(!wanted(true, false, true, false));
    }

    // Each test gets its own counters through its own pair of fns.
    macro_rules! counting_backend {
        ($enables:ident, $disables:ident) => {{
            static $enables: AtomicUsize = AtomicUsize::new(0);
            static $disables: AtomicUsize = AtomicUsize::new(0);
            fn enable() {
                $enables.fetch_add(1, Ordering::SeqCst);
            }
            fn disable() {
                $disables.fetch_add(1, Ordering::SeqCst);
            }
            (Backend { enable, disable }, &$enables, &$disables)
        }};
    }

    #[test]
    fn repeated_wishes_keep_the_os_count_at_one() {
        let (backend, enables, disables) = counting_backend!(E1, D1);
        let mut holder = Holder::default();
        assert!(holder.request(1, true, &backend));
        assert!(!holder.request(1, true, &backend));
        assert!(holder.request(1, false, &backend));
        assert!(!holder.request(1, false, &backend));
        assert_eq!(enables.load(Ordering::SeqCst), 1);
        assert_eq!(disables.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_window_that_does_not_hold_it_cannot_release_it() {
        let (backend, enables, disables) = counting_backend!(E2, D2);
        let mut holder = Holder::default();
        holder.request(1, true, &backend);
        // Window 2 becomes key and is not at a prompt before 1 resigns.
        assert!(!holder.request(2, false, &backend));
        assert_eq!(holder.owner, Some(1));
        assert!(holder.request(1, false, &backend));
        assert_eq!(enables.load(Ordering::SeqCst), 1);
        assert_eq!(disables.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ownership_moves_without_a_second_enable() {
        let (backend, enables, disables) = counting_backend!(E3, D3);
        let mut holder = Holder::default();
        holder.request(1, true, &backend);
        // Window 2 takes over before 1's resign arrives.
        assert!(!holder.request(2, true, &backend));
        assert!(!holder.request(1, false, &backend));
        assert_eq!(holder.owner, Some(2));
        assert!(holder.request(2, false, &backend));
        assert_eq!(enables.load(Ordering::SeqCst), 1);
        assert_eq!(disables.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn release_all_drops_a_hold_once() {
        let (backend, _enables, disables) = counting_backend!(E4, D4);
        let mut holder = Holder::default();
        assert!(!holder.release_all(&backend));
        holder.request(3, true, &backend);
        assert!(holder.release_all(&backend));
        assert!(!holder.release_all(&backend));
        assert_eq!(disables.load(Ordering::SeqCst), 1);
    }
}
