//! egui widget that draws a `TermSession` and forwards keyboard/mouse input to it.

use crate::term::{PaintKey, SearchMatch, TermSession};
use crate::theme;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use eframe::egui::{self, Align2, Color32, EventFilter, FontId, Key, Margin, Modifiers, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use std::path::{Path, PathBuf};

const PAD: f32 = 6.0;

pub struct TermViewOutput {
    pub response: egui::Response,
    /// The user typed into this terminal this frame.
    pub typed: bool,
}

/// Incremental scrollback search state for one terminal pane.
#[derive(Default)]
pub struct TermSearch {
    pub open: bool,
    pub query: String,
    pub focus_input: bool,
    pub matches: Vec<SearchMatch>,
    pub active: usize,
    last_gen: u64,
    last_query: String,
}

impl TermSearch {
    pub fn open(&mut self) {
        self.open = true;
        self.focus_input = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focus_input = false;
    }

    fn refresh(&mut self, sess: &TermSession) -> bool {
        let gen = sess.generation();
        if self.query == self.last_query && gen == self.last_gen {
            return false;
        }
        let query_changed = self.query != self.last_query;
        self.last_query = self.query.clone();
        self.last_gen = gen;
        self.matches = sess.search(&self.query);
        if self.matches.is_empty() {
            self.active = 0;
        } else if query_changed {
            // Jump to the latest (bottom-most) match first — closest to current output.
            self.active = self.matches.len() - 1;
        } else if self.active >= self.matches.len() {
            self.active = self.matches.len() - 1;
        }
        query_changed
    }

    fn step(&mut self, sess: &TermSession, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.active = (self.active as isize + delta).rem_euclid(len) as usize;
        sess.scroll_to_line(self.matches[self.active].line);
    }
}

/// Draw the terminal into the whole available rect and handle input when focused.
/// `cwd` resolves relative file paths for Ctrl+click.
pub fn show(
    ui: &mut egui::Ui,
    sess: &mut TermSession,
    id: egui::Id,
    font_size: f32,
    cwd: &Path,
    search: &mut TermSearch,
    single_pane: bool,
) -> TermViewOutput {
    let font = FontId::monospace(font_size);
    // Cell width = the real advance of a long run (text is drawn in runs, so a
    // single glyph's rounded width would drift and clip the right edge).
    // Measured once per font size / display scale, then remembered.
    let metrics_id = egui::Id::new(("term_metrics", font_size.to_bits(), ui.ctx().pixels_per_point().to_bits()));
    let (cw, glyph_h) = match ui.data(|d| d.get_temp::<(f32, f32)>(metrics_id)) {
        Some(m) => m,
        None => {
            let m = ui.fonts(|f| {
                let g = f.layout_no_wrap("M".repeat(100), font.clone(), Color32::WHITE);
                (g.size().x / 100.0, f.row_height(&font))
            });
            ui.data_mut(|d| d.insert_temp(metrics_id, m));
            m
        }
    };
    let glyph_h = glyph_h.ceil();
    let rect = ui.available_rect_before_wrap();

    // Reserve overlay zones before registering the main terminal click handler so
    // clicks on the search bar, jump-to-bottom pill, or scrollbar don't steal/leak.
    let (mode, display_offset, history_size) = {
        let t = sess.term.lock();
        (*t.mode(), t.grid().display_offset(), t.grid().history_size())
    };
    let sb_hit_rect = if history_size > 0 {
        Rect::from_min_max(Pos2::new(rect.right() - 10.0, rect.top() + 4.0), Pos2::new(rect.right(), rect.bottom() - 4.0))
    } else {
        Rect::NOTHING
    };
    let search_bar_h = if search.open { 34.0 } else { 0.0 };
    let search_bar_rect = if search.open {
        let w = 290.0_f32.min((rect.width() - 24.0).max(120.0));
        Rect::from_min_size(Pos2::new(rect.right() - w - 12.0, rect.top() + 6.0), Vec2::new(w, 28.0))
    } else {
        Rect::NOTHING
    };
    let pill_rect = if display_offset > 0 {
        let label = format!("↑ {display_offset} lines");
        let sz = ui.fonts(|f| f.layout_no_wrap(label, FontId::proportional(11.5), theme::TEXT).size());
        let w = sz.x + 14.0;
        let h = sz.y + 6.0;
        let right = if history_size > 0 { rect.right() - 14.0 } else { rect.right() - 10.0 };
        Rect::from_min_size(Pos2::new(right - w, rect.top() + 8.0 + search_bar_h), Vec2::new(w, h))
    } else {
        Rect::NOTHING
    };

    let pointer_pos = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
    let over_overlay = pointer_pos.is_some_and(|p| {
        (search.open && search_bar_rect.contains(p))
            || (display_offset > 0 && pill_rect.contains(p))
            || (history_size > 0 && sb_hit_rect.contains(p))
    });

    let response = ui.interact(rect, id, Sense::click_and_drag());
    ui.allocate_rect(rect, Sense::hover());

    let inner = rect.shrink(PAD);
    let cols = ((inner.width() / cw).floor() as usize).max(2);
    let rows = ((inner.height() / glyph_h).floor() as usize).max(1);
    // Spread rows a hair so they fill the pane: no empty strip at the bottom.
    let ch = (inner.height() / rows as f32).max(glyph_h);
    let text_dy = ((ch - glyph_h) / 2.0).floor();
    sess.resize(cols, rows, cw, ch);

    if !over_overlay && (response.clicked() || response.drag_started() || response.secondary_clicked()) {
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
    let mouse_mode = mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE);
    let shift_held = ui.input(|i| i.modifiers.shift);

    // ---------- Ctrl+click: open URLs and file paths ----------
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let mut link: Option<(usize, usize, usize)> = None; // (row, first col, last col)
    if ctrl_held && response.hovered() && !over_overlay {
        if let Some(pos) = response.hover_pos() {
            let (point, _, col, row) = cell_at(pos, display_offset);
            if let Some((c0, c1, target)) = link_at(&row_chars(sess, point.line), col, cwd) {
                link = Some((row, c0, c1));
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    open_target(&target);
                }
            }
        }
    }

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

    if !over_overlay {
        if mouse_mode && !shift_held && link.is_none() {
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
                _ => {
                    let clip = read_clipboard_text().filter(|c| !c.is_empty()).or_else(clipboard_attachment);
                    if let Some(clip) = clip {
                        sess.paste(&clip);
                        sess.scroll_to_bottom();
                        typed = true;
                    }
                }
            }
        }
    }

    // ---------- drag-and-drop files ----------
    let (hovered_files, dropped_paths) = ui.input(|i| {
        let hovered = !i.raw.hovered_files.is_empty();
        let dropped: Vec<PathBuf> = i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect();
        (hovered, dropped)
    });
    let drop_target = ui
        .input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()))
        .is_some_and(|p| rect.contains(p))
        || single_pane;
    if !dropped_paths.is_empty() && drop_target {
        let formatted = format_dropped_paths(&dropped_paths);
        if !formatted.is_empty() {
            sess.paste(&formatted);
            sess.scroll_to_bottom();
            response.request_focus();
            typed = true;
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
        let alt_screen = mode.contains(TermMode::ALT_SCREEN);
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
                // egui turns the Ctrl+V press into a Paste event only when the
                // clipboard holds text, but the release still arrives. Without
                // text, paste a copied image (saved as PNG) or copied files as paths.
                egui::Event::Key { key: Key::V, pressed: false, modifiers, .. }
                    if modifiers.command && !modifiers.alt && read_clipboard_text().is_none_or(|t| t.is_empty()) =>
                {
                    if let Some(paths) = clipboard_attachment() {
                        sess.paste(&paths);
                        sess.scroll_to_bottom();
                        typed = true;
                    }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    // Shift+PageUp/PageDown/Home/End scroll the viewport directly.
                    if !alt_screen && modifiers.shift && !modifiers.ctrl && !modifiers.alt {
                        let scroll = match key {
                            Key::PageUp => Some(Scroll::PageUp),
                            Key::PageDown => Some(Scroll::PageDown),
                            Key::Home => Some(Scroll::Top),
                            Key::End => Some(Scroll::Bottom),
                            _ => None,
                        };
                        if let Some(s) = scroll {
                            sess.term.lock().scroll_display(s);
                            continue;
                        }
                    }
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

    // ---------- paint cached terminal frame ----------
    let (display_offset, history_size, key) = {
        let t = sess.term.lock();
        let d = t.grid().display_offset();
        let h = t.grid().history_size();
        let key = PaintKey {
            generation: sess.generation(),
            rect,
            cell: (cw, ch),
            font_size,
            focused,
            display_offset: d,
            selection: t.selection.as_ref().and_then(|s| s.to_range(&*t)),
        };
        (d, h, key)
    };
    let painter = ui.painter_at(rect);
    match &sess.paint_cache {
        Some((k, shape)) if *k == key => {
            painter.add(shape.clone());
        }
        _ => {
            let shape = paint(ui.ctx(), sess, rect, inner, &font, cw, ch, text_dy, focused);
            painter.add(shape.clone());
            sess.paint_cache = Some((key, shape));
        }
    }

    // ---------- search highlights overlay ----------
    if search.open {
        let jumped = search.refresh(sess);
        if jumped {
            if let Some(m) = search.matches.get(search.active) {
                sess.scroll_to_line(m.line);
            }
        }
        let cur_offset = sess.term.lock().grid().display_offset() as i32;
        for (idx, m) in search.matches.iter().enumerate() {
            let vis_row = m.line + cur_offset;
            if vis_row < 0 || vis_row >= rows as i32 {
                continue;
            }
            let r = Rect::from_min_size(
                Pos2::new(inner.left() + m.col_start as f32 * cw, inner.top() + vis_row as f32 * ch),
                Vec2::new((m.col_end - m.col_start) as f32 * cw, ch),
            );
            if idx == search.active {
                painter.rect_filled(r, 2.0, theme::AMBER.gamma_multiply(0.45));
                painter.rect_stroke(r, 2.0, Stroke::new(1.0_f32, theme::AMBER), egui::StrokeKind::Inside);
            } else {
                painter.rect_filled(r, 2.0, theme::AMBER.gamma_multiply(0.22));
            }
        }
    }

    // ---------- Ctrl+hover link underline ----------
    if let Some((row, c0, c1)) = link {
        let y = inner.top() + (row as f32 + 1.0) * ch - 1.0;
        let x0 = inner.left() + c0 as f32 * cw;
        let x1 = inner.left() + (c1 + 1) as f32 * cw;
        painter.line_segment([Pos2::new(x0, y), Pos2::new(x1, y)], Stroke::new(1.5_f32, theme::ACCENT));
    }

    // ---------- slim hover scrollbar ----------
    if history_size > 0 {
        let sb_resp = ui.interact(sb_hit_rect, id.with("scrollbar"), Sense::click_and_drag());
        let near_edge = pointer_pos.is_some_and(|p| rect.contains(p) && rect.right() - p.x < 28.0);
        let active = sb_resp.hovered() || sb_resp.dragged();
        if display_offset > 0 || near_edge || active {
            let track_h = sb_hit_rect.height().max(1.0);
            let total = (rows + history_size) as f32;
            let thumb_h = ((rows as f32 / total) * track_h).clamp(20.0, track_h);
            let travel = (track_h - thumb_h).max(1.0);
            if sb_resp.dragged() || sb_resp.clicked() {
                if let Some(p) = sb_resp.interact_pointer_pos() {
                    let frac = ((p.y - sb_hit_rect.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
                    let target = ((1.0 - frac) * history_size as f32).round() as usize;
                    sess.scroll_to_offset(target);
                }
            }
            let cur_off = sess.term.lock().grid().display_offset();
            let frac = 1.0 - (cur_off as f32 / history_size as f32);
            let thumb_top = sb_hit_rect.top() + frac * travel;
            let bar_w = if active { 6.0 } else { 3.0 };
            let thumb = Rect::from_min_size(
                Pos2::new(rect.right() - bar_w - 2.0, thumb_top),
                Vec2::new(bar_w, thumb_h),
            );
            let alpha = if active { 0.75 } else { 0.4 };
            painter.rect_filled(thumb, bar_w * 0.5, theme::MUTED.gamma_multiply(alpha));
            if active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    }

    // ---------- clickable "↑ N lines" jump-to-bottom pill ----------
    let live_offset = sess.term.lock().grid().display_offset();
    if live_offset > 0 && pill_rect.is_positive() {
        let pill_resp = ui
            .interact(pill_rect, id.with("scroll_pill"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Jump to bottom (Shift+End)");
        let fill = if pill_resp.hovered() { theme::HOVER } else { theme::RAISED };
        painter.rect_filled(pill_rect, 6.0, fill);
        painter.rect_stroke(pill_rect, 6.0, Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
        let label = format!("↑ {live_offset} lines");
        let galley = ui.fonts(|f| f.layout_no_wrap(label, FontId::proportional(11.5), theme::TEXT));
        painter.galley(pill_rect.center() - galley.size() / 2.0, galley, theme::TEXT);
        if pill_resp.clicked() {
            sess.scroll_to_bottom();
        }
    }

    // ---------- floating search bar (Ctrl+Shift+F) ----------
    if search.open && search_bar_rect.is_positive() {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(search_bar_rect));
        egui::Frame::new()
            .fill(theme::RAISED)
            .stroke(Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(6, 3))
            .show(&mut child, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let input_w = (ui.available_width() - 96.0).max(60.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut search.query)
                            .hint_text("Find…")
                            .desired_width(input_w)
                            .margin(Margin::symmetric(4, 1)),
                    );
                    if std::mem::take(&mut search.focus_input) {
                        resp.request_focus();
                    }
                    if resp.has_focus() {
                        if ui.input(|i| i.key_pressed(Key::Escape)) {
                            search.close();
                            ui.memory_mut(|m| m.request_focus(id));
                        } else if ui.input(|i| i.key_pressed(Key::Enter)) {
                            let shift = ui.input(|i| i.modifiers.shift);
                            search.refresh(sess);
                            search.step(sess, if shift { -1 } else { 1 });
                            resp.request_focus();
                        }
                    }
                    let count_text = if search.query.is_empty() {
                        String::new()
                    } else if search.matches.is_empty() {
                        "0/0".into()
                    } else {
                        format!("{}/{}", search.active + 1, search.matches.len())
                    };
                    let count_col = if !search.query.is_empty() && search.matches.is_empty() {
                        theme::RED
                    } else {
                        theme::MUTED
                    };
                    ui.label(egui::RichText::new(count_text).small().color(count_col));
                    if ui.small_button("↑").on_hover_text("Previous (Shift+Enter)").clicked() {
                        search.refresh(sess);
                        search.step(sess, -1);
                    }
                    if ui.small_button("↓").on_hover_text("Next (Enter)").clicked() {
                        search.refresh(sess);
                        search.step(sess, 1);
                    }
                    if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
                        search.close();
                        ui.memory_mut(|m| m.request_focus(id));
                    }
                });
            });
    }

    // ---------- file drag-over indicator ----------
    if hovered_files && drop_target {
        painter.rect_filled(rect, 4.0, theme::ACCENT.gamma_multiply(0.10));
        painter.rect_stroke(rect.shrink(1.0), 4.0, Stroke::new(2.0_f32, theme::ACCENT), egui::StrokeKind::Inside);
        let galley = ui.fonts(|f| f.layout_no_wrap("Drop to paste path".into(), theme::bold(13.0), theme::TEXT));
        let badge = Rect::from_center_size(rect.center(), galley.size() + Vec2::new(20.0, 12.0));
        painter.rect_filled(badge, 8.0, theme::RAISED);
        painter.rect_stroke(badge, 8.0, Stroke::new(1.0_f32, theme::ACCENT), egui::StrokeKind::Inside);
        painter.galley(badge.center() - galley.size() / 2.0, galley, theme::TEXT);
    }

    TermViewOutput { response, typed }
}

/// Format dropped filesystem paths for pasting into a shell/TUI prompt.
/// Paths containing whitespace or common shell metacharacters are double-quoted.
pub fn format_dropped_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| {
            let s = p.to_string_lossy();
            if s.chars().any(|c| c.is_whitespace() || matches!(c, '&' | '(' | ')' | '^' | ';' | '\'' | '"')) {
                format!("\"{}\"", s.replace('"', "\\\""))
            } else {
                s.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Non-text clipboard contents as something a TUI can use: copied files
/// (Explorer) become their quoted paths; an image (screenshot, browser "Copy
/// image") is saved as a PNG and its path returned, so the agent can read it.
fn clipboard_attachment() -> Option<String> {
    let mut cb = arboard::Clipboard::new().ok()?;
    if let Ok(files) = cb.get().file_list() {
        let s = format_dropped_paths(&files);
        if !s.is_empty() {
            return Some(s);
        }
    }
    let img = cb.get_image().ok()?;
    drop(cb);
    let path = save_pasted_image(img.width as u32, img.height as u32, &img.bytes)?;
    Some(format_dropped_paths(&[path]))
}

/// Write RGBA pixels to `%TEMP%\kennel-pastes\paste-<time>.png`, pruning
/// pastes older than a week.
fn save_pasted_image(w: u32, h: u32, rgba: &[u8]) -> Option<PathBuf> {
    use image::ImageEncoder;
    let dir = std::env::temp_dir().join("kennel-pastes");
    std::fs::create_dir_all(&dir).ok()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let week = std::time::Duration::from_secs(7 * 24 * 3600);
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > week);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    let path = dir.join(format!("paste-{ms}.png"));
    let file = std::io::BufWriter::new(std::fs::File::create(&path).ok()?);
    let enc = image::codecs::png::PngEncoder::new_with_quality(
        file,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    enc.write_image(rgba, w, h, image::ExtendedColorType::Rgba8).ok()?;
    Some(path)
}

/// Read UTF-16 text from the Windows clipboard without extra crate dependencies.
#[cfg(windows)]
fn read_clipboard_text() -> Option<String> {
    use std::ffi::c_void;
    const CF_UNICODETEXT: u32 = 13;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalLock(hmem: *mut c_void) -> *const u16;
        fn GlobalUnlock(hmem: *mut c_void) -> i32;
    }

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                return None;
            }
            let ptr = GlobalLock(handle);
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(ptr, len);
            let s = String::from_utf16_lossy(slice);
            GlobalUnlock(handle);
            Some(s)
        })();
        CloseClipboard();
        result
    }
}

