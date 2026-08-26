mod click;
mod dbus;
mod macos;
mod windows;

#[derive(Debug, Clone)]
pub struct ToastNotification {
    pub title: String,
    pub message: String,
    pub url: Option<String>,
    pub timeout: Option<std::time::Duration>,
}

/// Run when the user activates (clicks) a notification; never on dismiss.
///
/// Called on whichever thread the platform delivers the click on — the AppKit
/// main thread on macOS, a helper thread elsewhere — so a handler must do its
/// own thread hop and nothing else. This is how a notification carries a target
/// without this crate knowing anything about panes, windows or the mux.
pub type ToastClick = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

impl ToastNotification {
    pub fn show(self) {
        show(self)
    }
}

#[cfg(windows)]
use crate::windows as backend;
#[cfg(all(not(target_os = "macos"), not(windows)))]
use dbus as backend;
#[cfg(target_os = "macos")]
use macos as backend;

mod nop {
    use super::*;

    #[allow(dead_code)]
    pub fn show_notif(
        _: ToastNotification,
        _: Option<ToastClick>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

pub fn show(notif: ToastNotification) {
    show_impl(notif, None)
}

/// Show a notification that does something when clicked.
///
/// Deliberately a second entry point rather than a field on `ToastNotification`:
/// the struct is built as a literal at ~30 sites, nearly all of them upstream
/// code with nothing to focus, and a `dyn Fn` field would also cost the
/// `#[derive(Debug)]`.
pub fn show_with_click(notif: ToastNotification, on_click: ToastClick) {
    show_impl(notif, Some(on_click))
}

fn show_impl(notif: ToastNotification, on_click: Option<ToastClick>) {
    if let Err(err) = backend::show_notif(notif, on_click) {
        log::error!("Failed to show notification: {}", err);
    }
}

pub fn persistent_toast_notification_with_click_to_open_url(title: &str, message: &str, url: &str) {
    show(ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: Some(url.to_string()),
        timeout: None,
    });
}

pub fn persistent_toast_notification(title: &str, message: &str) {
    show(ToastNotification {
        title: title.to_string(),
        message: message.to_string(),
        url: None,
        timeout: None,
    });
}

/// Like `persistent_toast_notification`, but clicking it runs `on_click`.
pub fn persistent_toast_notification_with_click(title: &str, message: &str, on_click: ToastClick) {
    show_with_click(
        ToastNotification {
            title: title.to_string(),
            message: message.to_string(),
            url: None,
            timeout: None,
        },
        on_click,
    );
}

#[cfg(target_os = "macos")]
pub use macos::initialize as macos_initialize;
