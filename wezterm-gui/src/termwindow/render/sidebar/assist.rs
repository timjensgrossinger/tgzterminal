//! The Ask card: painting, keys and clicks (TGZTerminal, `assist.ask`).
//! State and the CLI run are in `termwindow::tgz_assist`.
//!
//! A card over the bottom of the pane it asks about, painted with the menus.
//! Top to bottom: what is sent ("Sees: ..."), the question field, the files
//! the model looked at, the answer, the suggested commands with their risk
//! label, and the keys that act on them.

use super::*;
use crate::assist::risk::Risk;
use crate::termwindow::tgz_assist::{AssistHit, Phase};
use crate::termwindow::tgz_ui_text::{elide_start_px, Align, UiArea, UiFont};
use ::window::{Clipboard, CursorIcon, Modifiers, MouseEvent, MouseEventKind as WMEK};

/// Keys the card handles itself, normalised from physical, raw and decoded
/// key codes alike.
enum CardKey {
    Enter,
    Escape,
    Up,
    Down,
    Tab,
    PageUp,
    PageDown,
    Paste,
}

/// Markdown is shown as plain text: fences, bold markers and heading hashes
/// go; code lines are indented.
fn plain_answer(answer: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in answer.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(format!("    {}", line));
            continue;
        }
        let text = trimmed.trim_start_matches('#').trim_start();
        let text = if trimmed.starts_with('#') { text } else { line };
        out.push(text.replace("**", ""));
    }
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    out
}

/// Greedy word wrap at `max_w` pixels; a word wider than a line is cut by
/// character.
fn wrap_px(lines: &[String], max_w: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        if measure(line) <= max_w {
            out.push(line.clone());
            continue;
        }
        let indent: String = line.chars().take_while(|c| *c == ' ').collect();
        let mut current = indent.clone();
        for word in line.split_whitespace() {
            let candidate = if current.trim().is_empty() {
                format!("{}{}", indent, word)
            } else {
                format!("{} {}", current, word)
            };
            if measure(&candidate) <= max_w {
                current = candidate;
                continue;
            }
            if !current.trim().is_empty() {
                out.push(std::mem::replace(&mut current, indent.clone()));
            }
            // The word alone may still be too wide: cut it by character.
            let mut piece = indent.clone();
            for c in word.chars() {
                piece.push(c);
                if measure(&piece) > max_w && piece.chars().count() > indent.len() + 1 {
                    piece.pop();
                    out.push(std::mem::replace(&mut piece, format!("{}{}", indent, c)));
                }
            }
            current = piece;
        }
        if !current.trim().is_empty() {
            out.push(current);
        }
    }
    out
}

impl crate::TermWindow {
    pub(crate) fn paint_assist_card(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(pane_id) = self.assist.as_ref().map(|card| card.pane_id) else {
            return Ok(());
        };
        // The pane went away, or is in another tab now: the card goes too,
        // or it would take keys for a pane nobody can see.
        let Some(pos) = self
            .get_panes_to_render()
            .into_iter()
            .find(|pos| pos.pane.pane_id() == pane_id)
        else {
            self.assist = None;
            return Ok(());
        };

        let prev_area = self.ui_area.replace(UiArea::Base);
        let result = self.paint_assist_card_at(layers, &pos);
        self.ui_area.set(prev_area);
        result
    }

