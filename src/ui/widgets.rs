//! Small reusable widgets and layout helpers.

use crate::*;

pub(crate) fn logo(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, ACCENT);
    p.rect_filled(rect.shrink(5.0), 2.0, PANEL);
}

pub(crate) fn segmented(ui: &mut egui::Ui, view: &mut View) {
    egui::Frame::new().fill(BG).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(6).inner_margin(Margin::same(2)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for (v, label) in [(View::Focus, "▣  Focus"), (View::Grid, "▦  Grid")] {
                let btn = Btn { text: label.into(), kind: BtnKind::Segment(*view == v) };
                if ui.add(btn).on_hover_text("Ctrl+Shift+G").clicked() {
                    *view = v;
                }
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BtnKind {
    Primary,
    Ghost,
    Danger,
    Icon,
    /// Focus/Grid toggle; `true` = the active one.
    Segment(bool),
}

/// Our button: smooth hover fade, pressed state, hand cursor.
pub(crate) struct Btn {
    text: String,
    kind: BtnKind,
}

pub(crate) fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

impl egui::Widget for Btn {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        use BtnKind::*;
        let enabled = ui.is_enabled();
        let (font, pad_x, h, radius, min_w) = match self.kind {
            Primary => (bold(12.5), 9.0, 24.0, 6.0, 0.0),
            Ghost | Danger => (egui::FontId::proportional(12.5), 9.0, 24.0, 6.0, 0.0),
            Icon => (egui::FontId::proportional(12.5), 4.0, 20.0, 4.0, 20.0),
            Segment(_) => (egui::FontId::proportional(12.0), 9.0, 20.0, 5.0, 58.0),
        };
        let galley = ui.painter().layout_no_wrap(self.text, font, Color32::PLACEHOLDER);
        let size = Vec2::new((galley.size().x + pad_x * 2.0).max(min_w), h);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let hovered = resp.hovered() && enabled;
        let t = ui.ctx().animate_bool_with_time(resp.id.with("hover"), hovered, 0.12);
        let lp = |a: Color32, b: Color32| lerp_color(a, b, t);
        let clear = Color32::TRANSPARENT;
        let accent_hi = Color32::from_rgb(0x95, 0xA3, 0xFF);
        let red_hi = Color32::from_rgb(0xF0, 0x6E, 0x6E);
        let (mut fill, stroke, mut fg) = match self.kind {
            Primary => (lp(ACCENT.gamma_multiply(0.9), accent_hi), Stroke::NONE, Color32::WHITE),
            Ghost => (lp(RAISED, HOVER), Stroke::new(1.0_f32, lp(BORDER, Color32::from_rgb(0x3E, 0x46, 0x5E))), lp(TEXT, Color32::WHITE)),
            Danger => (lp(RED.gamma_multiply(0.75), red_hi), Stroke::NONE, Color32::WHITE),
            Icon => (lp(clear, HOVER), Stroke::NONE, lp(MUTED, TEXT)),
            Segment(true) => (RAISED, Stroke::NONE, TEXT),
            Segment(false) => (lp(clear, HOVER.gamma_multiply(0.7)), Stroke::NONE, lp(MUTED, TEXT)),
        };
        if resp.is_pointer_button_down_on() && enabled {
            fill = lerp_color(fill, Color32::BLACK, 0.18);
        }
        if !enabled {
            fill = fill.gamma_multiply(0.45);
            fg = fg.gamma_multiply(0.45);
        }
        let painter = ui.painter();
        painter.rect_filled(rect, radius, fill);
        if stroke.width > 0.0 {
            painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
        }
        let pos = rect.center() - galley.size() / 2.0;
        painter.galley(pos, galley, fg);
        if enabled {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

pub(crate) fn primary_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Primary }
}

pub(crate) fn ghost_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Ghost }
}

pub(crate) fn danger_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Danger }
}

