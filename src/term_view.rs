//! egui widget that draws a `TermSession` and forwards keyboard/mouse input to it.

use crate::term::TermSession;
use crate::theme;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use eframe::egui::{self, Align2, Color32, EventFilter, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Vec2};

const PAD: f32 = 6.0;

pub struct TermViewOutput {
    pub response: egui::Response,
    /// The user typed into this terminal this frame.
    pub typed: bool,
}

/// Draw the terminal into the whole available rect and handle input when focused.
pub fn show(ui: &mut egui::Ui, sess: &mut TermSession, id: egui::Id, font_size: f32) -> TermViewOutput {
    let font = FontId::monospace(font_size);
    let (cw, ch) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    let ch = ch.ceil();
    let rect = ui.available_rect_before_wrap();
    let response = ui.interact(rect, id, Sense::click_and_drag());
    ui.allocate_rect(rect, Sense::hover());

    let inner = rect.shrink(PAD);
    let cols = ((inner.width() / cw).floor() as usize).max(2);
    let rows = ((inner.height() / ch).floor() as usize).max(1);
    sess.resize(cols, rows, cw, ch);

    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }
    let focused = response.has_focus();
    let mut typed = false;

    let cell_at = |pos: Pos2, display_offset: usize| -> (Point, Side, usize, usize) {
        let x = ((pos.x - inner.left()) / cw).max(0.0);
        let col = (x.floor() as usize).min(cols.saturating_sub(1));
        let row = (((pos.y - inner.top()) / ch).max(0.0).floor() as usize).min(rows.saturating_sub(1));
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        let line = Line(row as i32 - display_offset as i32);
        (Point::new(line, Column(col)), side, col, row)
    };

    // ---------- mouse ----------
    let (mode, display_offset) = {
        let t = sess.term.lock();
        (*t.mode(), t.grid().display_offset())
    };
    let mouse_mode = mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE);
    let shift_held = ui.input(|i| i.modifiers.shift);

    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let acc_id = id.with("scroll_acc");
            let acc: f32 = ui.data(|d| d.get_temp(acc_id).unwrap_or(0.0)) + scroll / ch;
            let lines = acc.trunc() as i32;
            ui.data_mut(|d| d.insert_temp(acc_id, acc - lines as f32));
            if lines != 0 {
                if mouse_mode && !shift_held {
                    if let Some(pos) = response.hover_pos() {
                        let (_, _, c, r) = cell_at(pos, 0);
                        let btn = if lines > 0 { 64 } else { 65 };
                        for _ in 0..lines.unsigned_abs().min(10) {
                            sess.write(format!("\x1b[<{btn};{};{}M", c + 1, r + 1));
                        }
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                    let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
                    for _ in 0..lines.unsigned_abs().min(10) {
                        sess.write(seq.to_vec());
                    }
                } else {
                    sess.term.lock().scroll_display(Scroll::Delta(lines));
                }
            }
        }
    }

    if mouse_mode && !shift_held {
        // Forward clicks/drags to the app (OpenCode's TUI uses the mouse).
        let pressed = ui.input(|i| i.pointer.primary_pressed());
        let released = ui.input(|i| i.pointer.primary_released());
        if let Some(pos) = response.interact_pointer_pos().or(response.hover_pos()) {
            let (_, _, c, r) = cell_at(pos, 0);
            if pressed && response.hovered() {
                sess.write(format!("\x1b[<0;{};{}M", c + 1, r + 1));
            } else if released && focused {
                sess.write(format!("\x1b[<0;{};{}m", c + 1, r + 1));
            } else if response.dragged() && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                sess.write(format!("\x1b[<32;{};{}M", c + 1, r + 1));
            }
        }
    } else {
        // Local text selection.
        if let Some(pos) = response.interact_pointer_pos() {
            let (point, side, _, _) = cell_at(pos, display_offset);
            let mut t = sess.term.lock();
            if response.double_clicked() {
                t.selection = Some(Selection::new(SelectionType::Semantic, point, side));
            } else if response.drag_started() {
                t.selection = Some(Selection::new(SelectionType::Simple, point, side));
            } else if response.dragged() {
                if let Some(sel) = t.selection.as_mut() {
                    sel.update(point, side);
                }
            } else if response.clicked() {
                t.selection = None;
            }
        }
    }
    if response.secondary_clicked() {
        let text = sess.term.lock().selection_to_string();
        match text {
            Some(text) if !text.is_empty() => {
                ui.ctx().copy_text(text);
                sess.term.lock().selection = None;
            }
            _ => {}
        }
    }

    // ---------- keyboard ----------
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            )
        });
        let events = ui.input(|i| i.events.clone());
        let mods = ui.input(|i| i.modifiers);
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        for ev in events {
            match ev {
                egui::Event::Text(t) => {
                    if !(mods.ctrl && !mods.alt) {
                        sess.write(t.into_bytes());
                        typed = true;
                    }
                }
                egui::Event::Ime(egui::ImeEvent::Commit(t)) => {
                    sess.write(t.into_bytes());
                    typed = true;
                }
                egui::Event::Paste(t) => {
                    sess.paste(&t);
                    typed = true;
                }
                egui::Event::Copy => {
                    let sel = sess.term.lock().selection_to_string().filter(|s| !s.is_empty());
                    match sel {
                        Some(text) => {
                            ui.ctx().copy_text(text);
                            sess.term.lock().selection = None;
                        }
                        None if !mods.shift => {
                            sess.write(vec![0x03]);
                            typed = true;
                        }
                        None => {}
                    }
                }
                egui::Event::Cut => {
                    sess.write(vec![0x18]);
                    typed = true;
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(bytes) = key_bytes(key, modifiers, app_cursor) {
                        sess.write(bytes);
                        typed = true;
                    }
                }
                _ => {}
            }
        }
        if typed {
            sess.scroll_to_bottom();
            sess.term.lock().selection = None;
        }
    }

    paint(ui, sess, rect, inner, &font, cw, ch, focused);
    TermViewOutput { response, typed }
}