    fn paint_assist_card_at(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        pos: &mux::tab::PositionedPane,
    ) -> anyhow::Result<()> {
        let sb = self.sidebar_palette();
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_w = self.render_metrics.cell_size.width as f32;
        let cell_h = self.render_metrics.cell_size.height as f32;
        let (padding_left, _) = self.padding_left_top();
        let (_, top_pixel_y) = self.pane_grid_top()?;
        let border = self.get_os_border();
        let pane_x = padding_left + border.left.get() as f32 + pos.left as f32 * cell_w;
        let pane_y = top_pixel_y + pos.top as f32 * cell_h;
        let pane_w = pos.width as f32 * cell_w;
        let pane_h = pos.height as f32 * cell_h;

        let pad = 12. * dpi_scale;
        let margin = 8. * dpi_scale;
        let line = self.ui_line_height(UiFont::Regular);
        let small = self.ui_line_height(UiFont::Small);
        let medium = self.ui_line_height(UiFont::Medium);
        let card_w = (pane_w - margin * 2.).min(820. * dpi_scale).max(200.);
        let card_x = (pane_x + (pane_w - card_w) * 0.5).round();
        let text_x = card_x + pad;
        let text_w = card_w - pad * 2.;
        let field_h = (line * 1.7).ceil();
        let row_h = (line * 1.6).ceil();

        let Some(card) = self.assist.as_ref() else {
            return Ok(());
        };

        // --- Measure ---------------------------------------------------
        let answer_lines: Vec<String> = {
            let key = (card.answer.len(), text_w as u32);
            let cached = card
                .wrap_cache
                .borrow()
                .as_ref()
                .filter(|(len, w, _)| (*len, *w) == key)
                .map(|(_, _, lines)| lines.clone());
            match cached {
                Some(lines) => lines,
                None => {
                    let lines = wrap_px(&plain_answer(&card.answer), text_w, |s| {
                        self.ui_text_width(UiFont::Regular, s)
                    });
                    card.wrap_cache.replace(Some((key.0, key.1, lines.clone())));
                    lines
                }
            }
        };
        let status_line: Option<(String, bool)> = match &card.phase {
            Phase::Running { started } if card.answer.is_empty() => Some((
                format!(
                    "{}… {:.0}s",
                    if card.thinking { "Thinking" } else { "Working" },
                    started.elapsed().as_secs_f32()
                ),
                false,
            )),
            Phase::Failed(message) => Some((message.clone(), true)),
            _ => None,
        };
        let looked_line = (!card.looked.is_empty()).then(|| {
            card.looked
                .iter()
                .map(|look| {
                    if look.ok {
                        look.label.clone()
                    } else {
                        format!("{} (refused)", look.label)
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ")
        });
        let suggestions = card.suggestions.clone();
        let selected = card.selected;
        let show_risk = self.config.assist.risk_labels;

        let max_h = (pane_h * 0.62).max(medium * 8.);
        let fixed_h = pad
            + medium
            + pad * 0.4
            + small
            + pad * 0.6
            + field_h
            + pad * 0.6
            + looked_line.as_ref().map_or(0., |_| small + pad * 0.4)
            + status_line.as_ref().map_or(0., |_| line + pad * 0.4)
            + suggestions.len() as f32 * row_h
            + if suggestions.is_empty() {
                0.
            } else {
                pad * 0.5
            }
            + small
            + pad;
        let answer_room = ((max_h - fixed_h) / line).floor().max(2.) as usize;
        let answer_shown = answer_lines.len().min(answer_room);
        // Follow the stream; once done, the user scrolls.
        let max_scroll = answer_lines.len().saturating_sub(answer_shown);
        let scroll = if card.is_running() {
            max_scroll
        } else {
            card.scroll.min(max_scroll)
        };
        let answer_h = answer_shown as f32 * line + if answer_shown > 0 { pad * 0.4 } else { 0. };
        let card_h = (fixed_h + answer_h).min(pane_h - margin);
        let card_y = (pane_y + pane_h - margin - card_h).max(pane_y).round();

        // --- Paint -----------------------------------------------------
        let fill = lerp_rgba(sb.surface, sb.text_active, 0.06);
        self.sidebar_bordered_fill(
            layers,
            0,
            euclid::rect(card_x, card_y, card_w, card_h),
            MENU_RADIUS * dpi_scale * 1.5,
            dpi_scale.max(1.),
            fill,
            sb.menu_border,
        )?;
        self.ui_items.push(UIItem {
            x: card_x as usize,
            y: card_y as usize,
            width: card_w as usize,
            height: card_h as usize,
            item_type: UIItemType::AssistCard(AssistHit::Body),
        });

        let card = self.assist.as_ref().expect("checked above");
        let mut y = card_y + pad;
        // Header: "Ask Claude", and on the right the run's state or cost.
        let title = format!("Ask {}", card.provider);
        self.paint_ui_text(
            layers,
            UiFont::Medium,
            &title,
            text_x,
            y,
            text_w,
            sb.text_active,
            Align::Left,
        )?;
        let right = match &card.phase {
            Phase::Running { .. } => "esc stop".to_string(),
            _ => card
                .meta
                .clone()
                .map(|meta| format!("{}   ×", meta))
                .unwrap_or_else(|| "×".to_string()),
        };
        let right_w = self.ui_text_width(UiFont::Small, &right) + pad * 0.5;
        let right_x = card_x + card_w - pad - right_w;
        self.paint_ui_text(
            layers,
            UiFont::Small,
            &right,
            right_x,
            y + (medium - small) * 0.5,
            right_w,
            sb.text_meta,
            Align::Right,
        )?;
        self.ui_items.push(UIItem {
            x: right_x as usize,
            y: y as usize,
            width: (right_w + pad * 0.5) as usize,
            height: medium as usize,
            item_type: UIItemType::AssistCard(AssistHit::Close),
        });
        y += medium + pad * 0.4;

        // What is sent.
        let chips = if card.chips.is_empty() {
            "Sees: nothing yet (no commands in this pane)".to_string()
        } else {
            format!("Sees: {}", card.chips.join(" · "))
        };
        self.paint_ui_text(
            layers,
            UiFont::Small,
            &chips,
            text_x,
            y,
            text_w,
            sb.text_meta,
            Align::Left,
        )?;
        y += small + pad * 0.6;

        // The question field.
        let field_fill = lerp_rgba(sb.surface, sb.text_active, 0.12);
        self.sidebar_rounded_fill(
            layers,
            0,
            euclid::rect(text_x, y, text_w, field_h),
            MENU_RADIUS * dpi_scale,
            field_fill,
        )?;
        let inner_x = text_x + pad * 0.75;
        let inner_w = text_w - pad * 1.5;
        let text_y = y + (field_h - line) * 0.5;
        let editing = !card.is_running();
        if card.input.is_empty() {
            let placeholder = match (&card.phase, card.last_failed, card.has_blocks) {
                (Phase::Running { .. }, ..) => card.question.clone(),
                (Phase::Done, ..) => "Follow up, or ↑↓ to pick a command".to_string(),
                (_, true, _) => {
                    "↵ explains why the last command failed, or type a question".to_string()
                }
                (_, false, true) => "↵ explains the last command, or type a question".to_string(),
                _ => "Ask about this pane…".to_string(),
            };
            self.paint_ui_text(
                layers,
                UiFont::Regular,
                &placeholder,
                inner_x,
                text_y,
                inner_w,
                sb.text_meta,
                Align::Left,
            )?;
            if editing {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(inner_x, text_y, dpi_scale.max(1.), line),
                    sb.text_active,
                )?;
            }
        } else {
            let shown = elide_start_px(&card.input, inner_w - 4. * dpi_scale, |s| {
                self.ui_text_width(UiFont::Regular, s)
            });
            let width = self.paint_ui_text(
                layers,
                UiFont::Regular,
                &shown,
                inner_x,
                text_y,
                inner_w,
                sb.text_active,
                Align::Left,
            )?;
            if editing {
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(inner_x + width + 1., text_y, dpi_scale.max(1.), line),
                    sb.text_active,
                )?;
            }
        }
        y += field_h + pad * 0.6;

        if let Some(looked) = &looked_line {
            let text = format!("Looked at: {}", looked);
            self.paint_ui_text(
                layers,
                UiFont::Small,
                &text,
                text_x,
                y,
                text_w,
                sb.text_meta,
                Align::Left,
            )?;
            y += small + pad * 0.4;
        }
        if let Some((status, is_error)) = &status_line {
            let color = if *is_error {
                sb.attention
            } else {
                sb.text_idle
            };
            self.paint_ui_text(
                layers,
                UiFont::Regular,
                status,
                text_x,
                y,
                text_w,
                color,
                Align::Left,
            )?;
            y += line + pad * 0.4;
        }
        for text in answer_lines.iter().skip(scroll).take(answer_shown) {
            let color = if text.starts_with("    ") {
                sb.text_active
            } else {
                sb.text_idle
            };
            self.paint_ui_text(
                layers,
                UiFont::Regular,
                text,
                text_x,
                y,
                text_w,
                color,
                Align::Left,
            )?;
            y += line;
        }
        if answer_shown > 0 {
            y += pad * 0.4;
        }

        // Suggested commands.
        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        for (idx, suggestion) in suggestions.iter().enumerate() {
            let item_type = UIItemType::AssistCard(AssistHit::Suggestion(idx));
            let lit = selected == Some(idx) || hovered.as_ref() == Some(&item_type);
            if lit {
                self.sidebar_rounded_fill(
                    layers,
                    0,
                    euclid::rect(text_x, y, text_w, row_h),
                    MENU_RADIUS * dpi_scale,
                    sb.hover_fill,
                )?;
            }
            let mut x = text_x + pad * 0.5;
            if show_risk {
                let label = suggestion.risk.label();
                let pill_w = self.ui_text_width(UiFont::Small, label) + pad;
                let pill_h = (small * 1.25).ceil();
                let pill_fill = match suggestion.risk {
                    Risk::OnlyLooks => lerp_rgba(sb.surface, sb.text_meta, 0.35),
                    Risk::Changes => lerp_rgba(sb.surface, sb.accent(), 0.55),
                    Risk::Deletes => sb.attention,
                };
                self.sidebar_pill_fill(
                    layers,
                    1,
                    euclid::rect(x, y + (row_h - pill_h) * 0.5, pill_w, pill_h),
                    pill_h * 0.5,
                    pill_fill,
                )?;
                self.paint_ui_text(
                    layers,
                    UiFont::Small,
                    label,
                    x,
                    y + (row_h - small) * 0.5,
                    pill_w,
                    contrast_label_color(pill_fill, sb.text_active),
                    Align::Center,
                )?;
                x += pill_w + pad * 0.6;
            }
            self.paint_ui_text(
                layers,
                UiFont::Regular,
                &suggestion.command,
                x,
                y + (row_h - line) * 0.5,
                text_x + text_w - x - pad * 0.5,
                sb.text_active,
                Align::Left,
            )?;
            self.ui_items.push(UIItem {
                x: text_x as usize,
                y: y as usize,
                width: text_w as usize,
                height: row_h as usize,
                item_type,
            });
            y += row_h;
        }
        if !suggestions.is_empty() {
            y += pad * 0.5;
        }

        let card = self.assist.as_ref().expect("checked above");
        let hints = match &card.phase {
            Phase::Running { .. } => "esc stop",
            Phase::Done if !card.suggestions.is_empty() => {
                if card.selected.is_some() {
                    "↵ insert at the prompt · ⌘↵ run · ↑↓ pick · esc close"
                } else {
                    "↑↓ pick a command · ↵ follow up · ⌥↵ think longer · esc close"
                }
            }
            _ => "↵ ask · ⌥↵ think longer · esc close",
        };
        self.paint_ui_text(
            layers,
            UiFont::Small,
            hints,
            text_x,
            y.min(card_y + card_h - pad - small),
            text_w,
            sb.text_meta,
            Align::Left,
        )?;

        // A running request repaints itself: the elapsed time ticks and the
        // card follows the stream.
        if card.is_running() {
            let next = std::time::Instant::now() + std::time::Duration::from_millis(500);
            self.update_next_frame_time(Some(next));
        }
        Ok(())
    }

    /// Enter (with any modifier), Esc, arrows, Tab and paste, before key
    /// bindings see them. `true` when the card took the key.
    pub(crate) fn assist_early_key(
        &mut self,
        keycode: &::window::KeyCode,
        mods: Modifiers,
    ) -> bool {
        use ::window::{KeyCode as WK, PhysKeyCode as P};
        if self.assist.is_none() || self.chrome_dialog.is_some() || self.ssh_setup.is_some() {
            return false;
        }
        let mods = mods.remove_positional_mods();
        let paste_chord = if cfg!(target_os = "macos") {
            Modifiers::SUPER
        } else {
            Modifiers::CTRL
        };
        let key = match keycode {
            WK::Char('\r')
            | WK::Char('\n')
            | WK::Physical(P::Return)
            | WK::Physical(P::KeypadEnter) => CardKey::Enter,
            WK::Char('\u{1b}') | WK::Physical(P::Escape) => CardKey::Escape,
            WK::UpArrow | WK::Physical(P::UpArrow) if mods.is_empty() => CardKey::Up,
            WK::DownArrow | WK::Physical(P::DownArrow) if mods.is_empty() => CardKey::Down,
            WK::Char('\t') | WK::Physical(P::Tab) => CardKey::Tab,
            WK::PageUp | WK::Physical(P::PageUp) => CardKey::PageUp,
            WK::PageDown | WK::Physical(P::PageDown) => CardKey::PageDown,
            WK::Char('v') | WK::Char('V') | WK::Physical(P::V) if mods == paste_chord => {
                CardKey::Paste
            }
            _ => return false,
        };
        self.assist_card_key(key, mods);
        true
    }

    fn assist_card_key(&mut self, key: CardKey, mods: Modifiers) {
        let Some(card) = self.assist.as_mut() else {
            return;
        };
        match key {
            CardKey::Escape => {
                if card.is_running() {
                    // Stop the run, keep the card: the partial answer stays.
                    let partial = !card.answer.trim().is_empty();
                    card.phase = if partial {
                        Phase::Done
                    } else {
                        Phase::Failed("Stopped.".to_string())
                    };
                    let pane_id = card.pane_id;
                    // A fresh card state drops the run handle, which stops it.
                    self.assist_stop_run(pane_id);
                } else {
                    self.close_assist();
                }
            }
            CardKey::Enter => {
                let running = card.is_running();
                let selected = card.selected.filter(|_| card.input.is_empty());
                if mods.contains(Modifiers::ALT) {
                    if !running {
                        self.assist_ask(true);
                    }
                } else if let Some(idx) = selected {
                    self.assist_use_suggestion(idx, mods.contains(Modifiers::SUPER));
                } else if !running && !mods.contains(Modifiers::SUPER) {
                    self.assist_ask(false);
                }
            }
            CardKey::Up | CardKey::Down | CardKey::Tab => {
                let n = card.suggestions.len();
                if n == 0 {
                    card.scroll = match key {
                        CardKey::Up => card.scroll.saturating_sub(1),
                        CardKey::Down => card.scroll + 1,
                        _ => card.scroll,
                    };
                } else {
                    let back = matches!(key, CardKey::Up)
                        || (matches!(key, CardKey::Tab) && mods.contains(Modifiers::SHIFT));
                    card.selected = Some(match (card.selected, back) {
                        (None, true) => n - 1,
                        (None, false) => 0,
                        (Some(i), true) => (i + n - 1) % n,
                        (Some(i), false) => (i + 1) % n,
                    });
                    card.input.clear();
                }
            }
            CardKey::PageUp => card.scroll = card.scroll.saturating_sub(8),
            CardKey::PageDown => card.scroll += 8,
            CardKey::Paste => self.assist_paste(),
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Typed text while the card is open (after key bindings ran).
    pub(crate) fn assist_key(
        &mut self,
        key: &::termwiz::input::KeyCode,
        modifiers: Modifiers,
        context: &dyn WindowOps,
    ) {
        use ::termwiz::input::KeyCode as KC;
        let mods = modifiers.remove_positional_mods();
        match key {
            KC::Escape => return self.assist_card_key(CardKey::Escape, mods),
            KC::Enter => return self.assist_card_key(CardKey::Enter, mods),
            _ => {}
        }
        let Some(card) = self.assist.as_mut() else {
            return;
        };
        if card.is_running() {
            return;
        }
        match key {
            KC::Backspace => {
                card.input.pop();
            }
            KC::Char(c)
                if !mods.intersects(Modifiers::CTRL | Modifiers::SUPER) && !c.is_control() =>
            {
                card.input.push(*c);
                card.selected = None;
            }
            _ => return,
        }
        context.invalidate();
    }

    fn assist_paste(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let future = window.get_clipboard(Clipboard::Clipboard);
        promise::spawn::spawn(async move {
            if let Ok(text) = future.await {
                window.notify(TermWindowNotif::Apply(Box::new(move |myself| {
                    if let Some(card) = myself.assist.as_mut() {
                        if !card.is_running() {
                            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                            card.input.push_str(&flat);
                            card.selected = None;
                        }
                    }
                    if let Some(window) = myself.window.as_ref() {
                        window.invalidate();
                    }
                })));
            }
        })
        .detach();
    }

    pub(crate) fn mouse_event_assist(
        &mut self,
        hit: AssistHit,
        event: &MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(match hit {
            AssistHit::Body => CursorIcon::Default,
            _ => CursorIcon::Pointer,
        }));
        if event.kind != WMEK::Release(::window::MousePress::Left) {
            context.invalidate();
            return;
        }
        match hit {
            AssistHit::Body => {}
            AssistHit::Close => self.close_assist(),
            AssistHit::Suggestion(idx) => {
                let run = event.modifiers.contains(Modifiers::SUPER);
                self.assist_use_suggestion(idx, run);
            }
        }
        context.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn markdown_is_flattened() {
        let lines = plain_answer("## Cause\n**Bold** text\n```sh\nnpm test\n```\n\n");
        assert_eq!(lines, vec!["Cause", "Bold text", "    npm test"]);
    }

    #[test]
    fn wrapping_keeps_words_and_indent() {
        let lines = vec![
            "one two three four".to_string(),
            "    code that is long".to_string(),
        ];
        let wrapped = wrap_px(&lines, 9., chars);
        assert_eq!(
            wrapped,
            vec!["one two", "three", "four", "    code", "    that", "    is", "    long"]
        );
        assert!(wrapped.iter().all(|line| chars(line) <= 9.));
    }

    #[test]
    fn a_word_wider_than_the_line_is_cut() {
        let wrapped = wrap_px(&["abcdefghij".to_string()], 4., chars);
        assert_eq!(wrapped, vec!["abcd", "efgh", "ij"]);
    }
}
