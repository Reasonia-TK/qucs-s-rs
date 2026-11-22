//! GUI color themes: "Obsidian gold" (dark) and "Ivory drafting" (light).
//!
//! The scene renderer (`render.rs`) keeps Qucs-S colors so exported images and
//! MCP snapshots stay unchanged; the GUI maps those colors onto the active
//! palette at paint time (`Palette::map`), and maps rasterized plots per pixel.

use crate::render::Color;
use egui::{Color32, CornerRadius, Margin, Shadow, Stroke, Vec2};
use std::path::PathBuf;

pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub bar: Color32,
    pub raised: Color32,
    pub raised_hover: Color32,
    pub field: Color32,
    pub border: Color32,
    pub fg: Color32,
    pub fg_strong: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_hi: Color32,
    pub accent_bg: Color32,
    pub on_accent: Color32,
    pub canvas: Color32,
    pub grid: Color32,
    /// Ink used for Qucs' dark-blue symbol strokes and black text.
    pub ink: Color32,
    pub wire: Color32,
    pub error: Color32,
    pub warn: Color32,
}

const fn c(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub static DARK: Palette = Palette {
    dark: true,
    bg: c(0x121214),
    panel: c(0x18181B),
    bar: c(0x0C0C0E),
    raised: c(0x232328),
    raised_hover: c(0x2C2B2E),
    field: c(0x0F0F11),
    border: c(0x2E2A22),
    fg: c(0xD9D4C7),
    fg_strong: c(0xF0EADB),
    muted: c(0x8A8475),
    accent: c(0xC9A45C),
    accent_hi: c(0xE5C88A),
    accent_bg: c(0x3A3020),
    on_accent: c(0x121214),
    canvas: c(0x0F0F11),
    grid: c(0x3D382E),
    ink: c(0xE8E2D2),
    wire: c(0xC9A45C),
    error: c(0xE57367),
    warn: c(0xE0B252),
};

pub static LIGHT: Palette = Palette {
    dark: false,
    bg: c(0xF6F1E7),
    panel: c(0xF1EADC),
    bar: c(0xEAE2D1),
    raised: c(0xE9E0CE),
    raised_hover: c(0xE1D6C0),
    field: c(0xFBF8F1),
    border: c(0xD8CDB8),
    fg: c(0x2B2620),
    fg_strong: c(0x1A1612),
    muted: c(0x7E7466),
    accent: c(0x7A2E3A),
    accent_hi: c(0x9C3B4A),
    accent_bg: c(0xE8D6D1),
    on_accent: c(0xF6F1E7),
    canvas: c(0xFBF8F1),
    grid: c(0xCDBFA6),
    ink: c(0x2B2620),
    wire: c(0x1F3A5F),
    error: c(0xB3261E),
    warn: c(0x8A5A00),
};

pub fn pal(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        egui::Theme::Dark => &DARK,
        egui::Theme::Light => &LIGHT,
    }
}

pub fn to_color(c: Color32) -> Color {
    Color(c.r(), c.g(), c.b(), 255)
}

fn lerp(a: Color32, b: Color32, t: f32) -> [u8; 3] {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b())]
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let l = (mx + mn) / 2.0;
    if mx - mn < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    if s < 1e-6 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