pub(crate) fn icon_button(text: &str) -> Btn {
    Btn { text: text.into(), kind: BtnKind::Icon }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum WinBtn {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Title-bar window button, full toolbar height, drawn as crisp shapes.
pub(crate) fn window_button(ui: &mut egui::Ui, kind: WinBtn) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(42.0, ui.available_height()), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.10);
    let hot = if kind == WinBtn::Close { Color32::from_rgb(0xC4, 0x2B, 0x1C) } else { HOVER };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, lerp_color(Color32::TRANSPARENT, hot, t));
    let fg = lerp_color(MUTED, Color32::WHITE, t);
    let s = Stroke::new(1.0_f32, fg);
    let c = rect.center();
    let h = 5.0;
    match kind {
        WinBtn::Minimize => {
            painter.hline(c.x - h..=c.x + h, c.y + 0.5, s);
        }
        WinBtn::Maximize => {
            painter.rect_stroke(egui::Rect::from_center_size(c, Vec2::splat(2.0 * h)), 1.0, s, egui::StrokeKind::Inside);
        }
        WinBtn::Restore => {
            let back = egui::Rect::from_center_size(c + Vec2::new(2.0, -2.0), Vec2::splat(2.0 * h - 2.0));
            let front = egui::Rect::from_center_size(c + Vec2::new(-1.0, 1.0), Vec2::splat(2.0 * h - 2.0));
            painter.rect_stroke(back, 1.0, s, egui::StrokeKind::Inside);
            painter.rect_filled(front, 1.0, PANEL.gamma_multiply(1.0 - t));
            painter.rect_stroke(front, 1.0, s, egui::StrokeKind::Inside);
        }
        WinBtn::Close => {
            painter.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], s);
            painter.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], s);
        }
    }
    resp
}

/// Without native decorations, resize from a thin band along the window edges.
pub(crate) fn window_edges(ctx: &egui::Context) {
    use egui::{CursorIcon as C, ResizeDirection as D};
    let (maximized, pos, pressed) =
        ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.pointer.hover_pos(), i.pointer.primary_pressed()));
    let screen = ctx.screen_rect();
    if !maximized {
        // Hairline so the window edge is visible on any background.
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("frame")))
            .rect_stroke(screen, 0.0, Stroke::new(1.0_f32, BORDER), egui::StrokeKind::Inside);
    }
    let Some(p) = pos else { return };
    if maximized {
        return;
    }
    // Keep the top-right window controls (Minimize / Maximize / Close) unoccluded
    // so clicking the top-right corner of Close never starts a resize instead.
    if p.x >= screen.right() - 126.0 && p.y <= screen.top() + 32.0 {
        return;
    }
    const EDGE: f32 = 5.0;
    let (l, r) = (p.x - screen.left() < EDGE, screen.right() - p.x < EDGE);
    let (t, b) = (p.y - screen.top() < EDGE, screen.bottom() - p.y < EDGE);
    let dir = match (l, r, t, b) {
        (true, _, true, _) => Some((D::NorthWest, C::ResizeNorthWest)),
        (_, true, true, _) => Some((D::NorthEast, C::ResizeNorthEast)),
        (true, _, _, true) => Some((D::SouthWest, C::ResizeSouthWest)),
        (_, true, _, true) => Some((D::SouthEast, C::ResizeSouthEast)),
        (true, ..) => Some((D::West, C::ResizeWest)),
        (_, true, ..) => Some((D::East, C::ResizeEast)),
        (_, _, true, _) => Some((D::North, C::ResizeNorth)),
        (.., true) => Some((D::South, C::ResizeSouth)),
        _ => None,
    };
    if let Some((d, cursor)) = dir {
        ctx.set_cursor_icon(cursor);
        if pressed {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(d));
        }
    }
}

/// Status as a dot (plus label if `label`), or a small pulse while working.
/// Small amber "?" disc: the agent is waiting on your answer.
pub(crate) fn question_badge(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(r.center(), 6.5, AMBER);
    painter.text(r.center() + Vec2::new(0.0, 0.5), egui::Align2::CENTER_CENTER, "?", bold(10.0), Color32::from_rgb(0x1A, 0x14, 0x05));
    resp
}

