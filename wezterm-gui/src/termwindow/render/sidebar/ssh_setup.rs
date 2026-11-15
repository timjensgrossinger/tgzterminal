//! The "New SSH connection" form: painting, keys, clicks and saving. The
//! model and the file writing are in `termwindow::tgz_ssh_setup`.
//!
//! Drawn like the close dialog (a card over a dimmed backdrop, painted with
//! the menus) and, like it, the form takes the whole keyboard while open.

use super::*;
use crate::termwindow::tgz_ssh_setup::{
    self, transport_label, transport_program, Auth, Field, SshSetupForm, TRANSPORTS,
};
use crate::termwindow::tgz_ui_text::{elide_start_px, Align, UiFont};
use ::window::{Clipboard, CursorIcon, Modifiers, MouseEvent, MouseEventKind as WMEK};

impl crate::TermWindow {
    /// Opens the form, closing whatever menu was open.
    pub(crate) fn open_ssh_setup(&mut self) {
        self.close_floating_menus();
        self.tab_palette = None;
        self.ssh_setup = Some(SshSetupForm::default());
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    fn close_ssh_setup(&mut self) {
        self.ssh_setup = None;
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Keys while the form is open. Enter saves, Esc closes, Tab and the
    /// arrows move between fields and options.
    pub(crate) fn ssh_setup_key(
        &mut self,
        key: &::termwiz::input::KeyCode,
        modifiers: ::window::Modifiers,
    ) {
        use ::termwiz::input::KeyCode as KC;
        let paste_chord = if cfg!(target_os = "macos") {
            Modifiers::SUPER
        } else {
            Modifiers::CTRL
        };
        match key {
            KC::Escape => return self.close_ssh_setup(),
            KC::Enter => return self.save_ssh_setup(),
            KC::Char('v') | KC::Char('V') if modifiers.intersects(paste_chord) => {
                return self.paste_into_ssh_setup();
            }
            _ => {}
        }
        let Some(form) = self.ssh_setup.as_mut() else {
            return;
        };
        match key {
            KC::Tab => form.move_focus(if modifiers.contains(Modifiers::SHIFT) {
                -1
            } else {
                1
            }),
            KC::UpArrow => form.move_focus(-1),
            KC::DownArrow => form.move_focus(1),
            KC::LeftArrow => form.step_choice(-1),
            KC::RightArrow => form.step_choice(1),
            KC::Backspace => form.backspace(),
            KC::Char(' ') if !form.focus.is_text() => form.step_choice(1),
            KC::Char(c)
                if !modifiers.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER) =>
            {
                form.type_char(*c)
            }
            _ => return,
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    fn paste_into_ssh_setup(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let future = window.get_clipboard(Clipboard::Clipboard);
        promise::spawn::spawn(async move {
            if let Ok(text) = future.await {
                window.notify(TermWindowNotif::Apply(Box::new(move |myself| {
                    if let Some(form) = myself.ssh_setup.as_mut() {
                        form.paste(&text);
                    }
                    if let Some(window) = myself.window.as_ref() {
                        window.invalidate();
                    }
                })));
            }
        })
        .detach();
    }

    /// Checks the form, adds the host to the ssh config, remembers how to
    /// connect and connects in a new tab. A refusal stays in the form.
    fn save_ssh_setup(&mut self) {
        let Some(form) = self.ssh_setup.as_mut() else {
            return;
        };
        let new = match tgz_ssh_setup::validate(form) {
            Ok(new) => new,
            Err(err) => {
                form.error = Some(err);
                return self.invalidate_ssh_setup();
            }
        };
        let Some(argv) =
            tgz_ssh_setup::connect_argv(&new.name, new.transport, &resolve_command_path)
        else {
            form.error = Some(format!(
                "{} is not installed here (not on PATH). Pick another way to connect.",
                transport_program(new.transport)
            ));
            return self.invalidate_ssh_setup();
        };
        let home = &*config::HOME_DIR;
        if let Some(key) = &new.key_file {
            let path = tgz_ssh_setup::expand_home(key, home);
            if !path.is_file() {
                form.error = Some(format!("There is no key file at {}.", path.display()));
                return self.invalidate_ssh_setup();
            }
        }
        let path = tgz_ssh_setup::ssh_config_path(home);
        if let Err(err) = tgz_ssh_setup::append_host_block(&path, &new, crate::brand::PRODUCT_NAME)
        {
            form.error = Some(err);
            return self.invalidate_ssh_setup();
        }
        crate::termwindow::tgz_ui_state::save_ssh_host_transport(&new.name, new.transport);
        self.ssh_host_transports
            .insert(new.name.clone(), new.transport);
        let domain_name = format!("SSH:{}", new.name);
        self.ssh_recent_hosts =
            crate::termwindow::tgz_ssh_hosts::record_recent(&self.ssh_recent_hosts, &domain_name);
        crate::termwindow::tgz_ui_state::save_ssh_recent_hosts(&self.ssh_recent_hosts);
        *self.ssh_launcher_cache.borrow_mut() = None;
        crate::termwindow::tgz_ssh_hosts::refresh_ssh_hosts_now();
        self.ssh_setup = None;
        // Straight away, by its name: the host list catches up on its own.
        self.spawn_command(
            &SpawnCommand {
                label: Some(new.name.clone()),
                args: Some(argv),
                domain: SpawnTabDomain::DomainName("local".to_string()),
                ..Default::default()
            },
            SpawnWhere::NewTab,
        );
        self.invalidate_ssh_setup();
    }

    fn invalidate_ssh_setup(&self) {
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Clicks on the form's fields, options and buttons.
    pub(crate) fn mouse_event_ssh_setup(
        &mut self,
        item_type: &UIItemType,
        event: &MouseEvent,
        context: &dyn WindowOps,
    ) {
        let text_cursor = matches!(item_type, UIItemType::SshSetupField(_));
        context.set_cursor(Some(if text_cursor {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }));
        if event.kind != WMEK::Release(MousePress::Left) {
            context.invalidate();
            return;
        }
        self.pressed_ui_item = None;
        match item_type {
            UIItemType::SshSetupField(field) => {
                if let Some(form) = self.ssh_setup.as_mut() {
                    form.focus = *field;
                }
            }
            UIItemType::SshSetupOption { field, index } => {
                if let Some(form) = self.ssh_setup.as_mut() {
                    form.focus = *field;
                    match field {
                        Field::Auth => {
                            if let Some(auth) = Auth::ALL.get(*index) {
                                form.auth = *auth;
                            }
                        }
                        Field::Transport => {
                            if let Some(transport) = TRANSPORTS.get(*index) {
                                form.transport = *transport;
                            }
                        }
                        _ => {}
                    }
                    form.error = None;
                }
            }
            UIItemType::SshSetupButton { save: true } => self.save_ssh_setup(),
            UIItemType::SshSetupButton { save: false } => self.close_ssh_setup(),
            _ => {}
        }
        context.invalidate();
    }

    pub(crate) fn paint_ssh_setup(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(form) = self.ssh_setup.clone() else {
            return Ok(());
        };
        self.modal_backdrop(layers)?;

        let sb = self.sidebar_palette();
        let accent = sb.accent();
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let window_w = self.dimensions.pixel_width as f32;
        let window_h = self.dimensions.pixel_height as f32;
        let pad = (16. * dpi_scale).round();
        let gap = (8. * dpi_scale).round();
        let line = self.ui_line_height(UiFont::Regular);
        let small = self.ui_line_height(UiFont::Small);
        let title_h = self.ui_line_height(UiFont::Medium);
        let field_h = (line * 1.9).ceil();
        let button_h = (line * 2.2).ceil();
        let radius = MENU_RADIUS * dpi_scale;
        let fields = form.fields();

        let program_missing = resolve_command_path(transport_program(form.transport)).is_none();
        let note = match form.auth {
            Auth::Password => "Passwords are never saved: ssh asks for one when it connects.",
            Auth::Key => "Only where the key is gets saved, never the key itself.",
        };
        let warning = form.error.clone().or_else(|| {
            program_missing.then(|| {
                format!(
                    "{} is not installed here (not on PATH).",
                    transport_program(form.transport)
                )
            })
        });

        let box_w = (520. * dpi_scale).min(window_w - 32. * dpi_scale).max(200.);
        let body_h = fields.len() as f32 * (field_h + gap) - gap;
        let box_h = pad
            + title_h
            + gap
            + small
            + pad
            + body_h
            + pad
            + small
            + if warning.is_some() { gap + small } else { 0. }
            + pad
            + button_h
            + pad;
        let box_x = ((window_w - box_w) * 0.5).round();
        let box_y = ((window_h - box_h) * 0.38).max(pad).round();
        let fill = lerp_rgba(sb.surface, sb.text_active, 0.07);
        self.sidebar_bordered_fill(
            layers,
            0,
            euclid::rect(box_x, box_y, box_w, box_h),
            radius * 1.5,
            dpi_scale.max(1.),
            fill,
            sb.menu_border,
        )?;
        let text_x = box_x + pad;
        let text_w = box_w - pad * 2.;
        let mut y = box_y + pad;
        self.paint_ui_text(
            layers,
            UiFont::Medium,
            "New SSH connection",
            text_x,
            y,
            text_w,
            sb.text_active,
            Align::Left,
        )?;
        y += title_h + gap;
        self.paint_ui_text(
            layers,
            UiFont::Small,
            "Saved to ~/.ssh/config, so ssh, mosh and et all know it by its name.",
            text_x,
            y,
            text_w,
            sb.text_meta,
            Align::Left,
        )?;
        y += small + pad;

        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let label_w = fields
            .iter()
            .map(|field| self.ui_text_width(UiFont::Regular, field.label()))
            .fold(0., f32::max)
            .ceil()
            + pad;
        let control_x = text_x + label_w;
        let control_w = (text_w - label_w).max(40.);
        let field_fill = sb.search_fill;
        let focused_fill = sb.focused_search_fill;
        let text_pad = (10. * dpi_scale).round();

        for field in &fields {
            let focused = form.focus == *field;
            self.paint_ui_text(
                layers,
                UiFont::Regular,
                field.label(),
                text_x,
                y + ((field_h - line) * 0.5).round(),
                label_w - gap,
                if focused {
                    sb.text_active
                } else {
                    sb.text_idle
                },
                Align::Left,
            )?;
            if field.is_text() {
                let rect = euclid::rect(control_x, y, control_w, field_h);
                if focused {
                    self.sidebar_bordered_fill(
                        layers,
                        0,
                        rect,
                        radius,
                        dpi_scale.max(1.),
                        focused_fill,
                        accent,
                    )?;
                } else {
                    self.sidebar_rounded_fill(layers, 0, rect, radius, field_fill)?;
                }
                let value = form.text(*field);
                let room = control_w - text_pad * 2.;
                let (shown, color) = if value.is_empty() && !focused {
                    (field.hint().to_string(), sb.text_meta)
                } else {
                    let caret = if focused { "|" } else { "" };
                    let measure = |text: &str| self.ui_text_width(UiFont::Regular, text);
                    let caret_w = measure(caret);
                    (
                        format!("{}{caret}", elide_start_px(value, room - caret_w, measure)),
                        sb.text_active,
                    )
                };
                self.paint_ui_text(
                    layers,
                    UiFont::Regular,
                    &shown,
                    control_x + text_pad,
                    y + ((field_h - line) * 0.5).round(),
                    room,
                    color,
                    Align::Left,
                )?;
                self.ui_items.push(UIItem {
                    x: control_x as usize,
                    y: y as usize,
                    width: control_w as usize,
                    height: field_h as usize,
                    item_type: UIItemType::SshSetupField(*field),
                });
            } else {
                let options: Vec<(&str, bool)> = match field {
                    Field::Auth => Auth::ALL
                        .iter()
                        .map(|auth| (auth.label(), *auth == form.auth))
                        .collect(),
                    _ => TRANSPORTS
                        .iter()
                        .map(|t| (transport_label(*t), *t == form.transport))
                        .collect(),
                };
                let segment_w = (control_w / options.len() as f32).floor();
                if focused {
                    self.sidebar_bordered_fill(
                        layers,
                        0,
                        euclid::rect(control_x, y, control_w, field_h),
                        radius,
                        dpi_scale.max(1.),
                        field_fill,
                        accent,
                    )?;
                } else {
                    self.sidebar_rounded_fill(
                        layers,
                        0,
                        euclid::rect(control_x, y, control_w, field_h),
                        radius,
                        field_fill,
                    )?;
                }
                let inset = (3. * dpi_scale).round();
                for (index, (label, selected)) in options.iter().enumerate() {
                    let x = control_x + segment_w * index as f32;
                    let item_type = UIItemType::SshSetupOption {
                        field: *field,
                        index,
                    };
                    let lit = hovered.as_ref() == Some(&item_type);
                    let segment = euclid::rect(
                        x + inset,
                        y + inset,
                        segment_w - inset * 2.,
                        field_h - inset * 2.,
                    );
                    let fill = if *selected {
                        Some(accent)
                    } else if lit {
                        Some(sb.hover_fill)
                    } else {
                        None
                    };
                    if let Some(fill) = fill {
                        self.sidebar_rounded_fill(
                            layers,
                            1,
                            segment,
                            (radius - inset).max(2.),
                            fill,
                        )?;
                    }
                    let label_color = if *selected {
                        contrast_label_color(accent, sb.text_active)
                    } else {
                        sb.text_idle
                    };
                    self.paint_ui_text(
                        layers,
                        if *selected {
                            UiFont::Medium
                        } else {
                            UiFont::Regular
                        },
                        label,
                        x,
                        y + ((field_h - line) * 0.5).round(),
                        segment_w,
                        label_color,
                        Align::Center,
                    )?;
                    self.ui_items.push(UIItem {
                        x: x as usize,
                        y: y as usize,
                        width: segment_w as usize,
                        height: field_h as usize,
                        item_type,
                    });
                }
            }
            y += field_h + gap;
        }
        y += pad - gap;

        self.paint_ui_text(
            layers,
            UiFont::Small,
            note,
            text_x,
            y,
            text_w,
            sb.text_meta,
            Align::Left,
        )?;
        y += small;
        if let Some(warning) = &warning {
            y += gap;
            self.paint_ui_text(
                layers,
                UiFont::Small,
                warning,
                text_x,
                y,
                text_w,
                sb.attention,
                Align::Left,
            )?;
            y += small;
        }
        y += pad;

        // Buttons, right-aligned: the safe one first, the named action last.
        let button_pad = 16. * dpi_scale;
        let save_label = "Save & connect";
        let cancel_label = "Cancel";
        let save_w = self.ui_text_width(UiFont::Medium, save_label) + button_pad * 2.;
        let cancel_w = self.ui_text_width(UiFont::Medium, cancel_label) + button_pad * 2.;
        let save_x = box_x + box_w - pad - save_w;
        let cancel_x = save_x - gap - cancel_w;
        let medium = self.ui_line_height(UiFont::Medium);
        for (label, x, w, save) in [
            (cancel_label, cancel_x, cancel_w, false),
            (save_label, save_x, save_w, true),
        ] {
            let item_type = UIItemType::SshSetupButton { save };
            let lit = hovered.as_ref() == Some(&item_type);
            let base = if save {
                accent
            } else {
                lerp_rgba(sb.surface, sb.text_active, 0.16)
            };
            let button_fill = if lit {
                lerp_rgba(base, sb.text_active, 0.16)
            } else {
                base
            };
            self.sidebar_rounded_fill(
                layers,
                0,
                euclid::rect(x, y, w, button_h),
                radius,
                button_fill,
            )?;
            self.paint_ui_text(
                layers,
                UiFont::Medium,
                label,
                x,
                y + ((button_h - medium) * 0.5).round(),
                w,
                contrast_label_color(button_fill, sb.text_active),
                Align::Center,
            )?;
            self.ui_items.push(UIItem {
                x: x as usize,
                y: y as usize,
                width: w as usize,
                height: button_h as usize,
                item_type,
            });
        }
        Ok(())
    }
}
