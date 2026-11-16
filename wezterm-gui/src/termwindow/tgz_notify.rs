//! TGZTerminal's own system notifications, by kind, under `tgz_notifications`.
//!
//! The master switch (`tgz_notifications.enabled`) lives in the toast crate
//! itself (`wezterm_toast_notification::set_enabled`), so it also silences
//! notifications this module never sees: programs' OSC 9/777 alerts, update
//! notices, upstream error toasts. [`sync`] keeps it in step with config.
//! The kinds below only choose *within* that switch.

use wezterm_toast_notification::ToastNotification;

/// Which setting a notification answers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotifyKind {
    /// "Copied …" confirmations (`tgz_notifications.copy`).
    Copy,
    /// Feedback and errors from the fork's own actions
    /// (`tgz_notifications.status`).
    Status,
}

/// Whether a notification of `kind` may show under `config`. Pure, so the
/// rules are testable without a notification centre.
pub(crate) fn allowed(config: &config::TgzNotificationsConfig, kind: NotifyKind) -> bool {
    config.enabled
        && match kind {
            NotifyKind::Copy => config.copy,
            NotifyKind::Status => config.status,
        }
}

/// Show `notif` if its kind is on.
pub(crate) fn show(kind: NotifyKind, notif: ToastNotification) {
    if allowed(&config::configuration().tgz_notifications, kind) {
        wezterm_toast_notification::show(notif);
    }
}

/// [`show`] for a status or error message.
pub(crate) fn status(notif: ToastNotification) {
    show(NotifyKind::Status, notif);
}

/// [`show`] for a copy confirmation.
pub(crate) fn copy(notif: ToastNotification) {
    show(NotifyKind::Copy, notif);
}

/// Carry `tgz_notifications.enabled` into the toast crate's master switch.
/// Called at startup and on every config reload.
pub(crate) fn sync(config: &config::ConfigHandle) {
    wezterm_toast_notification::set_enabled(config.tgz_notifications.enabled);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_master_switch_silences_every_kind() {
        let mut config = config::TgzNotificationsConfig::default();
        assert!(allowed(&config, NotifyKind::Copy));
        assert!(allowed(&config, NotifyKind::Status));

        config.copy = false;
        assert!(!allowed(&config, NotifyKind::Copy));
        assert!(allowed(&config, NotifyKind::Status));

        config.copy = true;
        config.enabled = false;
        assert!(!allowed(&config, NotifyKind::Copy));
        assert!(!allowed(&config, NotifyKind::Status));
    }
}