/// Status as a dot (plus label if `label`), a pulse while working, or a "?"
/// badge when waiting on you. Hover shows how long it has been in that state.
pub(crate) fn status_indicator(ui: &mut egui::Ui, s: Status, label: bool, age: Option<Duration>) {
    let tip = match age {
        Some(d) if s.alive() => format!("{} for {}", s.label(), fmt_dur(d)),
        _ => s.label(),
    };
    let mark = match s {
        Status::Working => working_pulse(ui),
        Status::Asking => question_badge(ui),
        _ => status_dot(ui, s),
    };
    mark.on_hover_text(&tip);
    if label && s != Status::Working {
        ui.label(RichText::new(s.label()).small().color(s.color())).on_hover_text(tip);
    }
}

/// Three tiny dots pulsing in sequence.
pub(crate) fn working_pulse(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(15.0, 8.0), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let painter = ui.painter();
    for i in 0..3 {
        let k = ((t * 5.0 - i as f32 * 0.8).sin() + 1.0) / 2.0; // 0..1
        let center = egui::pos2(rect.left() + 2.5 + i as f32 * 5.0, rect.center().y - k);
        painter.circle_filled(center, 1.6, GREEN.gamma_multiply(0.3 + 0.7 * k));
    }
    // ~12 fps is plenty for three dots, and only while one is on screen.
    ui.ctx().request_repaint_after(Duration::from_millis(80));
    resp
}

pub(crate) fn status_dot(ui: &mut egui::Ui, s: Status) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
    ui.painter().circle_filled(r.center(), 3.0, s.color());
    resp
}



pub(crate) fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED).strong());
}

pub(crate) fn found_label(ui: &mut egui::Ui, p: &Option<PathBuf>) {
    match p {
        Some(p) => ui.label(RichText::new(format!("✓ {}", p.display())).small().color(GREEN)),
        None => ui.label(RichText::new("✗ Not found").small().color(RED)),
    };
}


pub(crate) fn provider_card(ui: &mut egui::Ui, p: Provider, sub: &str, selected: bool) -> egui::Response {
    let inner = egui::Frame::new()
        .fill(if selected { p.color().gamma_multiply(0.12) } else { RAISED })
        .stroke(Stroke::new(1.0_f32, if selected { p.color().gamma_multiply(0.7) } else { BORDER }))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, p.color());
                ui.label(RichText::new(p.label()).font(bold(14.0)));
            });
            ui.label(RichText::new(sub).small().color(MUTED));
        });
    ui.interact(inner.response.rect, egui::Id::new(("provider_card", p as u8)), Sense::click())
}

pub(crate) fn empty_state(ui: &mut egui::Ui, actions: &mut Vec<Action>, folder: &mut String) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 150.0).max(20.0));
        ui.label(RichText::new("Run your agents side by side").font(bold(22.0)));
        ui.add_space(2.0);
        ui.label(RichText::new("Pick an app to open your first agent's terminal.").color(MUTED));
        ui.add_space(16.0);
        ui.allocate_ui(Vec2::new(260.0, 0.0), |ui| {
            if let Some(p) = new_agent_menu(ui, folder) {
                actions.push(Action::Create(p));
            }
        });
    });
}

/// The "new agent" picker: one row per app (icon + name). Clicking a row
/// creates the agent and opens its terminal straight away.
pub(crate) fn new_agent_menu(ui: &mut egui::Ui, folder: &mut String) -> Option<Provider> {
    ui.set_width(260.0);
    let folder_ok = folder.trim().is_empty() || std::path::Path::new(folder.trim()).is_dir();
    let mut picked = None;
    for p in Provider::ALL {
        let sub = match p {
            Provider::Claude => "Anthropic's coding agent",
            Provider::OpenCode => "Open-source coding agent",
        };
        if app_row(ui, p, sub, folder_ok).clicked() && folder_ok {
            picked = Some(p);
        }
    }
    ui.add_space(4.0);
    ui.separator();
    ui.label(RichText::new("Opens in").small().color(MUTED));
    ui.add(
        egui::TextEdit::singleline(folder)
            .hint_text("home folder")
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(6, 3)),
    );
    if !folder_ok {
        ui.label(RichText::new("Folder not found").small().color(RED));
    }
    picked
}