#[cfg(not(windows))]
fn read_clipboard_text() -> Option<String> {
    None
}

/// Characters of one grid line (viewport-relative `Line`, as `cell_at` returns).
fn row_chars(sess: &TermSession, line: Line) -> Vec<char> {
    let t = sess.term.lock();
    let g = t.grid();
    if line.0 < -(g.history_size() as i32) || line.0 >= g.screen_lines() as i32 {
        return Vec::new();
    }
    let row = &g[line];
    (0..g.columns()).map(|c| row[Column(c)].c).collect()
}

/// The URL or existing file path under column `col`, as (first col, last col, target).
/// Paths may carry a `:line[:col]` suffix and may be relative to `cwd` or start with `~`.
pub fn link_at(chars: &[char], col: usize, cwd: &Path) -> Option<(usize, usize, String)> {
    let is_sep = |c: char| c.is_whitespace() || "\"'`<>()[]{}|│,".contains(c);
    if col >= chars.len() || is_sep(chars[col]) {
        return None;
    }
    let mut s = col;
    while s > 0 && !is_sep(chars[s - 1]) {
        s -= 1;
    }
    let mut e = col;
    while e + 1 < chars.len() && !is_sep(chars[e + 1]) {
        e += 1;
    }
    let mut token: String = chars[s..=e].iter().collect();
    while token.len() > 1 && token.ends_with(['.', ';', ':', '!', '?']) {
        token.pop();
        e -= 1;
    }
    if col > e {
        return None;
    }
    if token.starts_with("https://") || token.starts_with("http://") {
        return Some((s, e, token));
    }
    let path = strip_line_suffix(&token);
    if !(path.contains('\\') || path.contains('/') || path.contains('.')) {
        return None;
    }
    let expanded = match path.strip_prefix('~') {
        Some(rest) => format!("{}{}", std::env::var("USERPROFILE").unwrap_or_default(), rest),
        None => path.to_string(),
    };
    let p = Path::new(&expanded);
    let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    full.exists().then(|| (s, e, full.to_string_lossy().into_owned()))
}