#[allow(clippy::too_many_arguments)]
fn paint(ui: &egui::Ui, sess: &TermSession, rect: Rect, inner: Rect, font: &FontId, cw: f32, ch: f32, focused: bool) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::TERM_BG);
    // Cell backgrounds are collected and placed in this slot so they sit under all text.
    let bg_slot = painter.add(egui::Shape::Noop);
    let bg_shapes = std::cell::RefCell::new(Vec::<egui::Shape>::new());

    let t = sess.term.lock();
    let content = t.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;

    // Text run of same-styled ASCII cells.
    struct Run {
        row: i32,
        col: usize,
        fg: Color32,
        underline: bool,
        strike: bool,
        text: String,
    }
    let mut run: Option<Run> = None;
    // Background run.
    let mut bg_run: Option<(i32, usize, usize, Color32)> = None;

    let pos_of = |row: i32, col: usize| Pos2::new(inner.left() + col as f32 * cw, inner.top() + row as f32 * ch);
    let flush = |run: &mut Option<Run>| {
        if let Some(r) = run.take() {
            if r.text.trim_end().is_empty() && !r.underline && !r.strike {
                return;
            }
            let p = pos_of(r.row, r.col);
            painter.text(p, Align2::LEFT_TOP, &r.text, font.clone(), r.fg);
            let w = r.text.chars().count() as f32 * cw;
            if r.underline {
                let y = p.y + ch - 1.5;
                painter.line_segment([Pos2::new(p.x, y), Pos2::new(p.x + w, y)], Stroke::new(1.0_f32, r.fg));
            }
            if r.strike {
                let y = p.y + ch * 0.55;
                painter.line_segment([Pos2::new(p.x, y), Pos2::new(p.x + w, y)], Stroke::new(1.0_f32, r.fg));
            }
        }
    };
    let flush_bg = |bg: &mut Option<(i32, usize, usize, Color32)>| {
        if let Some((row, c0, c1, color)) = bg.take() {
            let a = pos_of(row, c0);
            bg_shapes.borrow_mut().push(egui::Shape::rect_filled(
                Rect::from_min_size(a, Vec2::new((c1 - c0) as f32 * cw, ch)),
                0.0,
                color,
            ));
        }
    };

    for indexed in content.display_iter {
        let cell = indexed.cell;
        let point = indexed.point;
        let row = point.line.0 + offset;
        let col = point.column.0;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

        let mut fg = resolve(cell.fg, colors, cell.flags.contains(Flags::BOLD));
        let mut bg = resolve(cell.bg, colors, false);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = theme::dim(fg);
        }
        if selection.is_some_and(|s| s.contains(point)) {
            bg = theme::ACCENT.gamma_multiply(0.45);
        }
        let is_cursor = show_cursor && point == cursor.point && offset == 0;
        if is_cursor && cursor.shape == CursorShape::Block && focused {
            fg = theme::TERM_BG;
            bg = theme::TERM_FG;
        }

        // background
        if bg != theme::TERM_BG {
            match &mut bg_run {
                Some((r, _, c1, color)) if *r == row && *c1 == col && *color == bg => *c1 += width,
                _ => {
                    flush_bg(&mut bg_run);
                    bg_run = Some((row, col, col + width, bg));
                }
            }
        } else {
            flush_bg(&mut bg_run);
        }

        // foreground
        let hidden = cell.flags.contains(Flags::HIDDEN);
        let c = if hidden { ' ' } else { cell.c };
        let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
        let strike = cell.flags.contains(Flags::STRIKEOUT);
        let zw = cell.zerowidth().filter(|z| !z.is_empty());
        if c.is_ascii() && zw.is_none() {
            match &mut run {
                Some(r) if r.row == row && r.col + r.text.len() == col && r.fg == fg && r.underline == underline && r.strike == strike => {
                    r.text.push(c)
                }
                _ => {
                    flush(&mut run);
                    run = Some(Run { row, col, fg, underline, strike, text: c.to_string() });
                }
            }
        } else {
            flush(&mut run);
            let mut s = c.to_string();
            if let Some(z) = zw {
                s.extend(z.iter());
            }
            let p = pos_of(row, col);
            // Center the glyph in its cell(s); fallback fonts may be wider/narrower.
            let center = Pos2::new(p.x + width as f32 * cw / 2.0, p.y);
            painter.text(center, Align2::CENTER_TOP, s, font.clone(), fg);
        }

        if is_cursor && !(cursor.shape == CursorShape::Block && focused) {
            let p = pos_of(row, col);
            let r = Rect::from_min_size(p, Vec2::new(cw * width as f32, ch));
            let color = theme::TERM_FG;
            match cursor.shape {
                CursorShape::Beam => {
                    painter.rect_filled(Rect::from_min_size(p, Vec2::new(2.0, ch)), 0.0, color);
                }
                CursorShape::Underline => {
                    painter.rect_filled(Rect::from_min_size(Pos2::new(p.x, p.y + ch - 2.0), Vec2::new(r.width(), 2.0)), 0.0, color);
                }
                _ => {
                    painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0_f32, color), egui::StrokeKind::Inside);
                }
            }
        }
    }
    flush_bg(&mut bg_run);
    flush(&mut run);
    painter.set(bg_slot, egui::Shape::Vec(bg_shapes.into_inner()));

    if content.display_offset > 0 {
        let label = format!("↑ {} lines", content.display_offset);
        let p = Pos2::new(rect.right() - 10.0, rect.top() + 8.0);
        let galley = painter.layout_no_wrap(label, FontId::proportional(11.5), theme::TEXT);
        let r = Rect::from_min_size(Pos2::new(p.x - galley.size().x - 12.0, p.y), galley.size() + Vec2::new(12.0, 6.0));
        painter.rect_filled(r, 6.0, theme::RAISED);
        painter.galley(r.min + Vec2::new(6.0, 3.0), galley, theme::TEXT);
    }
    let _ = t.grid().total_lines();
}