pub(crate) fn app_row(ui: &mut egui::Ui, p: Provider, sub: &str, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    let painter = ui.painter();
    if resp.hovered() && enabled {
        painter.rect_filled(rect, 6.0, HOVER);
    }
    let icon = egui::Rect::from_min_size(rect.min + Vec2::new(6.0, 7.0), Vec2::splat(22.0));
    draw_app_icon(painter, icon, p);
    let x = icon.right() + 9.0;
    let name_color = if enabled { TEXT } else { FAINT };
    painter.text(egui::pos2(x, rect.top() + 4.0), egui::Align2::LEFT_TOP, p.label(), bold(12.5), name_color);
    painter.text(egui::pos2(x, rect.top() + 20.0), egui::Align2::LEFT_TOP, sub, egui::FontId::proportional(10.5), MUTED);
    if resp.hovered() && enabled {
        painter.text(
            egui::pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "Open ›",
            egui::FontId::proportional(11.5),
            p.color(),
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// App icon as a vector tile: Claude's orange spark, OpenCode's square "o".
pub(crate) fn draw_app_icon(painter: &egui::Painter, rect: egui::Rect, p: Provider) {
    let w = rect.width();
    let c = rect.center();
    match p {
        Provider::Claude => {
            painter.rect_filled(rect, w * 0.24, CLAUDE);
            let stroke = Stroke::new((w * 0.085).max(1.5), Color32::from_rgb(0xFF, 0xF4, 0xEE));
            for i in 0..10 {
                let a = i as f32 / 10.0 * std::f32::consts::TAU;
                let dir = Vec2::new(a.cos(), a.sin());
                let len = if i % 2 == 0 { 0.34 } else { 0.27 };
                painter.line_segment([c + dir * (w * 0.07), c + dir * (w * len)], stroke);
            }
        }
        Provider::OpenCode => {
            painter.rect_filled(rect, w * 0.24, Color32::from_rgb(0x1A, 0x1C, 0x22));
            painter.rect_stroke(rect, w * 0.24, Stroke::new(1.0_f32, Color32::from_rgb(0x3A, 0x3F, 0x4C)), egui::StrokeKind::Inside);
            let o = egui::Rect::from_center_size(c, Vec2::splat(w * 0.44));
            painter.rect_stroke(o, 0.0, Stroke::new((w * 0.1).max(1.5), Color32::WHITE), egui::StrokeKind::Inside);
            let inner = egui::Rect::from_min_max(egui::pos2(o.left(), o.center().y), o.max).shrink(w * 0.1);
            painter.rect_filled(inner, 0.0, Color32::from_gray(0x8A));
        }
    }
}

pub(crate) fn app_icon(ui: &mut egui::Ui, p: Provider, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    draw_app_icon(ui.painter(), rect, p);
}

pub(crate) fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}

/// Left/top edge of each grid track, given their fractions of `avail` space.
pub(crate) fn offsets(fracs: &[f32], avail: f32, gap: f32, start: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(fracs.len());
    let mut pos = start;
    for f in fracs {
        out.push(pos);
        pos += f * avail + gap;
    }
    out
}

/// Move the border between tracks `i` and `i + 1` by `delta` (a fraction of the
/// total), keeping both at least `MIN` of the space.
pub(crate) fn resize_pair(fracs: &mut [f32], i: usize, delta: f32) {
    const MIN: f32 = 0.12;
    let total = fracs[i] + fracs[i + 1];
    let a = (fracs[i] + delta).clamp(MIN, (total - MIN).max(MIN));
    fracs[i] = a;
    fracs[i + 1] = total - a;
}

/// "12.3k tok · ctx 34%", in reading order whichever way the layout runs.
pub(crate) fn pane_stats(ui: &mut egui::Ui, a: &Agent, context: Option<f32>, rtl: bool) {
    let mut items = vec![(RichText::new(format!("{} tok", fmt_tokens(a.session_usage.total_tokens()))).small().color(FAINT), None)];
    if let Some(p) = context {
        items.push((RichText::new("·").small().color(FAINT), None));
        items.push((
            RichText::new(format!("ctx {p:.0}%")).small().color(context_color(p)),
            Some("How full this conversation's context window is"),
        ));
    }
    if rtl {
        items.reverse();
    }
    for (text, tip) in items {
        let r = ui.add(egui::Label::new(text).selectable(false));
        if let Some(t) = tip {
            r.on_hover_text(t);
        }
    }
}

/// Second line of a sidebar card: "folder · model".
pub(crate) fn card_subtitle(a: &Agent) -> String {
    let cwd = a.cwd.trim().trim_end_matches(['\\', '/']);
    let folder = if cwd.is_empty() {
        "~".to_string()
    } else {
        std::path::Path::new(cwd).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| cwd.to_string())
    };
    let model = match a.provider {
        Provider::Claude if a.model.trim().is_empty() => "default model",
        Provider::Claude => a.model.trim(),
        Provider::OpenCode => "OpenCode",
    };
    format!("{folder} · {model}")
}

pub(crate) fn context_color(pct: f32) -> Color32 {
    if pct >= 85.0 {
        RED
    } else if pct >= 60.0 {
        AMBER
    } else {
        GREEN
    }
}

/// Hover card for an agent: where it runs and how.
pub(crate) fn agent_info(a: &Agent) -> String {
    let folder = if a.cwd.trim().is_empty() { "home folder" } else { a.cwd.trim() };
    let model = match a.provider {
        Provider::OpenCode => "picked in OpenCode (/models)",
        Provider::Claude if a.model.trim().is_empty() => "default",
        Provider::Claude => a.model.trim(),
    };
    let session = a.session_id.as_deref().map(|s| s.chars().take(13).collect::<String>() + "…").unwrap_or_else(|| "new".into());
    format!(
        "{}\nFolder: {folder}\nModel: {model}\nPermissions: {}\nSession: {session}",
        a.provider.label(),
        a.permission.label()
    )
}

/// Count of new output lines on an agent you're not looking at.
pub(crate) fn unread_badge(ui: &mut egui::Ui, n: usize) {
    if n == 0 {
        return;
    }
    let text = if n > 99 { "99+".to_string() } else { n.to_string() };
    let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(10.0), Color32::WHITE);
    let size = Vec2::new((galley.size().x + 8.0).max(16.0), 14.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 7.0, ACCENT.gamma_multiply(0.85));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp.on_hover_text(format!("{n} new line{} since you last looked", if n == 1 { "" } else { "s" }));
}

/// "42s", "2m 14s", "1h 5m".
pub(crate) fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn resize_keeps_total_and_minimum() {
        let mut f = vec![0.5, 0.5];
        resize_pair(&mut f, 0, 0.2);
        assert!((f[0] - 0.7).abs() < 1e-6 && (f[0] + f[1] - 1.0).abs() < 1e-6);
        resize_pair(&mut f, 0, 0.9);
        assert!((f[1] - 0.12).abs() < 1e-6);
    }

    #[test]
    fn offsets_include_gaps() {
        assert_eq!(offsets(&[0.5, 0.5], 100.0, 6.0, 10.0), vec![10.0, 66.0]);
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_dur(Duration::from_secs(42)), "42s");
        assert_eq!(fmt_dur(Duration::from_secs(134)), "2m 14s");
        assert_eq!(fmt_dur(Duration::from_secs(3900)), "1h 5m");
    }
}
