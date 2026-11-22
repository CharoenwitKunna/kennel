//! Colors, fonts and egui styling.

use alacritty_terminal::vte::ansi::Rgb;
use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x0E, 0x10, 0x14);
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x1A, 0x24);
pub const RAISED: Color32 = Color32::from_rgb(0x1B, 0x1F, 0x2B);
pub const HOVER: Color32 = Color32::from_rgb(0x23, 0x28, 0x37);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2B, 0x3A);
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE8, 0xEE);
pub const MUTED: Color32 = Color32::from_rgb(0x8A, 0x90, 0xA2);
pub const FAINT: Color32 = Color32::from_rgb(0x5A, 0x60, 0x72);

pub const ACCENT: Color32 = Color32::from_rgb(0x7C, 0x8C, 0xFF);
pub const CLAUDE: Color32 = Color32::from_rgb(0xD9, 0x77, 0x57);
pub const OPENCODE: Color32 = Color32::from_rgb(0x4F, 0x9C, 0xF7);

pub const GREEN: Color32 = Color32::from_rgb(0x4A, 0xC2, 0x7D);
pub const AMBER: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x4A);
pub const RED: Color32 = Color32::from_rgb(0xE5, 0x5B, 0x5B);
pub const GREY: Color32 = Color32::from_rgb(0x6B, 0x72, 0x85);

/// Terminal background (slightly darker than the app background).
pub const TERM_BG: Color32 = Color32::from_rgb(0x08, 0x09, 0x0C);
pub const TERM_FG: Color32 = Color32::from_rgb(0xD8, 0xDC, 0xE6);

/// 16-color ANSI palette (One Dark-ish).
const ANSI: [[u8; 3]; 16] = [
    [0x1C, 0x1F, 0x26],
    [0xE0, 0x6C, 0x75],
    [0x98, 0xC3, 0x79],
    [0xE5, 0xC0, 0x7B],
    [0x61, 0xAF, 0xEF],
    [0xC6, 0x78, 0xDD],
    [0x56, 0xB6, 0xC2],
    [0xAB, 0xB2, 0xBF],
    [0x5C, 0x63, 0x70],
    [0xF0, 0x7F, 0x88],
    [0xB5, 0xE0, 0x90],
    [0xF5, 0xD0, 0x8C],
    [0x7C, 0xC4, 0xFF],
    [0xDA, 0x8E, 0xF0],
    [0x6F, 0xD3, 0xDE],
    [0xF0, 0xF2, 0xF5],
];

pub const BOLD: &str = "ui-bold";

/// Color for an xterm palette index 0..=268 (alacritty's `NamedColor` numbering).
pub fn term_color_rgb(idx: usize) -> Rgb {
    let c = term_color(idx);
    Rgb { r: c.r(), g: c.g(), b: c.b() }
}

pub fn term_color(idx: usize) -> Color32 {
    match idx {
        0..=15 => {
            let [r, g, b] = ANSI[idx];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            const V: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let i = idx - 16;
            Color32::from_rgb(V[i / 36], V[(i / 6) % 6], V[i % 6])
        }
        232..=255 => {
            let v = (8 + 10 * (idx - 232)) as u8;
            Color32::from_gray(v)
        }
        256 => TERM_FG,                          // Foreground
        257 => TERM_BG,                          // Background
        258 => TERM_FG,                          // Cursor
        259..=266 => dim(term_color(idx - 259)), // Dim colors
        267 => Color32::WHITE,                   // BrightForeground
        268 => dim(TERM_FG),                     // DimForeground
        _ => TERM_FG,
    }
}

pub fn dim(c: Color32) -> Color32 {
    Color32::from_rgb((c.r() as f32 * 0.66) as u8, (c.g() as f32 * 0.66) as u8, (c.b() as f32 * 0.66) as u8)
}

fn load(path: &str) -> Option<Arc<FontData>> {
    std::fs::read(path).ok().map(|b| Arc::new(FontData::from_owned(b)))
}

/// Set when Thai text shows up somewhere, so its font gets loaded (it isn't by default).
pub static WANT_THAI: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn is_thai(c: char) -> bool {
    ('\u{0E00}'..='\u{0E7F}').contains(&c)
}

pub fn has_thai(s: &str) -> bool {
    s.chars().any(is_thai)
}

/// Use Windows system fonts (nothing bundled): Segoe UI for the UI, Cascadia Mono /
/// Consolas for terminals, plus Segoe UI Symbol as a fallback. Leelawadee UI (Thai)
/// is only loaded when `thai` is set, to keep memory down for everyone else.
pub fn install_fonts(ctx: &egui::Context, thai: bool) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
    let mut add = |name: &str, files: &[&str]| -> bool {
        for f in files {
            if let Some(data) = load(&format!("{dir}{f}")) {
                fonts.font_data.insert(name.to_owned(), data);
                return true;
            }
        }
        false
    };
    let ui = add("ui", &["segoeui.ttf"]);
    let ui_bold = add("ui-bold", &["segoeuib.ttf", "segoeui.ttf"]);
    let mono = add("mono", &["CascadiaMono.ttf", "consola.ttf"]);
    let sym = add("sym", &["seguisym.ttf"]);
    let thai = thai && add("thai", &["LeelawUI.ttf"]);

    let fallbacks: Vec<String> =
        [(sym, "sym"), (thai, "thai")].iter().filter(|(ok, _)| *ok).map(|(_, n)| n.to_string()).collect();

    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    if ui {
        prop.insert(0, "ui".into());
    }
    prop.extend(fallbacks.iter().cloned());
    let monof = fonts.families.entry(FontFamily::Monospace).or_default();
    if mono {
        monof.insert(0, "mono".into());
    }
    monof.extend(fallbacks.iter().cloned());

    let mut bold = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if ui_bold {
        bold.insert(0, "ui-bold".into());
    }
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold);
    ctx.set_fonts(fonts);
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    use FontFamily::*;
    style.text_styles = [
        (TextStyle::Heading, FontId::new(17.0, Name(BOLD.into()))),
        (TextStyle::Body, FontId::new(13.0, Proportional)),
        (TextStyle::Button, FontId::new(12.5, Proportional)),
        (TextStyle::Small, FontId::new(11.0, Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = 22.0;
    style.spacing.window_margin = egui::Margin::same(12);
    style.spacing.menu_margin = egui::Margin::same(4);

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = RAISED;
    v.code_bg_color = RAISED;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_shadow =
        egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(140) };
    v.popup_shadow =
        egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(120) };
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.text_cursor.stroke = Stroke::new(2.0_f32, ACCENT);

    let r = CornerRadius::same(5);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = RAISED;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x36, 0x3D, 0x52));
    w.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = Color32::from_rgb(0x2B, 0x31, 0x44);
    w.active.weak_bg_fill = Color32::from_rgb(0x2B, 0x31, 0x44);
    w.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open = w.active;
    ctx.set_style(style);
}