fn resolve(c: Color, colors: &Colors, bold: bool) -> Color32 {
    match c {
        Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => {
            let i = if bold && i < 8 { i + 8 } else { i } as usize;
            colors[i].map(|c| Color32::from_rgb(c.r, c.g, c.b)).unwrap_or_else(|| theme::term_color(i))
        }
        Color::Named(n) => {
            let n = if bold { bright(n) } else { n };
            let i = n as usize;
            colors[i].map(|c| Color32::from_rgb(c.r, c.g, c.b)).unwrap_or_else(|| theme::term_color(i))
        }
    }
}

fn bright(n: NamedColor) -> NamedColor {
    if (n as usize) < 8 {
        n.to_bright()
    } else {
        n
    }
}

/// Bytes a terminal sends for a key press. `None` for keys that arrive as text
/// (letters, digits, space) or are handled as clipboard events (Ctrl+C/X/V).
pub fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier_param = 1 + (m.shift as u8) + 2 * (m.alt as u8) + 4 * (m.ctrl as u8);
    let csi_mod = |final_byte: char| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[1;{modifier_param}{final_byte}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[{n};{modifier_param}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let bytes = match key {
        Key::Enter => {
            if m.shift || m.alt {
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        Key::Tab => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        Key::Backspace => {
            if m.ctrl {
                vec![0x08]
            } else if m.alt {
                b"\x1b\x7f".to_vec()
            } else {
                vec![0x7f]
            }
        }
        Key::Escape => b"\x1b".to_vec(),
        Key::ArrowUp => csi_mod('A'),
        Key::ArrowDown => csi_mod('B'),
        Key::ArrowRight => csi_mod('C'),
        Key::ArrowLeft => csi_mod('D'),
        Key::Home => csi_mod('H'),
        Key::End => csi_mod('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Space if m.ctrl => vec![0],
        _ => {
            let name = key.name();
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            if !c.is_ascii_alphabetic() {
                return None;
            }
            if m.ctrl && !m.alt {
                // Ctrl+C / X / V arrive as Copy / Cut / Paste events instead.
                if matches!(c, 'C' | 'X' | 'V') {
                    return None;
                }
                vec![(c.to_ascii_uppercase() as u8) - b'A' + 1]
            } else if m.alt && !m.ctrl {
                let c = if m.shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
                vec![0x1b, c as u8]
            } else {
                return None;
            }
        }
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let none = Modifiers::NONE;
        assert_eq!(key_bytes(Key::Enter, none, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::Enter, Modifiers::SHIFT, false), Some(b"\x1b\r".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, none, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, none, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(Key::ArrowLeft, Modifiers::CTRL, false), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(key_bytes(Key::Tab, Modifiers::SHIFT, false), Some(b"\x1b[Z".to_vec()));
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL, false), Some(vec![4]));
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false), None);
        assert_eq!(key_bytes(Key::B, Modifiers::ALT, false), Some(vec![0x1b, b'b']));
        assert_eq!(key_bytes(Key::A, none, false), None);
        assert_eq!(key_bytes(Key::Delete, none, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key_bytes(Key::Backspace, none, false), Some(vec![0x7f]));
    }
}
