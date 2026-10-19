//! The quick preview under the Worktree panel's rows: the start of the
//! selected file, read on a worker thread and kept in step with the
//! selection while the preview is open.

use super::*;
use crate::file_tree::preview::{human_size, read_preview, Preview};

/// Share of the body the preview takes when it opens.
const DEFAULT_SHARE: f32 = 0.45;
/// Fewest rows either side of the divider keeps.
const MIN_ROWS: f32 = 3.;
/// Lines the wheel scrolls the preview per notch.
const WHEEL_LINES: usize = 3;

pub(super) struct PreviewView {
    /// The file on show (or being read).
    rel: Option<String>,
    content: Option<Preview>,
    /// The file a read is running for.
    reading: Option<String>,
    scroll: usize,
}

impl PreviewView {
    /// The file on show.
    pub(super) fn shown(&self) -> Option<&str> {
        self.rel.as_deref()
    }

    pub(super) fn new() -> Self {
        Self {
            rel: None,
            content: None,
            reading: None,
            scroll: 0,
        }
    }
}

impl crate::TermWindow {
    pub(super) fn file_tree_toggle_preview(&mut self, tab_id: TabId) {
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            view.preview = match view.preview.take() {
                Some(_) => None,
                None => Some(PreviewView::new()),
            };
        }
    }

    /// Start reading the selected file when the preview shows another.
    fn file_tree_preview_refresh(&mut self, tab_id: TabId) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        let wanted = view
            .selected
            .as_ref()
            .filter(|rel| {
                view.rows
                    .iter()
                    .any(|row| &row.rel == *rel && row.kind == EntryKind::File)
            })
            .cloned();
        let Some(root) = view.tree.as_ref().map(|tree| tree.root.clone()) else {
            return;
        };
        let Some(preview) = view.preview.as_mut() else {
            return;
        };
        if wanted == preview.rel || (wanted.is_some() && wanted == preview.reading) {
            return;
        }
        let Some(rel) = wanted else {
            // A folder or nothing: the last file stays on show.
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        preview.reading = Some(rel.clone());
        let path = Tree::new(root).abs_path(&rel);
        std::thread::spawn(move || {
            let content = read_preview(&path);
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                if let Some(preview) = term_window
                    .file_tree
                    .views
                    .get_mut(&tab_id)
                    .and_then(|view| view.preview.as_mut())
                {
                    if preview.reading.as_deref() == Some(rel.as_str()) {
                        preview.reading = None;
                    }
                    preview.rel = Some(rel);
                    preview.content = Some(content);
                    preview.scroll = 0;
                }
                if let Some(window) = term_window.window.as_ref() {
                    window.invalidate();
                }
            })));
        });
    }

    /// Where the preview starts when it is open: `area_top..area_bottom` is
    /// what the rows and the preview share.
    pub(super) fn file_tree_preview_top(
        &mut self,
        tab_id: TabId,
        area_top: f32,
        area_bottom: f32,
        row_h: f32,
    ) -> Option<f32> {
        let open = self
            .file_tree
            .views
            .get(&tab_id)
            .is_some_and(|view| view.preview.is_some());
        if !open {
            return None;
        }
        let height = area_bottom - area_top;
        if height < row_h * MIN_ROWS * 2. {
            return None;
        }
        self.file_tree_preview_refresh(tab_id);
        let share = self.file_tree.preview_share.unwrap_or(DEFAULT_SHARE);
        let top = area_bottom - height * share;
        Some(top.clamp(area_top + row_h * MIN_ROWS, area_bottom - row_h * MIN_ROWS))
    }

    /// The preview in `rect`: a divider to drag, a header naming the file,
    /// then its lines.
    pub(super) fn paint_file_tree_preview(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        tab_id: TabId,
        rect: RectF,
        badge: Option<Badge>,
    ) -> anyhow::Result<()> {
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_w = self.render_metrics.cell_size.width as f32;
        let cell_h = self.render_metrics.cell_size.height as f32;
        let pad_x = PAD_X * dpi_scale;
        let stroke = dpi_scale.max(1.);
        let sb = self.sidebar_palette();
        let surface = opaque(sb.surface);
        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let divider_hot = matches!(hovered, Some(UIItemType::FileTreePreviewDivider))
            || matches!(
                self.dragging.as_ref().map(|(item, _)| &item.item_type),
                Some(UIItemType::FileTreePreviewDivider)
            );

        // The divider: a hairline, the accent while pointed at or dragged.
        let line_x = rect.origin.x + pad_x * 0.6;
        let line_w = (rect.size.width - pad_x * 1.2).max(0.);
        self.filled_rectangle(
            layers,
            1,
            euclid::rect(line_x, rect.origin.y, line_w, stroke),
            if divider_hot {
                sb.accent()
            } else {
                lerp_rgba(surface, sb.divider, 0.8)
            },
        )?;
        let grip = 6. * dpi_scale;
        self.ui_items.push(UIItem {
            x: line_x as usize,
            y: (rect.origin.y - grip * 0.5).max(0.) as usize,
            width: line_w as usize,
            height: grip as usize,
            item_type: UIItemType::FileTreePreviewDivider,
        });

        let (rel, content, scroll, reading) = match self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.preview.as_ref())
        {
            Some(preview) => (
                preview.rel.clone(),
                preview.content.clone(),
                preview.scroll,
                preview.reading.is_some(),
            ),
            None => return Ok(()),
        };

        // Header: the file's name, what it holds, and its changes.
        let header_h = (cell_h + 8. * dpi_scale).ceil();
        let header_y = rect.origin.y + stroke + 2. * dpi_scale;
        let text_y = header_y + (header_h - cell_h) * 0.5;
        let mut right = rect.max_x() - pad_x * 0.6;
        if badge.is_some() && rel.is_some() {
            let label = "Changes";
            let label_w = label.len() as f32 * cell_w;
            let button_w = label_w + 12. * dpi_scale;
            let button_h = (cell_h + 4. * dpi_scale).min(header_h);
            let button_x = right - button_w;
            let button_y = header_y + (header_h - button_h) * 0.5;
            let item_type = UIItemType::FileTreeButton(FileTreeAction::ShowChanges);
            let bg = if hovered.as_ref() == Some(&item_type) {
                opaque(sb.pressed_fill)
            } else {
                opaque(lerp_rgba(surface, sb.text_meta, 0.12))
            };
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(button_x, button_y, button_w, button_h),
                5. * dpi_scale,
                bg,
            )?;
            self.diff_panel_text(
                layers,
                label,
                button_x + (button_w - label_w) * 0.5,
                button_y + (button_h - cell_h) * 0.5,
                label_w,
                contrast_label_color(bg, sb.text_active),
                bg,
                false,
            )?;
            self.ui_items.push(UIItem {
                x: button_x as usize,
                y: button_y as usize,
                width: button_w as usize,
                height: button_h as usize,
                item_type,
            });
            right = button_x - 6. * dpi_scale;
        }
        let summary = match &content {
            Some(Preview::Text { lines, truncated }) => {
                format!("{}{} lines", lines.len(), if *truncated { "+" } else { "" })
            }
            Some(Preview::Binary { bytes }) => human_size(*bytes),
            _ => String::new(),
        };
        let summary_w = summary.chars().count() as f32 * cell_w;
        if summary_w > 0. && right - summary_w > rect.origin.x + pad_x + 8. * cell_w {
            self.diff_panel_text(
                layers,
                &summary,
                right - summary_w,
                text_y,
                summary_w,
                sb.text_meta,
                surface,
                false,
            )?;
            right -= summary_w + cell_w;
        }
        let name = rel
            .as_deref()
            .map(|rel| rel.rsplit('/').next().unwrap_or(rel).to_string())
            .unwrap_or_else(|| "Preview".to_string());
        let name_x = rect.origin.x + pad_x;
        self.diff_panel_text(
            layers,
            &name,
            name_x,
            text_y,
            (right - name_x).max(0.),
            sb.text_active,
            surface,
            true,
        )?;

        // Body: line numbers, then the lines.
        let body_top = header_y + header_h + 2. * dpi_scale;
        let line_h = cell_h;
        let rows = ((rect.max_y() - body_top) / line_h).floor().max(0.) as usize;
        self.file_tree.preview_rows.set(rows);
        self.ui_items.push(UIItem {
            x: rect.origin.x as usize,
            y: body_top as usize,
            width: rect.size.width as usize,
            height: (rect.max_y() - body_top).max(0.) as usize,
            item_type: UIItemType::FileTreePreview,
        });
        let message = match &content {
            None if reading => Some("Reading\u{2026}".to_string()),
            None => Some("Select a file to preview it".to_string()),
            Some(Preview::Binary { .. }) => Some("Binary file".to_string()),
            Some(Preview::Unreadable(reason)) => Some(reason.clone()),
            Some(Preview::Text { lines, .. }) if lines.is_empty() => Some("Empty file".to_string()),
            Some(Preview::Text { .. }) => None,
        };
        if let Some(message) = message {
            self.diff_panel_text(
                layers,
                &message,
                rect.origin.x + pad_x,
                body_top,
                (rect.size.width - pad_x * 2.).max(0.),
                sb.text_meta,
                surface,
                false,
            )?;
            return Ok(());
        }
        let Some(Preview::Text { lines, .. }) = content else {
            return Ok(());
        };
        let digits = lines.len().max(1).to_string().len();
        let gutter_w = (digits + 1) as f32 * cell_w;
        let text_x = rect.origin.x + pad_x + gutter_w;
        let text_w = (rect.max_x() - pad_x * 0.6 - text_x).max(0.);
        let scroll = scroll.min(lines.len().saturating_sub(rows));
        for (slot, (number, line)) in lines.iter().enumerate().skip(scroll).take(rows).enumerate() {
            let y = body_top + slot as f32 * line_h;
            let label = format!("{:>digits$}", number + 1);
            self.diff_panel_text(
                layers,
                &label,
                rect.origin.x + pad_x,
                y,
                digits as f32 * cell_w,
                lerp_rgba(surface, sb.text_meta, 0.6),
                surface,
                false,
            )?;
            if !line.is_empty() {
                self.diff_panel_text(
                    layers,
                    line,
                    text_x,
                    y,
                    text_w,
                    sb.text_idle,
                    surface,
                    false,
                )?;
            }
        }
        Ok(())
    }

    /// The wheel over the preview scrolls its lines.
    pub fn mouse_event_file_tree_preview(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        context.set_cursor(Some(CursorIcon::Default));
        if event.kind == WMEK::Press(MousePress::Right) {
            return;
        }
        let WMEK::VertWheel(amount) = event.kind else {
            return self.mouse_event_file_tree(None, event, context);
        };
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        let rows = self.file_tree.preview_rows.get();
        if let Some(preview) = self
            .file_tree
            .views
            .get_mut(&tab_id)
            .and_then(|view| view.preview.as_mut())
        {
            let total = match &preview.content {
                Some(Preview::Text { lines, .. }) => lines.len(),
                _ => 0,
            };
            let max = total.saturating_sub(rows);
            let step = (amount.unsigned_abs() as usize).max(1) * WHEEL_LINES;
            let next = if amount > 0 {
                preview.scroll.saturating_sub(step)
            } else {
                (preview.scroll + step).min(max)
            };
            if next != preview.scroll {
                preview.scroll = next;
                context.invalidate();
            }
        }
    }

    pub fn mouse_event_file_tree_preview_divider(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::NsResize));
        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    pub fn drag_file_tree_preview_divider(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::NsResize));
        self.dragging.replace((item, start_event));
        let Some(painted) = self.file_tree.painted.get() else {
            return;
        };
        let (top, bottom) = painted.area();
        let height = bottom - top;
        if height <= 0. {
            return;
        }
        let share = ((bottom - event.coords.y as f32) / height).clamp(0.1, 0.9);
        if self.file_tree.preview_share != Some(share) {
            self.file_tree.preview_share = Some(share);
            context.invalidate();
        }
    }
}