/// "src/main.rs:12:5" -> "src/main.rs" (a drive's "C:" is kept).
fn strip_line_suffix(token: &str) -> &str {
    let mut t = token;
    for _ in 0..2 {
        match t.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => t = head,
            _ => break,
        }
    }
    t
}

/// Open a URL or file with its default app. Programs and scripts are only
/// revealed in Explorer, never run.
fn open_target(target: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        const RUNNABLE: &[&str] = &["exe", "bat", "cmd", "com", "ps1", "vbs", "js", "msi", "lnk", "scr", "pif", "jar", "reg", "hta"];
        let is_url = target.starts_with("http://") || target.starts_with("https://");
        let ext = Path::new(target).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let _ = if !is_url && RUNNABLE.contains(&ext.as_str()) {
            std::process::Command::new("explorer").arg(format!("/select,{target}")).spawn()
        } else {
            std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", target])
                .creation_flags(NO_WINDOW)
                .spawn()
        };
    }
}

/// Collects shapes instead of painting them, so a frame can be cached and reused.
struct Rec<'a> {
    ctx: &'a egui::Context,
    shapes: std::cell::RefCell<Vec<Shape>>,
}

impl Rec<'_> {
    fn push(&self, s: Shape) {
        self.shapes.borrow_mut().push(s);
    }
    fn layout_no_wrap(&self, text: String, font: FontId, color: Color32) -> std::sync::Arc<egui::Galley> {
        self.ctx.fonts(|f| f.layout_no_wrap(text, font, color))
    }
    fn text(&self, pos: Pos2, align: Align2, text: impl ToString, font: FontId, color: Color32) {
        let galley = self.layout_no_wrap(text.to_string(), font, color);
        let r = align.anchor_size(pos, galley.size());
        self.push(Shape::galley(r.min, galley, color));
    }
    fn rect_filled(&self, rect: Rect, radius: f32, color: Color32) {
        self.push(Shape::rect_filled(rect, radius, color));
    }
    fn rect_stroke(&self, rect: Rect, radius: f32, stroke: Stroke, kind: egui::StrokeKind) {
        self.push(Shape::rect_stroke(rect, radius, stroke, kind));
    }
    fn line_segment(&self, pts: [Pos2; 2], stroke: Stroke) {
        self.push(Shape::line_segment(pts, stroke));
    }
    fn polyline(&self, pts: Vec<Pos2>, stroke: Stroke) {
        self.push(Shape::line(pts, stroke));
    }
}

