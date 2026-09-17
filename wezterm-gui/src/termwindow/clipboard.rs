use crate::termwindow::{PaneCopyAction, TermWindowNotif};
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
    pub fn perform_pane_copy(&mut self, pane: &Arc<dyn Pane>, action: &PaneCopyAction) {
        let result = self.pane_copy_result(pane, action);
        if !result.text.trim().is_empty() {
            self.copy_to_clipboard(ClipboardCopyDestination::Clipboard, result.text);
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