impl Palette {
    /// Map a Qucs-S drawing color (meant for a white sheet) onto this theme.
    /// `smooth` keeps the mapping continuous (for anti-aliased raster images);
    /// otherwise saturated colors get a minimum lightness in the dark theme.
    pub fn map_rgb(&self, rgb: [u8; 3], smooth: bool) -> [u8; 3] {
        let [r, g, b] = rgb;
        if Color32::from_rgb(r, g, b) == self.wire {
            return rgb;
        }
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        if mx - mn <= 24 {
            let t = (r as f32 + g as f32 + b as f32) / (3.0 * 255.0);
            return lerp(self.ink, self.canvas, t);
        }
        if !smooth && rgb == [0, 0, 128] {
            let k = self.ink;
            return [k.r(), k.g(), k.b()];
        }
        if !self.dark {
            return rgb;
        }
        let (h, s, l) = rgb_to_hsl(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let l2 = if smooth {
            1.0 - l
        } else if l < 0.6 {
            (1.0 - l).clamp(0.6, 0.85)
        } else {
            l
        };
        // de-saturate slightly so primaries don't glare on black
        hsl_to_rgb(h, s * 0.85, l2)
    }

    pub fn map(&self, c: &Color, alpha: f32) -> Color32 {
        let [r, g, b] = self.map_rgb([c.0, c.1, c.2], false);
        Color32::from_rgba_unmultiplied(r, g, b, (c.3 as f32 * alpha) as u8)
    }

    /// Recolor a premultiplied RGBA image (rendered on white) for this theme.
    pub fn map_image(&self, rgba: &[u8]) -> Vec<u8> {
        let mut out = rgba.to_vec();
        let mut cache: std::collections::HashMap<[u8; 3], [u8; 3]> = std::collections::HashMap::new();
        for px in out.as_chunks_mut::<4>().0 {
            let a = px[3];
            if a == 0 {
                continue;
            }
            let un = |v: u8| if a == 255 { v } else { ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
            let key = [un(px[0]), un(px[1]), un(px[2])];
            let m = *cache.entry(key).or_insert_with(|| self.map_rgb(key, true));
            for k in 0..3 {
                px[k] = if a == 255 { m[k] } else { ((m[k] as u32 * a as u32 + 127) / 255) as u8 };
            }
        }
        out
    }

    pub fn visuals(&self) -> egui::Visuals {
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        let r = CornerRadius::same(5);
        v.panel_fill = self.panel;
        v.window_fill = self.bg;
        v.window_stroke = Stroke::new(1.0_f32, self.border);
        v.window_corner_radius = CornerRadius::same(8);
        v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if self.dark { 140 } else { 40 }) };
        v.popup_shadow = Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(if self.dark { 120 } else { 30 }) };
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = self.field;
        v.faint_bg_color = self.raised;
        v.code_bg_color = self.raised;
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.warn;
        v.error_fg_color = self.error;
        v.selection.bg_fill = self.accent_bg;
        v.selection.stroke = Stroke::new(1.0_f32, self.accent);
        v.text_cursor.stroke = Stroke::new(2.0_f32, self.accent);
        v.striped = false;
        v.slider_trailing_fill = true;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.panel;
        w.noninteractive.weak_bg_fill = self.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, self.fg);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = self.raised;
        w.inactive.weak_bg_fill = self.raised;
        w.inactive.bg_stroke = Stroke::new(1.0_f32, self.border);
        w.inactive.fg_stroke = Stroke::new(1.0_f32, self.fg);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = self.raised_hover;
        w.hovered.weak_bg_fill = self.raised_hover;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, self.accent);
        w.hovered.fg_stroke = Stroke::new(1.5_f32, self.fg_strong);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = self.accent_bg;
        w.active.weak_bg_fill = self.accent_bg;
        w.active.bg_stroke = Stroke::new(1.0_f32, self.accent);
        w.active.fg_stroke = Stroke::new(1.5_f32, self.accent_hi);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open.bg_fill = self.accent_bg;
        w.open.weak_bg_fill = self.accent_bg;
        w.open.bg_stroke = Stroke::new(1.0_f32, self.accent);
        w.open.fg_stroke = Stroke::new(1.0_f32, self.fg_strong);
        w.open.corner_radius = r;
        v
    }
}

/// Apply both palettes and spacing to the context; egui then switches with `set_theme`.
pub fn install(ctx: &egui::Context, pref: egui::ThemePreference) {
    ctx.set_visuals_of(egui::Theme::Dark, DARK.visuals());
    ctx.set_visuals_of(egui::Theme::Light, LIGHT.visuals());
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(9.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.window_margin = Margin::same(14);
        s.spacing.menu_margin = Margin::same(8);
        s.spacing.indent = 16.0;
    });
    ctx.set_theme(pref);
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("qucs-s-rs").join("theme.txt"))
}

pub fn load_pref() -> egui::ThemePreference {
    let s = settings_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    match s.trim() {
        "dark" => egui::ThemePreference::Dark,
        "light" => egui::ThemePreference::Light,
        _ => egui::ThemePreference::System,
    }
}

pub fn save_pref(p: egui::ThemePreference) {
    let s = match p {
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::System => "system",
    };
    if let Some(path) = settings_path() {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(path, s);
    }
}