/// Build the terminal's picture: background, cell backgrounds, then text.
#[allow(clippy::too_many_arguments)]
fn paint(ctx: &egui::Context, sess: &TermSession, rect: Rect, inner: Rect, font: &FontId, cw: f32, ch: f32, text_dy: f32, focused: bool) -> Shape {
    let painter = Rec { ctx, shapes: Default::default() };
    // Cell backgrounds are collected separately so they sit under all text.
    let bg_shapes = std::cell::RefCell::new(vec![Shape::rect_filled(rect, 0.0, theme::TERM_BG)]);

    let t = sess.term.lock();
    let content = t.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;

    // Text run of same-styled single-width cells (ASCII or batched non-ASCII).
    struct Run {
        row: i32,
        col: usize,
        cells: usize,
        ascii: bool,
        fg: Color32,
        bold: bool,
        underline: bool,
        strike: bool,
        text: String,
    }
    // Bold = the same glyphs drawn again a hair to the right: real weight,
    // identical width, so columns stay aligned.
    const BOLD_SHIFT: f32 = 0.7;
    let mut run: Option<Run> = None;
    // Background run.
    let mut bg_run: Option<(i32, usize, usize, Color32)> = None;

    let pos_of = |row: i32, col: usize| Pos2::new(inner.left() + col as f32 * cw, inner.top() + row as f32 * ch);
    let draw_decor = |p: Pos2, w: f32, underline: bool, strike: bool, fg: Color32| {
        if underline {
            let y = p.y + ch - 1.5;
            painter.line_segment([Pos2::new(p.x, y), Pos2::new(p.x + w, y)], Stroke::new(1.0_f32, fg));
        }
        if strike {
            let y = p.y + ch * 0.55;
            painter.line_segment([Pos2::new(p.x, y), Pos2::new(p.x + w, y)], Stroke::new(1.0_f32, fg));
        }
    };
    let flush = |run: &mut Option<Run>| {
        if let Some(r) = run.take() {
            if r.text.trim_end().is_empty() && !r.underline && !r.strike {
                return;
            }
            let p = pos_of(r.row, r.col);
            let w = r.cells as f32 * cw;
            if !r.ascii && r.cells == 1 {
                // Single isolated non-ASCII symbol (e.g. ●, ❯, ✻): center in cell.
                let center = Pos2::new(p.x + cw / 2.0, p.y + text_dy);
                if r.bold {
                    painter.text(center + Vec2::new(BOLD_SHIFT, 0.0), Align2::CENTER_TOP, &r.text, font.clone(), r.fg);
                }
                painter.text(center, Align2::CENTER_TOP, &r.text, font.clone(), r.fg);
            } else {
                let tp = p + Vec2::new(0.0, text_dy);
                painter.text(tp, Align2::LEFT_TOP, &r.text, font.clone(), r.fg);
                if r.bold {
                    painter.text(tp + Vec2::new(BOLD_SHIFT, 0.0), Align2::LEFT_TOP, &r.text, font.clone(), r.fg);
                }
            }
            draw_decor(p, w, r.underline, r.strike, r.fg);
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

        let bold = cell.flags.contains(Flags::BOLD);
        let mut fg = resolve(cell.fg, colors, bold);
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
                Some(r)
                    if r.ascii
                        && r.row == row
                        && r.col + r.cells == col
                        && r.fg == fg
                        && r.bold == bold
                        && r.underline == underline
                        && r.strike == strike =>
                {
                    r.text.push(c);
                    r.cells += 1;
                }
                _ => {
                    flush(&mut run);
                    run = Some(Run { row, col, cells: 1, ascii: true, fg, bold, underline, strike, text: c.to_string() });
                }
            }
        } else if zw.is_none() && ('\u{2500}'..='\u{257F}').contains(&c) {
            // Box-drawing characters drawn as crisp pixel-aligned vector primitives.
            flush(&mut run);
            let cell_rect = Rect::from_min_size(pos_of(row, col), Vec2::new(cw, ch));
            draw_box_drawing(&painter, c, cell_rect, fg);
        } else if let Some(rects) = zw.is_none().then(|| block_rects(c)).flatten() {
            // Block elements are drawn as solid shapes so they tile without gaps.
            flush(&mut run);
            let cell_rect = Rect::from_min_size(pos_of(row, col), Vec2::new(cw, ch));
            let (rects, alpha) = rects;
            let color = if alpha < 1.0 { fg.gamma_multiply(alpha) } else { fg };
            for (x0, y0, x1, y1) in rects {
                let r = Rect::from_min_max(
                    Pos2::new(cell_rect.left() + x0 * cw, cell_rect.top() + y0 * ch),
                    Pos2::new(cell_rect.left() + x1 * cw, cell_rect.top() + y1 * ch),
                );
                painter.rect_filled(r.expand(0.25), 0.0, color);
            }
        } else if width == 1 {
            // Batch consecutive single-width non-ASCII cells (Thai, accents, etc.)
            // into one text run so combining glyphs shape cleanly and allocations drop.
            if theme::is_thai(c) {
                theme::WANT_THAI.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            match &mut run {
                Some(r)
                    if !r.ascii
                        && r.row == row
                        && r.col + r.cells == col
                        && r.fg == fg
                        && r.bold == bold
                        && r.underline == underline
                        && r.strike == strike =>
                {
                    r.text.push(c);
                    if let Some(z) = zw {
                        r.text.extend(z.iter());
                    }
                    r.cells += 1;
                }
                _ => {
                    flush(&mut run);
                    let mut text = c.to_string();
                    if let Some(z) = zw {
                        text.extend(z.iter());
                    }
                    run = Some(Run { row, col, cells: 1, ascii: false, fg, bold, underline, strike, text });
                }
            }
        } else {
            // Wide character (CJK / emoji): center in its 2-cell slot.
            flush(&mut run);
            if theme::is_thai(c) {
                theme::WANT_THAI.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let mut s = c.to_string();
            if let Some(z) = zw {
                s.extend(z.iter());
            }
            let p = pos_of(row, col);
            let center = Pos2::new(p.x + width as f32 * cw / 2.0, p.y + text_dy);
            if bold {
                painter.text(center + Vec2::new(BOLD_SHIFT, 0.0), Align2::CENTER_TOP, &s, font.clone(), fg);
            }
            painter.text(center, Align2::CENTER_TOP, s, font.clone(), fg);
            draw_decor(p, width as f32 * cw, underline, strike, fg);
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

    drop(t);
    let mut all = bg_shapes.into_inner();
    all.append(&mut painter.shapes.into_inner());
    Shape::Vec(all)
}

// ---------- Vector box-drawing (U+2500..=U+257F) ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    None,
    Light,
    Heavy,
    Double,
}

/// Cardinal arms (left, right, up, down) for standard Unicode box-drawing characters.
fn box_arms(c: char) -> Option<(Arm, Arm, Arm, Arm)> {
    use Arm::{Double as D, Heavy as H, Light as L, None as N};
    let arms = match c {
        '\u{2500}' => (L, L, N, N), // ─
        '\u{2501}' => (H, H, N, N), // ━
        '\u{2502}' => (N, N, L, L), // │
        '\u{2503}' => (N, N, H, H), // ┃
        '\u{250C}' => (N, L, N, L), // ┌
        '\u{250D}' => (N, H, N, L), // ┍
        '\u{250E}' => (N, L, N, H), // ┎
        '\u{250F}' => (N, H, N, H), // ┏
        '\u{2510}' => (L, N, N, L), // ┐
        '\u{2511}' => (H, N, N, L), // ┑
        '\u{2512}' => (L, N, N, H), // ┒
        '\u{2513}' => (H, N, N, H), // ┓
        '\u{2514}' => (N, L, L, N), // └
        '\u{2515}' => (N, H, L, N), // ┕
        '\u{2516}' => (N, L, H, N), // ┖
        '\u{2517}' => (N, H, H, N), // ┗
        '\u{2518}' => (L, N, L, N), // ┘
        '\u{2519}' => (H, N, L, N), // ┙
        '\u{251A}' => (L, N, H, N), // ┚
        '\u{251B}' => (H, N, H, N), // ┛
        '\u{251C}' => (N, L, L, L), // ├
        '\u{251D}' => (N, H, L, L), // ┝
        '\u{251E}' => (N, L, H, L), // ┞
        '\u{251F}' => (N, L, L, H), // ┟
        '\u{2520}' => (N, L, H, H), // ┠
        '\u{2521}' => (N, H, H, L), // ┡
        '\u{2522}' => (N, H, L, H), // ┢
        '\u{2523}' => (N, H, H, H), // ┣
        '\u{2524}' => (L, N, L, L), // ┤
        '\u{2525}' => (H, N, L, L), // ┥
        '\u{2526}' => (L, N, H, L), // ┦
        '\u{2527}' => (L, N, L, H), // ┧
        '\u{2528}' => (L, N, H, H), // ┨
        '\u{2529}' => (H, N, H, L), // ┩
        '\u{252A}' => (H, N, L, H), // ┪
        '\u{252B}' => (H, N, H, H), // ┫
        '\u{252C}' => (L, L, N, L), // ┬
        '\u{252D}' => (H, L, N, L), // ┭
        '\u{252E}' => (L, H, N, L), // ┮
        '\u{252F}' => (H, H, N, L), // ┯
        '\u{2530}' => (L, L, N, H), // ┰
        '\u{2531}' => (H, L, N, H), // ┱
        '\u{2532}' => (L, H, N, H), // ┲
        '\u{2533}' => (H, H, N, H), // ┳
        '\u{2534}' => (L, L, L, N), // ┴
        '\u{2535}' => (H, L, L, N), // ┵
        '\u{2536}' => (L, H, L, N), // ┶
        '\u{2537}' => (H, H, L, N), // ┷
        '\u{2538}' => (L, L, H, N), // ┸
        '\u{2539}' => (H, L, H, N), // ┹
        '\u{253A}' => (L, H, H, N), // ┺
        '\u{253B}' => (H, H, H, N), // ┻
        '\u{253C}' => (L, L, L, L), // ┼
        '\u{253D}' => (H, L, L, L), // ┽
        '\u{253E}' => (L, H, L, L), // ┾
        '\u{253F}' => (H, H, L, L), // ┿
        '\u{2540}' => (L, L, H, L), // ╀
        '\u{2541}' => (L, L, L, H), // ╁
        '\u{2542}' => (L, L, H, H), // ╂
        '\u{2543}' => (H, L, H, L), // ╃
        '\u{2544}' => (L, H, H, L), // ╄
        '\u{2545}' => (H, L, L, H), // ╅
        '\u{2546}' => (L, H, L, H), // ╆
        '\u{2547}' => (H, H, H, L), // ╇
        '\u{2548}' => (H, H, L, H), // ╈
        '\u{2549}' => (H, L, H, H), // ╉
        '\u{254A}' => (L, H, H, H), // ╊
        '\u{254B}' => (H, H, H, H), // ╋
        '\u{2550}' => (D, D, N, N), // ═
        '\u{2551}' => (N, N, D, D), // ║
        '\u{2552}' => (N, D, N, L), // ╒
        '\u{2553}' => (N, L, N, D), // ╓
        '\u{2554}' => (N, D, N, D), // ╔
        '\u{2555}' => (D, N, N, L), // ╕
        '\u{2556}' => (L, N, N, D), // ╖
        '\u{2557}' => (D, N, N, D), // ╗
        '\u{2558}' => (N, D, L, N), // ╘
        '\u{2559}' => (N, L, D, N), // ╙
        '\u{255A}' => (N, D, D, N), // ╚
        '\u{255B}' => (D, N, L, N), // ╛
        '\u{255C}' => (L, N, D, N), // ╜
        '\u{255D}' => (D, N, D, N), // ╝
        '\u{255E}' => (N, D, L, L), // ╞
        '\u{255F}' => (N, L, D, D), // ╟
        '\u{2560}' => (N, D, D, D), // ╠
        '\u{2561}' => (D, N, L, L), // ╡
        '\u{2562}' => (L, N, D, D), // ╢
        '\u{2563}' => (D, N, D, D), // ╣
        '\u{2564}' => (D, D, N, L), // ╤
        '\u{2565}' => (L, L, N, D), // ╥
        '\u{2566}' => (D, D, N, D), // ╦
        '\u{2567}' => (D, D, L, N), // ╧
        '\u{2568}' => (L, L, D, N), // ╨
        '\u{2569}' => (D, D, D, N), // ╩
        '\u{256A}' => (D, D, L, L), // ╪
        '\u{256B}' => (L, L, D, D), // ╫
        '\u{256C}' => (D, D, D, D), // ╬
        '\u{2574}' => (L, N, N, N), // ╴
        '\u{2575}' => (N, N, L, N), // ╵
        '\u{2576}' => (N, L, N, N), // ╶
        '\u{2577}' => (N, N, N, L), // ╷
        '\u{2578}' => (H, N, N, N), // ╸
        '\u{2579}' => (N, N, H, N), // ╹
        '\u{257A}' => (N, H, N, N), // ╺
        '\u{257B}' => (N, N, N, H), // ╻
        '\u{257C}' => (L, H, N, N), // ╼
        '\u{257D}' => (N, N, L, H), // ╽
        '\u{257E}' => (H, L, N, N), // ╾
        '\u{257F}' => (N, N, H, L), // ╿
        _ => return None,
    };
    Some(arms)
}

/// Draw any Unicode box-drawing glyph (`U+2500..=U+257F`) as crisp vector shapes
/// that join across adjacent cells without sub-pixel gaps.
fn draw_box_drawing(painter: &Rec<'_>, c: char, cell: Rect, fg: Color32) {
    let cw = cell.width();
    let ch = cell.height();
    let cx = cell.center().x;
    let cy = cell.center().y;
    let lw = (cw * 0.13).clamp(1.0, 2.0).round();
    let hw = (lw * 2.0).min(cw * 0.35).round().max(lw + 1.0);
    let gap = (cw * 0.24).clamp(2.0, 4.0).round();
    // Slight bleed past cell borders so neighboring box cells join seamlessly.
    let pad = 0.25;
    let x0 = cell.left() - pad;
    let x1 = cell.right() + pad;
    let y0 = cell.top() - pad;
    let y1 = cell.bottom() + pad;

    // 1. Dashed lines (2-, 3-, 4-dash light & heavy).
    let dash = match c {
        '\u{2504}' => Some((true, 3, lw)),  // ┄
        '\u{2505}' => Some((true, 3, hw)),  // ┅
        '\u{2506}' => Some((false, 3, lw)), // ┆
        '\u{2507}' => Some((false, 3, hw)), // ┇
        '\u{2508}' => Some((true, 4, lw)),  // ┈
        '\u{2509}' => Some((true, 4, hw)),  // ┉
        '\u{250A}' => Some((false, 4, lw)), // ┊
        '\u{250B}' => Some((false, 4, hw)), // ┋
        '\u{254C}' => Some((true, 2, lw)),  // ╌
        '\u{254D}' => Some((true, 2, hw)),  // ╍
        '\u{254E}' => Some((false, 2, lw)), // ╎
        '\u{254F}' => Some((false, 2, hw)), // ╏
        _ => None,
    };
    if let Some((horiz, count, thick)) = dash {
        let half = thick * 0.5;
        let step = (if horiz { cw } else { ch }) / count as f32;
        let len = (step * 0.62).max(1.0);
        for i in 0..count {
            let start = i as f32 * step + (step - len) * 0.5;
            let r = if horiz {
                Rect::from_min_max(Pos2::new(cell.left() + start, cy - half), Pos2::new(cell.left() + start + len, cy + half))
            } else {
                Rect::from_min_max(Pos2::new(cx - half, cell.top() + start), Pos2::new(cx + half, cell.top() + start + len))
            };
            painter.rect_filled(r, 0.0, fg);
        }
        return;
    }

    // 2. Rounded corners (╭ ╮ ╯ ╰).
    if matches!(c, '\u{256D}'..='\u{2570}') {
        let rad = (cw * 0.5).min(ch * 0.5);
        let stroke = Stroke::new(lw, fg);
        // Quarter-circle arc + straight extensions to the cell edges.
        let (arc_center, a0, a1, p_start, p_end) = match c {
            '\u{256D}' => (
                // ╭ right + down
                Pos2::new(cx + rad, cy + rad),
                std::f32::consts::PI,
                1.5 * std::f32::consts::PI,
                Pos2::new(cx, y1),
                Pos2::new(x1, cy),
            ),
            '\u{256E}' => (
                // ╮ left + down
                Pos2::new(cx - rad, cy + rad),
                1.5 * std::f32::consts::PI,
                std::f32::consts::TAU,
                Pos2::new(x0, cy),
                Pos2::new(cx, y1),
            ),
            '\u{256F}' => (
                // ╯ left + up
                Pos2::new(cx - rad, cy - rad),
                0.0,
                0.5 * std::f32::consts::PI,
                Pos2::new(cx, y0),
                Pos2::new(x0, cy),
            ),
            _ => (
                // ╰ right + up
                Pos2::new(cx + rad, cy - rad),
                0.5 * std::f32::consts::PI,
                std::f32::consts::PI,
                Pos2::new(x1, cy),
                Pos2::new(cx, y0),
            ),
        };
        const STEPS: usize = 6;
        let mut pts = Vec::with_capacity(STEPS + 3);
        pts.push(p_start);
        for i in 0..=STEPS {
            let t = i as f32 / STEPS as f32;
            let a = a0 + (a1 - a0) * t;
            pts.push(Pos2::new(arc_center.x + a.cos() * rad, arc_center.y + a.sin() * rad));
        }
        pts.push(p_end);
        painter.polyline(pts, stroke);
        return;
    }

    // 3. Diagonals (╱ ╲ ╳).
    if matches!(c, '\u{2571}'..='\u{2573}') {
        let stroke = Stroke::new(lw, fg);
        if c == '\u{2571}' || c == '\u{2573}' {
            painter.line_segment([Pos2::new(x0, y1), Pos2::new(x1, y0)], stroke);
        }
        if c == '\u{2572}' || c == '\u{2573}' {
            painter.line_segment([Pos2::new(x0, y0), Pos2::new(x1, y1)], stroke);
        }
        return;
    }

    // 4. Standard cardinal arms (light / heavy / double).
    let Some((left, right, up, down)) = box_arms(c) else { return };
    let half_w = |a: Arm| match a {
        Arm::Heavy => hw * 0.5,
        Arm::Light | Arm::Double => lw * 0.5,
        Arm::None => 0.0,
    };
    let h_off = gap * 0.5;
    let v_off = gap * 0.5;
    let v_has_double = up == Arm::Double || down == Arm::Double;
    let h_has_double = left == Arm::Double || right == Arm::Double;

    // Junction extent so single/heavy arms meet cleanly at the center without notches.
    let v_half = half_w(up).max(half_w(down)).max(if v_has_double { v_off + lw * 0.5 } else { 0.0 });
    let h_half = half_w(left).max(half_w(right)).max(if h_has_double { h_off + lw * 0.5 } else { 0.0 });

    let rect_h = |a: f32, b: f32, y: f32, half: f32| {
        painter.rect_filled(Rect::from_min_max(Pos2::new(a.min(b), y - half), Pos2::new(a.max(b), y + half)), 0.0, fg);
    };
    let rect_v = |x: f32, a: f32, b: f32, half: f32| {
        painter.rect_filled(Rect::from_min_max(Pos2::new(x - half, a.min(b)), Pos2::new(x + half, a.max(b))), 0.0, fg);
    };

    // Horizontal arms
    match left {
        Arm::Light | Arm::Heavy => {
            let end = if v_has_double && right == Arm::None { cx - v_off + lw * 0.5 } else { cx + v_half };
            rect_h(x0, end, cy, half_w(left));
        }
        Arm::Double => {
            let top_end = if up == Arm::Double {
                cx - v_off + lw * 0.5
            } else if down == Arm::Double && right == Arm::None {
                cx + v_off + lw * 0.5
            } else {
                cx + lw * 0.5
            };
            let bot_end = if down == Arm::Double {
                cx - v_off + lw * 0.5
            } else if up == Arm::Double && right == Arm::None {
                cx + v_off + lw * 0.5
            } else {
                cx + lw * 0.5
            };
            rect_h(x0, top_end, cy - h_off, lw * 0.5);
            rect_h(x0, bot_end, cy + h_off, lw * 0.5);
        }
        Arm::None => {}
    }
    match right {
        Arm::Light | Arm::Heavy => {
            let start = if v_has_double && left == Arm::None { cx + v_off - lw * 0.5 } else { cx - v_half };
            rect_h(start, x1, cy, half_w(right));
        }
        Arm::Double => {
            let top_start = if up == Arm::Double {
                cx + v_off - lw * 0.5
            } else if down == Arm::Double && left == Arm::None {
                cx - v_off - lw * 0.5
            } else {
                cx - lw * 0.5
            };
            let bot_start = if down == Arm::Double {
                cx + v_off - lw * 0.5
            } else if up == Arm::Double && left == Arm::None {
                cx - v_off - lw * 0.5
            } else {
                cx - lw * 0.5
            };
            rect_h(top_start, x1, cy - h_off, lw * 0.5);
            rect_h(bot_start, x1, cy + h_off, lw * 0.5);
        }
        Arm::None => {}
    }

    // Vertical arms
    match up {
        Arm::Light | Arm::Heavy => {
            let end = if h_has_double && down == Arm::None { cy - h_off + lw * 0.5 } else { cy + h_half };
            rect_v(cx, y0, end, half_w(up));
        }
        Arm::Double => {
            let left_end = if left == Arm::Double {
                cy - h_off + lw * 0.5
            } else if right == Arm::Double && down == Arm::None {
                cy + h_off + lw * 0.5
            } else {
                cy + lw * 0.5
            };
            let right_end = if right == Arm::Double {
                cy - h_off + lw * 0.5
            } else if left == Arm::Double && down == Arm::None {
                cy + h_off + lw * 0.5
            } else {
                cy + lw * 0.5
            };
            rect_v(cx - v_off, y0, left_end, lw * 0.5);
            rect_v(cx + v_off, y0, right_end, lw * 0.5);
        }
        Arm::None => {}
    }
    match down {
        Arm::Light | Arm::Heavy => {
            let start = if h_has_double && up == Arm::None { cy + h_off - lw * 0.5 } else { cy - h_half };
            rect_v(cx, start, y1, half_w(down));
        }
        Arm::Double => {
            let left_start = if left == Arm::Double {
                cy + h_off - lw * 0.5
            } else if right == Arm::Double && up == Arm::None {
                cy - h_off - lw * 0.5
            } else {
                cy - lw * 0.5
            };
            let right_start = if right == Arm::Double {
                cy + h_off - lw * 0.5
            } else if left == Arm::Double && up == Arm::None {
                cy - h_off - lw * 0.5
            } else {
                cy - lw * 0.5
            };
            rect_v(cx - v_off, left_start, y1, lw * 0.5);
            rect_v(cx + v_off, right_start, y1, lw * 0.5);
        }
        Arm::None => {}
    }
}

type Frac = (f32, f32, f32, f32);

/// Unicode block elements (U+2580..U+259F) as fractional rects of the cell,
/// plus an opacity for the shade characters.
fn block_rects(c: char) -> Option<(Vec<Frac>, f32)> {
    const TL: Frac = (0.0, 0.0, 0.5, 0.5);
    const TR: Frac = (0.5, 0.0, 1.0, 0.5);
    const BL: Frac = (0.0, 0.5, 0.5, 1.0);
    const BR: Frac = (0.5, 0.5, 1.0, 1.0);
    let full = vec![(0.0, 0.0, 1.0, 1.0)];
    let r = match c {
        '\u{2580}' => (vec![(0.0, 0.0, 1.0, 0.5)], 1.0), // ▀
        '\u{2581}'..='\u{2587}' => {
            // ▁..▇ lower eighths
            let n = (c as u32 - 0x2580) as f32;
            (vec![(0.0, 1.0 - n / 8.0, 1.0, 1.0)], 1.0)
        }
        '\u{2588}' => (full, 1.0), // █
        '\u{2589}'..='\u{258F}' => {
            // ▉..▏ left eighths
            let n = (0x2590 - c as u32) as f32;
            (vec![(0.0, 0.0, n / 8.0, 1.0)], 1.0)
        }
        '\u{2590}' => (vec![(0.5, 0.0, 1.0, 1.0)], 1.0), // ▐
        '\u{2591}' => (full, 0.25),                      // ░
        '\u{2592}' => (full, 0.5),                       // ▒
        '\u{2593}' => (full, 0.75),                      // ▓
        '\u{2594}' => (vec![(0.0, 0.0, 1.0, 0.125)], 1.0), // ▔
        '\u{2595}' => (vec![(0.875, 0.0, 1.0, 1.0)], 1.0), // ▕
        '\u{2596}' => (vec![BL], 1.0),
        '\u{2597}' => (vec![BR], 1.0),
        '\u{2598}' => (vec![TL], 1.0),
        '\u{2599}' => (vec![TL, BL, BR], 1.0),
        '\u{259A}' => (vec![TL, BR], 1.0),
        '\u{259B}' => (vec![TL, TR, BL], 1.0),
        '\u{259C}' => (vec![TL, TR, BR], 1.0),
        '\u{259D}' => (vec![TR], 1.0),
        '\u{259E}' => (vec![TR, BL], 1.0),
        '\u{259F}' => (vec![TR, BL, BR], 1.0),
        _ => return None,
    };
    Some(r)
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
        Key::Space if m.ctrl && !m.alt => vec![0x00],
        Key::Space if m.alt && !m.ctrl => b"\x1b ".to_vec(),
        // Ctrl+punctuation control codes (ESC, FS, GS, RS, US, NUL).
        Key::OpenBracket if m.ctrl && !m.alt => vec![0x1b],
        Key::Backslash if m.ctrl && !m.alt => vec![0x1c],
        Key::CloseBracket if m.ctrl && !m.alt => vec![0x1d],
        Key::Num6 if m.ctrl && !m.alt => vec![0x1e],
        Key::Slash if m.ctrl && !m.alt => vec![0x1f],
        Key::Minus if m.ctrl && !m.alt => vec![0x1f],
        Key::Num2 if m.ctrl && !m.alt => vec![0x00],
        _ => {
            if m.alt && !m.ctrl {
                if let Some((unshifted, shifted)) = alt_symbol(key) {
                    let b = if m.shift { shifted } else { unshifted };
                    return Some(vec![0x1b, b]);
                }
            }
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

/// ASCII pair `(unshifted, shifted)` for digits and punctuation with Alt.
fn alt_symbol(key: Key) -> Option<(u8, u8)> {
    Some(match key {
        Key::Num0 => (b'0', b')'),
        Key::Num1 => (b'1', b'!'),
        Key::Num2 => (b'2', b'@'),
        Key::Num3 => (b'3', b'#'),
        Key::Num4 => (b'4', b'$'),
        Key::Num5 => (b'5', b'%'),
        Key::Num6 => (b'6', b'^'),
        Key::Num7 => (b'7', b'&'),
        Key::Num8 => (b'8', b'*'),
        Key::Num9 => (b'9', b'('),
        Key::Period => (b'.', b'>'),
        Key::Comma => (b',', b'<'),
        Key::Slash => (b'/', b'?'),
        Key::Backslash => (b'\\', b'|'),
        Key::OpenBracket => (b'[', b'{'),
        Key::CloseBracket => (b']', b'}'),
        Key::Semicolon => (b';', b':'),
        Key::Quote => (b'\'', b'"'),
        Key::Backtick => (b'`', b'~'),
        Key::Minus => (b'-', b'_'),
        Key::Equals => (b'=', b'+'),
        Key::Plus => (b'+', b'+'),
        _ => return None,
    })
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
        // Ctrl+punctuation & Alt+symbols
        assert_eq!(key_bytes(Key::OpenBracket, Modifiers::CTRL, false), Some(vec![0x1b]));
        assert_eq!(key_bytes(Key::Backslash, Modifiers::CTRL, false), Some(vec![0x1c]));
        assert_eq!(key_bytes(Key::CloseBracket, Modifiers::CTRL, false), Some(vec![0x1d]));
        assert_eq!(key_bytes(Key::Num6, Modifiers::CTRL, false), Some(vec![0x1e]));
        assert_eq!(key_bytes(Key::Slash, Modifiers::CTRL, false), Some(vec![0x1f]));
        assert_eq!(key_bytes(Key::Minus, Modifiers::CTRL | Modifiers::SHIFT, false), Some(vec![0x1f]));
        assert_eq!(key_bytes(Key::Num2, Modifiers::CTRL, false), Some(vec![0x00]));
        assert_eq!(key_bytes(Key::Period, Modifiers::ALT, false), Some(b"\x1b.".to_vec()));
        assert_eq!(key_bytes(Key::Period, Modifiers::ALT | Modifiers::SHIFT, false), Some(b"\x1b>".to_vec()));
    }

    #[test]
    fn dropped_paths_quoting() {
        let p = vec![PathBuf::from(r"C:\src\main.rs"), PathBuf::from(r"C:\My Docs\a b.txt")];
        assert_eq!(format_dropped_paths(&p), r#"C:\src\main.rs "C:\My Docs\a b.txt""#);
    }

    #[test]
    fn box_drawing_covers_full_unicode_block() {
        let ctx = egui::Context::default();
        let cell = Rect::from_min_size(Pos2::ZERO, Vec2::new(8.0, 16.0));
        for cp in 0x2500u32..=0x257Fu32 {
            let c = char::from_u32(cp).unwrap();
            let rec = Rec { ctx: &ctx, shapes: Default::default() };
            draw_box_drawing(&rec, c, cell, Color32::WHITE);
            assert!(!rec.shapes.borrow().is_empty(), "missing vector box-drawing for U+{cp:04X} ({c})");
        }
    }
}

#[cfg(test)]
mod block_tests {
    use super::block_rects;

    #[test]
    fn blocks() {
        assert_eq!(block_rects('█').unwrap().0, vec![(0.0, 0.0, 1.0, 1.0)]);
        assert_eq!(block_rects('▀').unwrap().0, vec![(0.0, 0.0, 1.0, 0.5)]);
        assert_eq!(block_rects('▄').unwrap().0, vec![(0.0, 0.5, 1.0, 1.0)]);
        assert_eq!(block_rects('▌').unwrap().0, vec![(0.0, 0.0, 0.5, 1.0)]);
        assert_eq!(block_rects('▁').unwrap().0, vec![(0.0, 0.875, 1.0, 1.0)]);
        assert_eq!(block_rects('▏').unwrap().0, vec![(0.0, 0.0, 0.125, 1.0)]);
        assert_eq!(block_rects('▒').unwrap().1, 0.5);
        assert!(block_rects('a').is_none());
        assert!(block_rects('─').is_none());
    }
}

#[cfg(test)]
mod link_tests {
    use super::{link_at, strip_line_suffix};
    use std::path::Path;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn finds_urls() {
        let line = chars("see https://example.com/docs. for more");
        let (s, e, t) = link_at(&line, 10, Path::new(".")).unwrap();
        assert_eq!(t, "https://example.com/docs");
        assert_eq!((s, e), (4, 27));
        assert!(link_at(&line, 1, Path::new(".")).is_none());
    }

    #[test]
    fn finds_existing_paths_relative_to_cwd() {
        let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
        let line = chars("  ⎿ edited src/main.rs:12:5 ok");
        let (_, _, t) = link_at(&line, 14, cwd).unwrap();
        assert!(t.ends_with("main.rs"), "{t}");
        assert!(link_at(&chars("nope/missing.rs"), 3, cwd).is_none());
    }

    #[test]
    fn line_suffixes() {
        assert_eq!(strip_line_suffix("a.rs:12:5"), "a.rs");
        assert_eq!(strip_line_suffix(r"C:\x\a.rs:7"), r"C:\x\a.rs");
        assert_eq!(strip_line_suffix("C:"), "C:");
    }
}

#[cfg(test)]
mod clipboard_tests {
    /// Touches the real clipboard, so run by hand: `cargo test clipboard_image -- --ignored`.
    #[test]
    #[ignore]
    fn clipboard_image_becomes_png_path() {
        let mut cb = arboard::Clipboard::new().unwrap();
        let px = vec![255u8, 0, 0, 255].repeat(4 * 3);
        cb.set_image(arboard::ImageData { width: 4, height: 3, bytes: px.into() }).unwrap();
        drop(cb);
        let pasted = super::clipboard_attachment().expect("image pasted as a path");
        let p = std::path::PathBuf::from(pasted.trim_matches('"'));
        let img = image::open(&p).expect("valid png");
        assert_eq!((img.width(), img.height()), (4, 3));
        std::fs::remove_file(p).unwrap();
    }
}
