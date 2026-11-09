use crate::termwindow::{CopyFlash, CopySource, PaneCopyAction, TermWindowNotif};
use crate::TermWindow;
use config::keyassignment::{ClipboardCopyDestination, ClipboardPasteSource};
use mux::pane::Pane;
use mux::Mux;
use std::sync::Arc;
use window::{Clipboard, WindowOps};

impl TermWindow {
    pub fn copy_to_clipboard(&self, clipboard: ClipboardCopyDestination, text: String) {
        let clipboard = match clipboard {
            ClipboardCopyDestination::Clipboard => [Some(Clipboard::Clipboard), None],
            ClipboardCopyDestination::PrimarySelection => [Some(Clipboard::PrimarySelection), None],
            ClipboardCopyDestination::ClipboardAndPrimarySelection => [
                Some(Clipboard::Clipboard),
                Some(Clipboard::PrimarySelection),
            ],
        };
        for &c in &clipboard {
            if let Some(c) = c {
                self.window.as_ref().unwrap().set_clipboard(c, text.clone());
            }
        }
    }

    /// The one place a pane copy reaches the clipboard.
    ///
    /// Shared by the toolbelt copy menu and the `CopyLastCommandOutput` family
    /// of key assignments so the empty-payload guard and the toast can only be
    /// got right or wrong once.
    ///
    /// Never overwrites the clipboard with nothing: an empty copy plus a
    /// success toast is how this bug hid the first time. The message is built
    /// from the payload *before* the guard, so the empty case still reports
    /// itself rather than going silent.
    ///
    /// A successful copy also starts the Copy controls' checkmark
    /// ([`CopyFlash`]). From a button that checkmark *is* the confirmation, so
    /// the toast is kept only for what it cannot say: an empty copy, or how
    /// many secrets were redacted.
    pub fn perform_pane_copy(
        &mut self,
        pane: &Arc<dyn Pane>,
        action: &PaneCopyAction,
        source: CopySource,
    ) {
        let result = self.pane_copy_result(pane, action);
        let copied = !result.text.trim().is_empty();
        if copied {
            self.copy_to_clipboard(ClipboardCopyDestination::Clipboard, result.text);
            let pane_id = pane.pane_id();
            self.copy_flash = Some(CopyFlash {
                pane_id,
                tab_id: Mux::get()
                    .resolve_pane_id(pane_id)
                    .map(|(_domain, _window, tab_id)| tab_id),
                started: std::time::Instant::now(),
            });
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
        if !copy_toast_wanted(source, copied, result.redacted) {
            return;
        }
        wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
            title: result.title.to_string(),
            message: result.message,
            url: None,
            timeout: Some(std::time::Duration::from_millis(1800)),
        });
    }

    pub fn paste_from_clipboard(&mut self, pane: &Arc<dyn Pane>, clipboard: ClipboardPasteSource) {
        let pane_id = pane.pane_id();
        log::trace!(
            "paste_from_clipboard in pane {} {:?}",
            pane.pane_id(),
            clipboard
        );
        let window = self.window.as_ref().unwrap().clone();
        let clipboard = match clipboard {
            ClipboardPasteSource::Clipboard => Clipboard::Clipboard,
            ClipboardPasteSource::PrimarySelection => Clipboard::PrimarySelection,
        };
        let future = window.get_clipboard(clipboard);
        promise::spawn::spawn(async move {
            if let Ok(clip) = future.await {
                window.notify(TermWindowNotif::Apply(Box::new(move |myself| {
                    if let Some(pane) = myself
                        .pane_state(pane_id)
                        .overlay
                        .as_ref()
                        .map(|overlay| overlay.pane.clone())
                        .or_else(|| {
                            let mux = Mux::get();
                            mux.get_pane(pane_id)
                        })
                    {
                        pane.send_paste(&clip).ok();
                    }
                })));
            }
        })
        .detach();
        self.maybe_scroll_to_bottom_for_input(&pane);
    }
}

/// Whether a pane copy shows a toast.
///
/// A key-assignment copy always does: nothing on screen confirms it. A button
/// copy is confirmed by its checkmark, so it toasts only for an empty copy
/// (there is no checkmark then, and silence would look like success) or a
/// redaction (the count is worth knowing and the checkmark cannot say it).
fn copy_toast_wanted(source: CopySource, copied: bool, redacted: usize) -> bool {
    match source {
        CopySource::Key => true,
        CopySource::Button => !copied || redacted > 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_copies_always_toast() {
        assert!(copy_toast_wanted(CopySource::Key, true, 0));
        assert!(copy_toast_wanted(CopySource::Key, false, 0));
        assert!(copy_toast_wanted(CopySource::Key, true, 3));
    }

    #[test]
    fn button_copies_toast_only_when_the_checkmark_cannot_say_it() {
        assert!(!copy_toast_wanted(CopySource::Button, true, 0));
        assert!(copy_toast_wanted(CopySource::Button, false, 0));
        assert!(copy_toast_wanted(CopySource::Button, true, 1));
    }
}
