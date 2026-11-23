//! Desktop editor (egui). It draws the very same scene the MCP snapshots use and
//! follows the .sch file live, so the user can watch – and correct – the agent.

use crate::check::{self, Issue};
use crate::doc::{snap, Doc};
use crate::library::{self, lib};
use crate::netlist::Connectivity;
use crate::render::{build_scene, Color, Overlay, Scene, Shape};
use crate::schematic::Schematic;
use crate::sim::{self, SimResult};
use crate::theme::{self, Palette};
use egui::{Align2, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Instant, SystemTime};

#[cfg(windows)]
extern "system" {
    fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    fn FreeConsole() -> i32;
}

/// When started by double-click Windows gives us a console window of our own; drop it.
fn detach_own_console() {
    #[cfg(windows)]
    unsafe {
        let mut ids = [0u32; 4];
        if GetConsoleProcessList(ids.as_mut_ptr(), 4) == 1 {
            FreeConsole();
        }
    }
}

pub fn run(path: Option<PathBuf>) {
    // one editor window: hand the file to a running editor (opens as a tab)
    if let Some(p) = &path {
        let alive = std::fs::metadata(crate::mcp::gui_heartbeat_path()).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs_f64() < 3.0).unwrap_or(false);
        if alive && std::env::var_os("QUCS_RS_NEW_WINDOW").is_none() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::mcp::gui_open_request_path()) {
                let abs = plain_path(&p.canonicalize().unwrap_or_else(|_| p.clone()));
                if writeln!(f, "{}", abs.display()).is_ok() {
                    return;
                }
            }
        }
    }
    detach_own_console();
    // the window geometry is not remembered: a saved position can point at a
    // monitor that is gone (multi-display setups), leaving the window off screen
    let vp = egui::ViewportBuilder::default().with_title("Qucs-S RS").with_icon(std::sync::Arc::new(app_icon())).with_inner_size([1280.0, 780.0]).with_maximized(true);
    let opts = eframe::NativeOptions { viewport: vp, ..Default::default() };
    let _ = eframe::run_native("qucs-s-rs", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, path)))));
}

#[derive(Clone, PartialEq)]
enum Tool {
    Select,
    Wire,
    AutoWire,
    Label,
    Place(String),
    Paint(String),
}

/// What a primary-button drag in the select tool is moving.
enum Grab {
    WireLabel { wire: usize, orig: (i32, i32), start: (f64, f64) },
    CompLabel { comp: usize, orig: (i32, i32), start: (f64, f64) },
    Paintings { orig: Vec<(usize, String)>, start: (i32, i32), moved: (i32, i32) },
    /// a wire segment: `orig` is the wire list before the drag, `pins` the pin points
    Wire { wire: usize, orig: Vec<crate::schematic::Wire>, pins: std::collections::HashSet<(i32, i32)>, start: (i32, i32), moved: (i32, i32) },
    /// a mixed selection (parts + wires + paintings) moved as one block; `orig` is the
    /// schematic before the drag, `pins` the component pins `(component, x, y)` then
    Group { orig: Box<Schematic>, pins: Vec<(usize, i32, i32)>, comps: Vec<usize>, wires: Vec<usize>, paints: Vec<usize>, start: (i32, i32), moved: (i32, i32) },
}

struct Drag {
    start: (i32, i32),
    orig: Vec<(usize, i32, i32)>,
    moved: bool,
}

struct App {
    doc: Doc,
    conn: Connectivity,
    scene: Scene,
    gen: u64,
    scene_key: (u64, bool, bool, bool, Vec<usize>, Vec<usize>),
    offset: Vec2,
    zoom: f32,
    need_fit: bool,
    tool: Tool,
    sel: Vec<usize>,
    sel_wires: Vec<usize>,
    drag: Option<Drag>,
    grab: Option<Grab>,
    sel_paint: Vec<usize>,
    paint_start: Option<(i32, i32)>,
    text_prompt: Option<((i32, i32), String, Option<usize>)>,
    box_start: Option<Pos2>,
    wire_pts: Vec<(i32, i32)>,
    autowire_first: Option<String>,
    place_rot: i32,
    query: String,
    lib_query: String,
    palette_tab: u8,
    color_nets: bool,
    pin_names: bool,
    show_issues: bool,
    show_grid: bool,
    auto_save: bool,
    auto_reroute: bool,
    disk_mtime: Option<SystemTime>,
    last_poll: Instant,
    external_change: bool,
    status: String,
    issues: Vec<Issue>,
    show_issue_panel: bool,
    sim_rx: Option<(u64, Receiver<Result<SimResult, String>>)>,
    sim: Option<SimResult>,
    show_sim: bool,
    plot_analysis: usize,
    plot_vectors: Vec<String>,
    plot_tex: Option<egui::TextureHandle>,
    plot_dirty: bool,
    theme_pref: egui::ThemePreference,
    /// Theme the scene and plot textures were built for.
    tex_theme: egui::Theme,
    prop_for: Option<usize>,
    prop_buf: Vec<String>,
    name_buf: String,
    label_prompt: Option<(String, String)>, // (terminal, text)
    cursor: (i32, i32),
    last_state: String,
    last_state_write: Instant,
    icons: std::collections::HashMap<String, (Vec<Shape>, [f64; 4])>,
    diag_kind: crate::diagram::DiagramKind,
    markers: Vec<f64>,
    marker_out: serde_json::Value,
    plot_area: Option<(f64, f64, f64, f64, f64, f64, bool)>,
    plot_px: (f32, f32),
    tools: Tools,
    // ---- tabs: the active document lives in the fields above, the others here
    tab_id: u64,
    next_tab_id: u64,
    tabs: Vec<Option<TabState>>,
    active: usize,
    last_heartbeat: Instant,
    mc: McWindow,
    // ---- preferences, dialogs
    settings: crate::settings::Settings,
    confirm: Option<Confirm>,
    allow_close: bool,
    find_open: bool,
    find_query: String,
    find_focus: bool,
    about_open: bool,
    canvas_rect: Rect,
    op_rx: Option<(u64, Receiver<Result<sim::OperatingPoint, String>>)>,
    op: Option<OpView>,
    op_window: bool,
    subckt_prompt: Option<String>,
    shot: Option<Shot>,
}

/// Self-test hook: `QUCS_RS_SHOT=out.png` saves a screenshot of the window and exits.
/// `QUCS_RS_SHOT_ACTIONS` (comma separated) prepares the view first:
/// `op` (DC operating point), `find:<text>`, `about`, `quit` (the save-changes dialog),
/// `select:<name>`, `zoom:<factor>`.
struct Shot {
    path: PathBuf,
    actions: Vec<String>,
    frame: u32,
    requested: bool,
}

/// A pending "save changes?" question.
#[derive(Clone, Copy, PartialEq)]
enum Confirm {
    /// closing the tab with this index
    Tab(usize),
    /// closing the window
    Quit,
}

/// A computed DC operating point and the schematic it belongs to.
struct OpView {
    tab_id: u64,
    comps: Vec<crate::schematic::Component>,
    wires: Vec<crate::schematic::Wire>,
    op: sim::OperatingPoint,
}

/// Per-document state of an inactive tab.
struct TabState {
    tab_id: u64,
    doc: Doc,
    conn: Connectivity,
    scene: Scene,
    gen: u64,
    offset: Vec2,
    zoom: f32,
    need_fit: bool,
    sel: Vec<usize>,
    sel_wires: Vec<usize>,
    sel_paint: Vec<usize>,
    disk_mtime: Option<SystemTime>,
    external_change: bool,
    issues: Vec<Issue>,
    sim: Option<SimResult>,
    show_sim: bool,
    plot_analysis: usize,
    plot_vectors: Vec<String>,
    markers: Vec<f64>,
}

impl TabState {
    fn new(doc: Doc, id: u64) -> TabState {
        let conn = doc.conn();
        let disk_mtime = doc.path.as_ref().and_then(mtime);
        TabState {
            tab_id: id,
            doc,
            conn,
            scene: Scene::default(),
            gen: 1,
            offset: Vec2::ZERO,
            zoom: 1.5,
            need_fit: true,
            sel: vec![],
            sel_wires: vec![],
            sel_paint: vec![],
            disk_mtime,
            external_change: false,
            issues: vec![],
            sim: None,
            show_sim: false,
            plot_analysis: 0,
            plot_vectors: vec![],
            markers: vec![],
        }
    }
}

/// Monte Carlo window.
struct McWindow {
    open: bool,
    runs: usize,
    tol_r: f64,
    tol_c: f64,
    tol_l: f64,
    gauss: bool,
    analysis: String,
    vector: String,
    at: String,
    rx: Option<Receiver<Result<(serde_json::Value, Option<(u32, u32, Vec<u8>)>, Option<(u32, u32, Vec<u8>)>), String>>>,
    summary: Option<serde_json::Value>,
    tex: Vec<egui::TextureHandle>,
    raw: Vec<(u32, u32, Vec<u8>)>,
    error: String,
}

impl Default for McWindow {
    fn default() -> Self {
        McWindow { open: false, runs: 50, tol_r: 5.0, tol_c: 10.0, tol_l: 10.0, gauss: true, analysis: String::new(), vector: String::new(), at: String::new(), rx: None, summary: None, tex: vec![], raw: vec![], error: String::new() }
    }
}

/// State of the analysis / design tool windows.
struct Tools {
    filter_open: bool,
    f_response: usize,
    f_class: usize,
    f_order: usize,
    f_ripple: f64,
    f_f1: String,
    f_f2: String,
    f_z0: f64,
    f_series: bool,
    f_bench: bool,
    atten_open: bool,
    a_topo: usize,
    a_db: f64,
    a_zin: f64,
    a_zout: f64,
    tline_open: bool,
    t_coax: bool,
    t_er: f64,
    t_h: f64,
    t_t: f64,
    t_w: f64,
    t_z0: f64,
    t_freq: String,
    t_d: f64,
    t_dd: f64,
    t_out: String,
    sweep_open: bool,
    s_param: String,
    s_sim: String,
    s_start: String,
    s_stop: String,
    s_points: String,
    s_type: usize,
    tune_open: bool,
    tunes: Vec<Tune>,
    tune_auto: bool,
    tune_pick: usize,
    result: String,
}

struct Tune {
    comp: String,
    prop: usize,
    /// `Some(name)` for a `name=value` parameter (SpicePar / Eqn).
    key: Option<String>,
    label: String,
    unit: String,
    value: f64,
    min: f64,
    max: f64,
}

impl Tune {
    fn same_target(&self, o: &Tune) -> bool {
        self.comp == o.comp && self.prop == o.prop && self.key == o.key
    }

    /// The current value as it is written (without the `name=` part).
    fn value_text(&self) -> String {
        if self.key.is_none() {
            return crate::rftools::format_value(self.value, &self.unit);
        }
        // SPICE reads "M" as milli and does not allow "1 k"
        let v = crate::rftools::format_value(self.value, "").replace(' ', "");
        match v.strip_suffix('M') {
            Some(m) => format!("{m}Meg"),
            None => v,
        }
    }

    /// Property text for the current value.
    fn text(&self) -> String {
        match &self.key {
            Some(k) => format!("{k}={}", self.value_text()),
            None => self.value_text(),
        }
    }
}

/// Unit name following the number and SI prefix (`"10 pF"` -> `"F"`).
fn value_unit(s: &str) -> String {
    let rest = s.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' || ch == 'e' || ch == 'E').trim();
    let rest = ["Meg", "meg", "MEG"].iter().find_map(|p| rest.strip_prefix(p)).unwrap_or_else(|| rest.trim_start_matches(['f', 'p', 'n', 'u', 'µ', 'm', 'k', 'M', 'G', 'T']));
    rest.trim().to_string()
}

/// Every numeric value of `c` that the tuning window can drive.
fn tune_targets(c: &crate::schematic::Component) -> Vec<Tune> {
    let keyed = c.typ == "SpicePar" || c.typ == "Eqn";
    let mut out = Vec::new();
    for (i, p) in c.props.iter().enumerate() {
        let (key, text) = match p.value.split_once('=') {
            Some((k, v)) if keyed && !k.trim().is_empty() => (Some(k.trim().to_string()), v.trim()),
            _ => (None, p.value.trim()),
        };
        let Some(v) = crate::rftools::parse_value(text) else { continue };
        if v == 0.0 || !v.is_finite() {
            continue; // no sensible ×1/10..×10 range
        }
        let label = match &key {
            Some(k) => format!("{}: {k}", c.name),
            None => format!("{}.{}", c.name, crate::library::prop_name(c, i)),
        };
        let unit = if key.is_some() { String::new() } else { value_unit(text) };
        let (min, max) = if v > 0.0 { (v / 10.0, v * 10.0) } else { (v * 10.0, v / 10.0) };
        out.push(Tune { comp: c.name.clone(), prop: i, key, label, unit, value: v, min, max });
    }
    out
}

impl Default for Tools {
    fn default() -> Self {
        Tools {
            filter_open: false,
            f_response: 0,
            f_class: 0,
            f_order: 3,
            f_ripple: 0.5,
            f_f1: "10 MHz".into(),
            f_f2: "20 MHz".into(),
            f_z0: 50.0,
            f_series: false,
            f_bench: true,
            atten_open: false,
            a_topo: 0,
            a_db: 10.0,
            a_zin: 50.0,
            a_zout: 50.0,
            tline_open: false,
            t_coax: false,
            t_er: 4.4,
            t_h: 1.6,
            t_t: 35.0,
            t_w: 3.0,
            t_z0: 50.0,
            t_freq: "1 GHz".into(),
            t_d: 0.9,
            t_dd: 2.95,
            t_out: String::new(),
            sweep_open: false,
            s_param: String::new(),
            s_sim: String::new(),
            s_start: String::new(),
            s_stop: String::new(),
            s_points: "5".into(),
            s_type: 0,
            tune_open: false,
            tunes: vec![],
            tune_auto: true,
            tune_pick: 0,
            result: String::new(),
        }
    }
}

fn mtime(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn load_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        ("segoe", r"C:\Windows\Fonts\segoeui.ttf", 0u32),
        ("meiryo", r"C:\Windows\Fonts\meiryo.ttc", 0),
        ("msgothic", r"C:\Windows\Fonts\msgothic.ttc", 0),
        ("noto_cjk", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
        ("noto_cjk2", "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
        ("noto_cjk3", "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 0),
        ("noto_cjk4", "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc", 0),
        ("droid", "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf", 0),
        ("hiragino", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
        ("hiragino2", "/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
        ("arialuni", "/Library/Fonts/Arial Unicode.ttf", 0),
    ];
    let mut first = true;
    let mut cjk = false;
    let only = std::env::var("QUCS_RS_FONTS").ok();
    for (name, path, idx) in candidates {
        if let Some(o) = &only {
            if !o.split(',').any(|x| x == name) {
                continue;
            }
        }
        if name != "segoe" && cjk {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path) {
            let mut fd = egui::FontData::from_owned(bytes);
            fd.index = idx;
            fonts.font_data.insert(name.to_string(), std::sync::Arc::new(fd));
            let fam = fonts.families.entry(egui::FontFamily::Proportional).or_default();
            if first && name == "segoe" {
                fam.insert(0, name.to_string());
            } else {
                fam.push(name.to_string());
                fonts.families.entry(egui::FontFamily::Monospace).or_default().push(name.to_string());
                cjk = true;
            }
            first = false;
        }
    }
    ctx.set_fonts(fonts);
}

/// Window / taskbar icon (same drawing as the .exe icon, see build.rs).
fn app_icon() -> egui::IconData {
    egui::IconData { rgba: crate::icon_pixels::icon_rgba(64), width: 64, height: 64 }
}

/// `canonicalize` on Windows yields `\?\C:\...`; Qucs-S and users expect plain paths.
fn plain_path(p: &std::path::Path) -> PathBuf {
    let s = p.display().to_string();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

/// Component for a palette key: a type name, or "Lib:<library>/<component>".
fn spec_component(spec: &str, x: i32, y: i32) -> Result<crate::schematic::Component, String> {
    match spec.strip_prefix("Lib:").and_then(|r| r.split_once('/')) {
        Some((l, c)) => crate::qlib::new_lib_component(l, c, "X", x, y),
        None => library::new_component(spec, "?", x, y),
    }
}

fn paint_shapes_xf(painter: &egui::Painter, shapes: &[Shape], alpha: f32, z: f32, xf: &dyn Fn(f64, f64) -> Pos2) {
    let pal: &Palette = theme::pal(painter.ctx());
    let col = |c: &Color| pal.map(c, alpha);
    for s in shapes {
        match s {
            Shape::Line { a, b, color, width, dash } => {
                let pa = xf(a.0, a.1);
                let pb = xf(b.0, b.1);
                let st = Stroke::new((*width as f32 * z).max(1.0), col(color));
                if *dash {
                    painter.extend(egui::Shape::dashed_line(&[pa, pb], st, 4.0 * z, 3.0 * z));
                } else {
                    painter.line_segment([pa, pb], st);
                }
            }
            Shape::Poly { pts, color, width, fill, closed, .. } => {
                let p: Vec<Pos2> = pts.iter().map(|q| xf(q.0, q.1)).collect();
                let st = if *width > 0.0 { Stroke::new((*width as f32 * z).max(1.0), col(color)) } else { Stroke::NONE };
                if let Some(f) = fill {
                    painter.add(egui::Shape::convex_polygon(p.clone(), col(f), Stroke::NONE));
                }
                if *width > 0.0 {
                    if *closed {
                        painter.add(egui::Shape::closed_line(p, st));
                    } else {
                        painter.add(egui::Shape::line(p, st));
                    }
                }
            }
            Shape::Ellipse { c, rx, ry, color, width, fill } => {
                let pc = xf(c.0, c.1);
                let r = Vec2::new(*rx as f32 * z, *ry as f32 * z);
                if let Some(f) = fill {
                    painter.add(egui::Shape::ellipse_filled(pc, r, col(f)));
                }
                if *width > 0.0 {
                    painter.add(egui::Shape::ellipse_stroke(pc, r, Stroke::new((*width as f32 * z).max(1.0), col(color))));
                }
            }
            Shape::Text { pos, text, size, color, bold, angle, bg } => {
                let fs = (*size as f32 * z).max(1.0);
                if fs < 3.0 {
                    continue;
                }
                let p = xf(pos.0, pos.1);
                let galley = painter.layout_no_wrap(text.clone(), FontId::proportional(fs), col(color));
                if let Some(b) = bg {
                    painter.rect_filled(Rect::from_min_size(p - Vec2::new(2.0, 0.0), galley.size() + Vec2::new(4.0, 0.0)), 2.0, col(b));
                }
                let mut ts = egui::epaint::TextShape::new(p, galley.clone(), col(color));
                if angle.abs() > 0.01 {
                    ts = ts.with_angle(angle.to_radians() as f32);
                }
                painter.add(ts);
                if *bold {
                    // faux bold
                    let mut t2 = egui::epaint::TextShape::new(p + Vec2::new(0.6, 0.0), galley, col(color));
                    if angle.abs() > 0.01 {
                        t2 = t2.with_angle(angle.to_radians() as f32);
                    }
                    painter.add(t2);
                }
            }
        }
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> App {
        if std::env::var_os("QUCS_RS_NO_FONTS").is_none() {
            load_fonts(&cc.egui_ctx);
        }
        let theme_pref = theme::load_pref();
        theme::install(&cc.egui_ctx, theme_pref);
        // Ctrl +/- zoom the schematic, not the whole UI
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let settings = crate::settings::Settings::load();
        let mut status = String::new();
        let doc = match &path {
            Some(p) if p.exists() => Doc::open(p).unwrap_or_else(|e| {
                status = format!("読み込みエラー: {e}");
                Doc::new(Schematic::new_empty("untitled"), Some(p.clone()))
            }),
            Some(p) => Doc::new(Schematic::new_empty(&p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()), Some(p.clone())),
            None => Doc::new(Schematic::new_empty("untitled"), None),
        };
        let conn = doc.conn();
        let disk_mtime = doc.path.as_ref().and_then(mtime);
        let mut settings = settings;
        if let Some(p) = doc.path.as_ref().filter(|p| p.exists()) {
            settings.add_recent(p);
            settings.save();
        }
        App {
            conn,
            doc,
            scene: Scene::default(),
            gen: 1,
            scene_key: (0, false, false, false, vec![], vec![]),
            offset: Vec2::ZERO,
            zoom: 1.5,
            need_fit: true,
            tool: Tool::Select,
            sel: vec![],
            sel_wires: vec![],
            drag: None,
            grab: None,
            sel_paint: vec![],
            paint_start: None,
            text_prompt: None,
            box_start: None,
            wire_pts: vec![],
            autowire_first: None,
            place_rot: 0,
            query: String::new(),
            lib_query: String::new(),
            palette_tab: 0,
            color_nets: settings.color_nets,
            pin_names: settings.pin_names,
            show_issues: settings.show_issues,
            show_grid: settings.show_grid,
            auto_save: settings.auto_save,
            auto_reroute: settings.auto_reroute,
            disk_mtime,
            last_poll: Instant::now(),
            external_change: false,
            status: if status.is_empty() { "準備完了".into() } else { status },
            issues: vec![],
            show_issue_panel: false,
            sim_rx: None,
            sim: None,
            show_sim: false,
            plot_analysis: 0,
            plot_vectors: vec![],
            plot_tex: None,
            plot_dirty: false,
            theme_pref,
            tex_theme: cc.egui_ctx.theme(),
            prop_for: None,
            prop_buf: vec![],
            name_buf: String::new(),
            label_prompt: None,
            cursor: (0, 0),
            last_state: String::new(),
            last_state_write: Instant::now(),
            icons: Default::default(),
            diag_kind: crate::diagram::DiagramKind::Rect,
            markers: vec![],
            marker_out: serde_json::Value::Null,
            plot_area: None,
            plot_px: (0.0, 0.0),
            tools: Tools::default(),
            tab_id: 1,
            next_tab_id: 2,
            tabs: vec![None],
            active: 0,
            last_heartbeat: Instant::now() - std::time::Duration::from_secs(10),
            mc: McWindow::default(),
            settings,
            confirm: None,
            allow_close: false,
            find_open: false,
            find_query: String::new(),
            find_focus: false,
            about_open: false,
            canvas_rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
            op_rx: None,
            op: None,
            op_window: false,
            subckt_prompt: None,
            shot: std::env::var_os("QUCS_RS_SHOT").map(|p| Shot {
                path: PathBuf::from(p),
                actions: std::env::var("QUCS_RS_SHOT_ACTIONS").unwrap_or_default().split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect(),
                frame: 0,
                requested: false,
            }),
        }
    }

    fn drive_shot(&mut self, ctx: &egui::Context) {
        let Some(sh) = self.shot.as_mut() else { return };
        sh.frame += 1;
        let frame = sh.frame;
        ctx.request_repaint();
        if frame == 5 {
            let actions = sh.actions.clone();
            for a in actions {
                let (k, v) = a.split_once(':').unwrap_or((a.as_str(), ""));
                match k {
                    "op" => self.run_op(),
                    "find" => {
                        self.find_open = true;
                        self.find_query = v.to_string();
                    }
                    "about" => self.about_open = true,
                    "quit" => {
                        self.doc.dirty = true;
                        self.auto_save = false;
                        self.confirm = Some(Confirm::Quit);
                    }
                    "select" => self.sel = self.doc.sch.component_index(v).into_iter().collect(),
                    "zoom" => self.zoom_by(v.parse().unwrap_or(1.0)),
                    _ => {}
                }
            }
        }
        // screenshot once background work is done
        let busy = self.op_rx.is_some() || self.sim_rx.is_some();
        if let Some(sh) = self.shot.as_mut() {
            if frame > 30 && !busy && !sh.requested {
                sh.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            if frame > 3000 {
                eprintln!("shot: timed out");
                std::process::exit(2);
            }
        }
        let img = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let (Some(img), Some(sh)) = (img, self.shot.as_ref()) {
            let [w, h] = img.size;
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let r = resvg::tiny_skia::IntSize::from_wh(w as u32, h as u32)
                .and_then(|sz| resvg::tiny_skia::Pixmap::from_vec(rgba, sz))
                .ok_or_else(|| "bad image".to_string())
                .and_then(|pm| pm.encode_png().map_err(|e| e.to_string()))
                .and_then(|png| std::fs::write(&sh.path, png).map_err(|e| e.to_string()));
            match r {
                Ok(_) => eprintln!("shot: {} ({}) {}", sh.path.display(), self.status, w),
                Err(e) => eprintln!("shot failed: {e}"),
            }
            self.shot = None;
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Edits that would be lost: not saved to disk (auto-save only covers named files).
    fn unsaved(&self, d: &Doc) -> bool {
        d.dirty && (d.path.is_none() || !self.auto_save)
    }

    fn tab_unsaved(&self, i: usize) -> bool {
        if i == self.active {
            self.unsaved(&self.doc)
        } else {
            self.tabs.get(i).and_then(|t| t.as_ref()).map(|t| self.unsaved(&t.doc)).unwrap_or(false)
        }
    }

    /// Save the active document (asks for a name when it has none). False when not saved.
    fn save_current(&mut self) -> bool {
        if self.doc.path.is_none() {
            return self.save_as();
        }
        match self.doc.save() {
            Ok(p) => {
                self.disk_mtime = mtime(&p);
                self.status = format!("保存: {}", p.display());
                true
            }
            Err(e) => {
                self.status = format!("保存エラー: {e}");
                false
            }
        }
    }

    /// Write the view toggles into the settings and store them.
    fn store_settings(&mut self) {
        let s = &mut self.settings;
        s.color_nets = self.color_nets;
        s.pin_names = self.pin_names;
        s.show_issues = self.show_issues;
        s.show_grid = self.show_grid;
        s.auto_save = self.auto_save;
        s.auto_reroute = self.auto_reroute;
        s.save();
    }

    // ------------------------------------------------------------------ model sync

    fn changed(&mut self) {
        self.gen += 1;
        self.conn = self.doc.conn();
        self.sel.retain(|&i| i < self.doc.sch.components.len());
        self.sel_wires.retain(|&i| i < self.doc.sch.wires.len());
        self.prop_for = None;
        if self.show_issues {
            self.issues = check::check(&self.doc.sch, &self.conn).issues;
        }
        if self.auto_save && self.doc.path.is_some() {
            match self.doc.save() {
                Ok(p) => self.disk_mtime = mtime(&p),
                Err(e) => self.status = format!("保存エラー: {e}"),
            }
        }
    }

    fn reload_from_disk(&mut self) {
        if let Some(p) = self.doc.path.clone() {
            match Doc::open(&p) {
                Ok(d) => {
                    let was_empty = self.doc.sch.components.is_empty();
                    self.doc = d;
                    self.disk_mtime = mtime(&p);
                    if was_empty {
                        self.need_fit = true;
                    }
                    self.gen += 1;
                    self.conn = self.doc.conn();
                    self.sel.clear();
                    self.sel_wires.clear();
                    self.prop_for = None;
                    if self.show_issues {
                        self.issues = check::check(&self.doc.sch, &self.conn).issues;
                    }
                    self.external_change = false;
                    self.status = "ファイルの変更（AIエージェント等）を反映しました".into();
                }
                Err(e) => self.status = format!("再読み込み失敗: {e}"),
            }
        }
    }

    fn poll_disk(&mut self) {
        if self.last_poll.elapsed().as_millis() < 300 {
            return;
        }
        self.last_poll = Instant::now();
        let Some(p) = self.doc.path.clone() else { return };
        let m = mtime(&p);
        if m.is_some() && m != self.disk_mtime {
            if self.doc.dirty && !self.auto_save {
                self.external_change = true;
            } else {
                self.reload_from_disk();
            }
        }
    }

    fn write_state(&mut self, canvas: Rect) {
        if self.last_state_write.elapsed().as_millis() < 400 {
            return;
        }
        self.last_state_write = Instant::now();
        let tl = self.to_sch(canvas, canvas.min);
        let br = self.to_sch(canvas, canvas.max);
        let st = serde_json::json!({
            "file": self.doc.path.as_ref().map(|p| p.display().to_string()),
            "selected_components": self.sel.iter().filter_map(|&i| self.doc.sch.components.get(i).map(|c| c.name.clone())).collect::<Vec<_>>(),
            "selected_wires": self.sel_wires.iter().filter_map(|&i| self.doc.sch.wires.get(i).map(|w| serde_json::json!({"i": i, "p": [w.x1, w.y1, w.x2, w.y2]}))).collect::<Vec<_>>(),
            "visible_region": [tl.0, tl.1, br.0, br.1],
            "cursor": [self.cursor.0, self.cursor.1],
            "tool": match &self.tool { Tool::Select => "select".to_string(), Tool::Wire => "wire".into(), Tool::AutoWire => "autowire".into(), Tool::Label => "label".into(), Tool::Place(t) => format!("place:{t}"), Tool::Paint(k) => format!("paint:{k}") },
            "unsaved_changes": self.doc.dirty,
            "open_tabs": (0..self.tabs.len()).map(|i| if i == self.active { self.doc.path.clone() } else { self.tabs[i].as_ref().and_then(|t| t.doc.path.clone()) }.map(|p| p.display().to_string())).collect::<Vec<_>>(),
        })
        .to_string();
        if st != self.last_state {
            let p = crate::mcp::gui_state_path();
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&p, &st);
            self.last_state = st;
        }
    }

    // ------------------------------------------------------------------ coordinates

    fn to_screen(&self, canvas: Rect, x: f64, y: f64) -> Pos2 {
        canvas.min + (Vec2::new(x as f32, y as f32) - self.offset) * self.zoom
    }
    fn to_sch_f(&self, canvas: Rect, p: Pos2) -> (f64, f64) {
        let v = (p - canvas.min) / self.zoom + self.offset;
        (v.x as f64, v.y as f64)
    }
    fn to_sch(&self, canvas: Rect, p: Pos2) -> (i32, i32) {
        let (x, y) = self.to_sch_f(canvas, p);
        (x.round() as i32, y.round() as i32)
    }
    fn snapped(&self, canvas: Rect, p: Pos2) -> (i32, i32) {
        let (x, y) = self.to_sch(canvas, p);
        (snap(x), snap(y))
    }

    fn fit(&mut self, canvas: Rect) {
        let b = self.scene.bounds;
        let (w, h) = ((b[2] - b[0]).max(100.0) as f32 + 80.0, (b[3] - b[1]).max(100.0) as f32 + 80.0);
        self.zoom = (canvas.width() / w).min(canvas.height() / h).clamp(0.2, 4.0);
        let cx = (b[0] + b[2]) as f32 / 2.0;
        let cy = (b[1] + b[3]) as f32 / 2.0;
        self.offset = Vec2::new(cx, cy) - canvas.size() / 2.0 / self.zoom;
    }

    // ------------------------------------------------------------------ hit tests

    fn comp_at(&self, x: f64, y: f64) -> Option<usize> {
        let mut best: Option<(f64, usize)> = None;
        for (i, p) in self.conn.placed.iter().enumerate() {
            let b = p.bbox;
            if x >= b[0] - 2.0 && x <= b[2] + 2.0 && y >= b[1] - 2.0 && y <= b[3] + 2.0 {
                let area = (b[2] - b[0]) * (b[3] - b[1]);
                if best.map(|bb| area < bb.0).unwrap_or(true) {
                    best = Some((area, i));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Start dragging the whole selection (parts, wires and paintings) as one block.
    fn start_group(&mut self, px: f64, py: f64) {
        let pins = self.conn.pins.iter().map(|p| (p.comp, p.x, p.y)).collect();
        self.grab = Some(Grab::Group {
            orig: Box::new(self.doc.sch.clone()),
            pins,
            comps: self.sel.clone(),
            wires: self.sel_wires.clone(),
            paints: self.sel_paint.clone(),
            start: (px.round() as i32, py.round() as i32),
            moved: (0, 0),
        });
    }

    fn wire_at(&self, x: f64, y: f64) -> Option<usize> {
        let tol = (5.0 / self.zoom as f64).max(2.0);
        for (i, w) in self.doc.sch.wires.iter().enumerate() {
            let (x1, y1, x2, y2) = (w.x1 as f64, w.y1 as f64, w.x2 as f64, w.y2 as f64);
            let (dx, dy) = (x2 - x1, y2 - y1);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((x - x1) * dx + (y - y1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let (px, py) = (x1 + t * dx, y1 + t * dy);
            if ((px - x).powi(2) + (py - y).powi(2)).sqrt() <= tol {
                return Some(i);
            }
        }
        None
    }

    fn pin_at(&self, x: f64, y: f64) -> Option<String> {
        let tol = (8.0 / self.zoom as f64).max(4.0);
        self.conn
            .pins
            .iter()
            .map(|p| (((p.x as f64 - x).powi(2) + (p.y as f64 - y).powi(2)).sqrt(), p))
            .filter(|(d, _)| *d <= tol)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p.label.clone())
    }

    // ------------------------------------------------------------------ actions

    fn rotate_selected(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.doc.checkpoint();
        for &i in &self.sel.clone() {
            let c = &mut self.doc.sch.components[i];
            c.rotate = (c.rotate + 1) % 4;
            self.doc.default_label_pos(i);
        }
        self.reroute_selected();
        self.changed();
    }

    fn mirror_selected(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.doc.checkpoint();
        for &i in &self.sel.clone() {
            let c = &mut self.doc.sch.components[i];
            c.mirror = 1 - c.mirror.clamp(0, 1);
        }
        self.reroute_selected();
        self.changed();
    }

    fn reroute_selected(&mut self) {
        if !self.auto_reroute {
            return;
        }
        let moves: Vec<(String, i32, i32)> = self.sel.iter().map(|&i| {
            let c = &self.doc.sch.components[i];
            (c.name.clone(), c.x, c.y)
        }).collect();
        if let Err(e) = self.doc.move_components(&moves, true) {
            self.status = e;
        }
    }

    fn copy_selection(&mut self, ctx: &egui::Context, cut: bool) {
        if self.sel.is_empty() && self.sel_wires.is_empty() && self.sel_paint.is_empty() {
            return;
        }
        let text = crate::edit::copy_text(&self.doc, &self.sel, &self.sel_wires, &self.sel_paint);
        ctx.copy_text(text);
        self.status = format!("{} 個の部品をコピー", self.sel.len());
        if cut {
            self.delete_selected();
        }
    }

    fn paste(&mut self, text: &str) {
        // place the clip so that its top-left lands at the cursor
        let Ok(clip) = Schematic::parse(text.trim()) else {
            self.status = "クリップボードに回路図データがありません".into();
            return;
        };
        let mut minx = i32::MAX;
        let mut miny = i32::MAX;
        for c in &clip.components {
            minx = minx.min(c.x);
            miny = miny.min(c.y);
        }
        for w in &clip.wires {
            minx = minx.min(w.x1.min(w.x2));
            miny = miny.min(w.y1.min(w.y2));
        }
        if minx == i32::MAX {
            minx = self.cursor.0;
            miny = self.cursor.1;
        }
        self.doc.checkpoint();
        match crate::edit::paste_text(&mut self.doc, text, snap(self.cursor.0 - minx), snap(self.cursor.1 - miny)) {
            Ok((c, w, p)) => {
                self.status = format!("{} 個の部品を貼り付け", c.len());
                self.changed();
                self.sel = c;
                self.sel_wires = w;
                self.sel_paint = p;
            }
            Err(e) => self.status = e,
        }
    }

    fn duplicate_selection(&mut self) {
        if self.sel.is_empty() && self.sel_paint.is_empty() {
            return;
        }
        self.doc.checkpoint();
        match crate::edit::duplicate(&mut self.doc, &self.sel.clone(), &self.sel_wires.clone(), &self.sel_paint.clone(), 40, 40) {
            Ok((c, w, p)) => {
                self.changed();
                self.sel = c;
                self.sel_wires = w;
                self.sel_paint = p;
                self.status = "複製しました".into();
            }
            Err(e) => self.status = e,
        }
    }

    fn mirror_y_selected(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.doc.checkpoint();
        for &i in &self.sel.clone() {
            crate::edit::mirror_y(&mut self.doc.sch.components[i]);
            self.doc.default_label_pos(i);
        }
        self.reroute_selected();
        self.changed();
    }

    fn align_selected(&mut self, mode: &str) {
        match crate::edit::align(&self.doc, &self.sel, mode) {
            Ok(moves) => {
                self.doc.checkpoint();
                if let Err(e) = self.doc.move_components(&moves, self.auto_reroute) {
                    self.status = e;
                }
                self.changed();
            }
            Err(e) => self.status = e,
        }
    }

    fn export_image(&mut self) {
        let Some(p) = rfd::FileDialog::new().add_filter("PNG", &["png"]).add_filter("SVG", &["svg"]).set_file_name("schematic.png").save_file() else { return };
        let ov = Overlay::default();
        let sc = build_scene(&self.doc.sch, &self.conn, &ov);
        let view = crate::render::fit_view(sc.bounds, 30.0, Some(2.0), 6000.0);
        let (svg, _) = crate::render::scene_svg(&sc, &view, &crate::render::Decor::default());
        let res = if p.extension().map(|e| e.eq_ignore_ascii_case("svg")).unwrap_or(false) {
            std::fs::write(&p, svg).map_err(|e| e.to_string())
        } else {
            crate::render::svg_to_png(&svg).and_then(|png| std::fs::write(&p, png).map_err(|e| e.to_string()))
        };
        self.status = match res {
            Ok(_) => format!("書き出し: {}", p.display()),
            Err(e) => format!("書き出し失敗: {e}"),
        };
    }

    fn delete_selected(&mut self) {
        if self.sel.is_empty() && self.sel_wires.is_empty() && self.sel_paint.is_empty() {
            return;
        }
        self.doc.checkpoint();
        let mut ps = std::mem::take(&mut self.sel_paint);
        ps.sort_unstable();
        for &k in ps.iter().rev() {
            if k < self.doc.sch.paintings.len() {
                self.doc.sch.paintings.remove(k);
            }
        }
        let names: Vec<String> = self.sel.iter().map(|&i| self.doc.sch.components[i].name.clone()).collect();
        let _ = self.doc.delete_components(&names);
        let mut ws = self.sel_wires.clone();
        ws.sort_unstable();
        for &w in ws.iter().rev() {
            if w < self.doc.sch.wires.len() {
                self.doc.sch.wires.remove(w);
            }
        }
        self.sel.clear();
        self.sel_wires.clear();
        self.changed();
    }

    /// The file a simulation reads. With auto-save on it is the document itself (saved
    /// first); otherwise unsaved edits go to a temporary copy next to it, so that trying
    /// values never overwrites the .sch. Returns (file to simulate, copy to delete afterwards).
    fn sim_source(&mut self, kind: &str) -> Result<(PathBuf, Option<PathBuf>), String> {
        let p = self.doc.path.clone().ok_or_else(|| "先に保存してください".to_string())?;
        if self.auto_save {
            self.doc.save()?;
            self.disk_mtime = mtime(&p);
            return Ok((p, None));
        }
        if !self.doc.dirty {
            return Ok((p, None));
        }
        let tmp = sim::unsaved_copy_path(&p, kind)?;
        self.doc.sch.save(&tmp)?;
        Ok((tmp.clone(), Some(tmp)))
    }

    fn run_sim(&mut self) {
        let (p, tmp) = match self.sim_source("sim") {
            Ok(x) => x,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = sim::simulate(&p, 300);
            if let Some(t) = tmp {
                let _ = std::fs::remove_file(t);
            }
            let _ = tx.send(r);
        });
        self.sim_rx = Some((self.tab_id, rx));
        self.status = "シミュレーション実行中…".into();
    }

    fn open_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new().add_filter("Qucs schematic", &["sch"]).pick_files() {
            for p in files {
                self.open_path(&p);
            }
        }
    }

    /// Open a file in a tab (switches to it when it is already open).
    fn open_path(&mut self, p: &std::path::Path) {
        let canon = |x: &std::path::Path| x.canonicalize().unwrap_or_else(|_| x.to_path_buf());
        let want = canon(p);
        if self.doc.path.as_deref().map(canon) == Some(want.clone()) {
            return;
        }
        if let Some(i) = self.tabs.iter().position(|t| t.as_ref().and_then(|t| t.doc.path.as_deref().map(canon)) == Some(want.clone())) {
            self.switch_tab(i);
            return;
        }
        match Doc::open(p) {
            Ok(d) => {
                // reuse an empty, untitled tab
                if self.doc.path.is_none() && self.doc.sch.components.is_empty() && self.doc.sch.wires.is_empty() {
                    let id = self.tab_id;
                    self.install_tab(TabState::new(d, id));
                } else {
                    self.new_tab(d);
                }
                self.status = format!("開きました: {}", p.display());
                self.settings.add_recent(p);
                self.settings.save();
            }
            Err(e) => {
                self.status = e;
                // drop stale entries from the recent list
                if !p.exists() {
                    self.settings.recent.retain(|r| std::path::Path::new(r) != p);
                    self.settings.save();
                }
            }
        }
    }

    // ------------------------------------------------------------------ DC operating point

    fn run_op(&mut self) {
        let (p, tmp) = match self.sim_source("op") {
            Ok(x) => x,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = sim::operating_point(&p, 120);
            if let Some(t) = tmp {
                let _ = std::fs::remove_file(t);
            }
            let _ = tx.send(r);
        });
        self.op_rx = Some((self.tab_id, rx));
        self.status = "DC 動作点を計算中…".into();
    }

    /// The operating point is shown while the schematic is unchanged since it was computed.
    fn op_visible(&self) -> bool {
        self.op.as_ref().map(|o| o.tab_id == self.tab_id && o.comps == self.doc.sch.components && o.wires == self.doc.sch.wires).unwrap_or(false)
    }

    fn poll_op(&mut self) {
        let Some((tid, rx)) = &self.op_rx else { return };
        let Ok(r) = rx.try_recv() else { return };
        let tid = *tid;
        self.op_rx = None;
        match r {
            Ok(op) if tid == self.tab_id => {
                let n = op.net_volts.iter().filter(|v| v.is_some()).count();
                self.status = if op.errors.is_empty() { format!("DC 動作点: {n} ネット") } else { format!("DC 動作点（警告あり）: {}", op.errors.join(" / ")) };
                self.op = Some(OpView { tab_id: tid, comps: self.doc.sch.components.clone(), wires: self.doc.sch.wires.clone(), op });
                self.op_window = true;
            }
            Ok(_) => {}
            Err(e) => self.status = format!("DC 動作点の計算に失敗: {e}"),
        }
    }

    /// Voltage tags on the canvas, one per net at its longest wire (or first pin).
    fn paint_op(&self, painter: &egui::Painter, canvas: Rect) {
        let Some(o) = self.op.as_ref().filter(|_| self.op_visible()) else { return };
        let pal = self.pal();
        for (ni, net) in self.conn.nets.iter().enumerate() {
            if net.name.eq_ignore_ascii_case("gnd") {
                continue;
            }
            let Some(v) = o.op.net_volts.get(ni).copied().flatten() else { continue };
            let at = net
                .wires
                .iter()
                .filter_map(|&wi| self.doc.sch.wires.get(wi))
                .max_by_key(|w| w.len())
                .map(|w| ((w.x1 + w.x2) as f64 / 2.0, (w.y1 + w.y2) as f64 / 2.0))
                .or_else(|| net.pins.first().map(|&p| (self.conn.pins[p].x as f64, self.conn.pins[p].y as f64)));
            let Some((x, y)) = at else { continue };
            let pos = self.to_screen(canvas, x, y) + Vec2::new(4.0, 3.0);
            let galley = painter.layout_no_wrap(format!("{}V", crate::plot::eng(v)), FontId::proportional(12.0), pal.on_accent);
            let r = Rect::from_min_size(pos, galley.size() + Vec2::new(8.0, 2.0));
            painter.rect_filled(r, 4.0, pal.accent.gamma_multiply(0.92));
            painter.galley(pos + Vec2::new(4.0, 1.0), galley, pal.on_accent);
        }
    }

    fn op_window(&mut self, ctx: &egui::Context) {
        if !self.op_window || !self.op_visible() {
            return;
        }
        let Some(o) = &self.op else { return };
        let mut open = true;
        egui::Window::new("DC 動作点").open(&mut open).default_size([360.0, 420.0]).min_width(320.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("opv").striped(true).num_columns(2).show(ui, |ui| {
                    ui.strong("ネット");
                    ui.strong("電圧");
                    ui.end_row();
                    let mut rows: Vec<(&String, f64)> = o.op.net_names.iter().zip(&o.op.net_volts).filter(|(n, _)| !n.eq_ignore_ascii_case("gnd")).filter_map(|(n, v)| v.map(|v| (n, v))).collect();
                    rows.sort_by(|a, b| a.0.starts_with("_net").cmp(&b.0.starts_with("_net")).then(a.0.cmp(b.0)));
                    for (n, v) in rows {
                        ui.label(n);
                        ui.monospace(format!("{}V", crate::plot::eng(v)));
                        ui.end_row();
                    }
                    if !o.op.currents.is_empty() {
                        ui.end_row();
                        ui.strong("素子");
                        ui.strong("電流");
                        ui.end_row();
                        for (n, v) in &o.op.currents {
                            ui.label(n);
                            ui.monospace(format!("{}A", crate::plot::eng(*v)));
                            ui.end_row();
                        }
                    }
                });
                for e in &o.op.errors {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                }
            });
        });
        if !open {
            self.op_window = false;
        }
    }

    // ------------------------------------------------------------------ PDF / print

    fn export_pdf(&mut self) {
        let name = self.doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| format!("{}.pdf", s.to_string_lossy())).unwrap_or_else(|| "schematic.pdf".into());
        let Some(p) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).save_file() else { return };
        let title = self.doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        self.status = match crate::pdf::schematic_pdf(&self.doc.sch, &self.conn, &title).and_then(|b| std::fs::write(&p, b).map_err(|e| e.to_string())) {
            Ok(_) => format!("PDF 書き出し: {}", p.display()),
            Err(e) => format!("PDF 書き出し失敗: {e}"),
        };
    }

    /// Print = PDF in the temp folder, opened in the system viewer.
    fn print(&mut self) {
        let title = self.doc.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".into());
        let p = std::env::temp_dir().join("qucs-s-rs").join(format!("{title}_print.pdf"));
        let r = crate::pdf::schematic_pdf(&self.doc.sch, &self.conn, &title).and_then(|b| {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&p, b).map_err(|e| e.to_string())
        });
        self.status = match r.and_then(|_| crate::pdf::open_with_system(&p)) {
            Ok(_) => "印刷用 PDF をビューアで開きました。ビューアから印刷してください".into(),
            Err(e) => format!("印刷の準備に失敗: {e}"),
        };
    }

    // ------------------------------------------------------------------ hierarchy

    fn open_selected_sub(&mut self) {
        let Some(name) = self.sel.first().and_then(|&i| self.doc.sch.components.get(i)).map(|c| c.name.clone()) else { return };
        match crate::hier::sub_file(&self.doc, &name) {
            Ok(p) => self.open_path(&p),
            Err(e) => self.status = e,
        }
    }

    fn create_subcircuit(&mut self, file: &str) {
        let names: Vec<String> = self.sel.iter().filter_map(|&i| self.doc.sch.components.get(i)).map(|c| c.name.clone()).collect();
        if self.doc.dirty {
            if let Err(e) = self.doc.save() {
                self.status = e;
                return;
            }
        }
        self.doc.checkpoint();
        match crate::hier::create_subcircuit(&mut self.doc, &names, file, false) {
            Ok(r) => {
                self.status = format!("サブ回路 {} を作成（ポート {} 個）", r["subcircuit_file"].as_str().unwrap_or(file), r["ports"].as_array().map(|a| a.len()).unwrap_or(0));
                self.sel.clear();
                self.sel_wires.clear();
                self.changed();
                if let Some(i) = r["component"].as_str().and_then(|n| self.doc.sch.component_index(n)) {
                    self.sel = vec![i];
                }
            }
            Err(e) => {
                self.doc.undo();
                self.changed();
                self.status = format!("サブ回路を作成できません: {e}");
            }
        }
    }

    // ------------------------------------------------------------------ find

    fn find_window(&mut self, ctx: &egui::Context) {
        if !self.find_open {
            return;
        }
        let mut open = true;
        let mut goto: Option<(Vec<usize>, Vec<usize>, (f64, f64))> = None;
        egui::Window::new("検索").open(&mut open).default_size([320.0, 360.0]).show(ctx, |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.find_query).hint_text("部品名・種類・値・ネット名").desired_width(f32::INFINITY));
            if self.find_focus {
                r.request_focus();
                self.find_focus = false;
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let q = self.find_query.trim().to_lowercase();
            if q.is_empty() {
                ui.small("入力すると一致する部品とネットを表示します。クリックで選択して移動。");
                return;
            }
            let mut hits: Vec<(String, Vec<usize>, Vec<usize>, (f64, f64))> = Vec::new();
            for (i, c) in self.doc.sch.components.iter().enumerate() {
                let props: Vec<&str> = c.props.iter().map(|p| p.value.as_str()).collect();
                let hay = format!("{} {} {}", c.name, c.typ, props.join(" ")).to_lowercase();
                if hay.contains(&q) {
                    let b = self.conn.placed.get(i).map(|p| p.bbox).unwrap_or([c.x as f64; 4]);
                    let val = c.props.first().map(|p| p.value.clone()).unwrap_or_default();
                    hits.push((format!("{}  ({} {})", c.name, c.typ, val), vec![i], vec![], ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)));
                }
            }
            for net in &self.conn.nets {
                if net.name.to_lowercase().contains(&q) {
                    let at = net.wires.first().and_then(|&w| self.doc.sch.wires.get(w)).map(|w| (w.x1 as f64, w.y1 as f64)).or_else(|| net.pins.first().map(|&p| (self.conn.pins[p].x as f64, self.conn.pins[p].y as f64)));
                    if let Some(at) = at {
                        hits.push((format!("ネット {}  ({} ピン)", net.name, net.pins.len()), vec![], net.wires.clone(), at));
                    }
                }
            }
            ui.small(format!("{} 件", hits.len()));
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (k, (label, comps, wires, at)) in hits.iter().enumerate() {
                    if ui.selectable_label(false, label).clicked() || (enter && k == 0) {
                        goto = Some((comps.clone(), wires.clone(), *at));
                    }
                }
            });
        });
        self.find_open = open;
        if let Some((comps, wires, (x, y))) = goto {
            self.tool = Tool::Select;
            self.sel = comps;
            self.sel_wires = wires;
            self.sel_paint.clear();
            let c = self.canvas_rect;
            self.offset = Vec2::new(x as f32, y as f32) - c.size() / 2.0 / self.zoom;
        }
    }

    fn zoom_by(&mut self, factor: f32) {
        let c = self.canvas_rect;
        let center = c.center();
        let before = self.to_sch_f(c, center);
        self.zoom = (self.zoom * factor).clamp(0.1, 12.0);
        let after = self.to_sch_f(c, center);
        self.offset += Vec2::new((before.0 - after.0) as f32, (before.1 - after.1) as f32);
    }

    // ------------------------------------------------------------------ dialogs

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Qucs-S RS について").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.heading(format!("Qucs-S RS {}", env!("CARGO_PKG_VERSION")));
            ui.label("Qucs-S の Rust 製フォーク（軽量回路図エディタ＋MCP サーバー）");
            ui.label("ライセンス: GPL-2.0-or-later");
            ui.separator();
            let found = |r: Result<PathBuf, String>| r.map(|p| p.display().to_string()).unwrap_or_else(|_| "見つかりません".into());
            egui::Grid::new("about").num_columns(2).show(ui, |ui| {
                ui.label("ngspice");
                ui.monospace(found(sim::ngspice_path()));
                ui.end_row();
                ui.label("Qucs-S");
                ui.monospace(found(sim::qucs_s_path()));
                ui.end_row();
                ui.label("ネットリスト");
                ui.label(if sim::qucs_s_path().is_ok() { "内蔵（未対応の部品は Qucs-S）" } else { "内蔵のみ" });
                ui.end_row();
                ui.label("設定");
                ui.monospace(crate::settings::config_dir().map(|p| p.display().to_string()).unwrap_or_default());
                ui.end_row();
            });
        });
        self.about_open = open;
    }

    fn subckt_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.subckt_prompt.take() else { return };
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new("サブ回路を作成").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(format!("選択した {} 個の部品を新しいファイルへ移し、Sub 部品で置き換えます。", self.sel.len()));
            ui.label("外へつながるネットごとにポートができます。");
            ui.horizontal(|ui| {
                ui.label("ファイル名");
                let r = ui.text_edit_singleline(&mut name);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ok = true;
                }
            });
            ui.small("（現在の回路図と同じフォルダに保存）");
            ui.horizontal(|ui| {
                ok |= ui.button("作成").clicked();
                cancel = ui.button("キャンセル").clicked();
            });
        });
        if ok && !name.trim().is_empty() {
            self.create_subcircuit(name.trim());
        } else if !cancel {
            self.subckt_prompt = Some(name);
        }
    }

    /// "Save changes?" for closing a tab or the window.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm else { return };
        enum Answer {
            Save,
            Discard,
            Cancel,
        }
        let mut answer = None;
        let unsaved: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tab_unsaved(i)).collect();
        egui::Window::new("変更の保存").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            match c {
                Confirm::Tab(i) => {
                    ui.label(format!("「{}」への変更を保存しますか？", self.tab_title(i).trim_end_matches(" *")));
                }
                Confirm::Quit => {
                    ui.label("保存していない変更があります:");
                    for &i in &unsaved {
                        ui.label(format!("  • {}", self.tab_title(i).trim_end_matches(" *")));
                    }
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let save = if c == Confirm::Quit { "すべて保存して終了" } else { "保存" };
                let discard = if c == Confirm::Quit { "保存せずに終了" } else { "保存しない" };
                if ui.button(save).clicked() {
                    answer = Some(Answer::Save);
                }
                if ui.button(discard).clicked() {
                    answer = Some(Answer::Discard);
                }
                if ui.button("キャンセル").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    answer = Some(Answer::Cancel);
                }
            });
        });
        let Some(a) = answer else { return };
        self.confirm = None;
        match (c, a) {
            (_, Answer::Cancel) => {}
            (Confirm::Tab(i), Answer::Discard) => self.force_close_tab(i),
            (Confirm::Tab(i), Answer::Save) => {
                self.switch_tab(i);
                if self.save_current() {
                    self.force_close_tab(self.active);
                }
            }
            (Confirm::Quit, Answer::Discard) => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            (Confirm::Quit, Answer::Save) => {
                for i in unsaved {
                    self.switch_tab(i);
                    if !self.save_current() {
                        self.status = "保存されなかったため終了を中止しました".into();
                        return;
                    }
                }
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    // ------------------------------------------------------------------ tabs

    fn stash_tab(&mut self) -> TabState {
        TabState {
            tab_id: self.tab_id,
            doc: std::mem::replace(&mut self.doc, Doc::new(Schematic::new_empty("untitled"), None)),
            conn: std::mem::replace(&mut self.conn, Connectivity::build(&Schematic::default(), None)),
            scene: std::mem::take(&mut self.scene),
            gen: self.gen,
            offset: self.offset,
            zoom: self.zoom,
            need_fit: self.need_fit,
            sel: std::mem::take(&mut self.sel),
            sel_wires: std::mem::take(&mut self.sel_wires),
            sel_paint: std::mem::take(&mut self.sel_paint),
            disk_mtime: self.disk_mtime,
            external_change: self.external_change,
            issues: std::mem::take(&mut self.issues),
            sim: self.sim.take(),
            show_sim: self.show_sim,
            plot_analysis: self.plot_analysis,
            plot_vectors: std::mem::take(&mut self.plot_vectors),
            markers: std::mem::take(&mut self.markers),
        }
    }

    fn install_tab(&mut self, t: TabState) {
        self.tab_id = t.tab_id;
        self.doc = t.doc;
        self.conn = t.conn;
        self.scene = t.scene;
        self.gen = t.gen + 1;
        // the cached scene key belongs to the previous tab and its gen can equal ours
        // (every new tab starts at gen 2), so invalidate it: gen is never 0
        self.scene_key.0 = 0;
        self.offset = t.offset;
        self.zoom = t.zoom;
        self.need_fit = t.need_fit;
        self.sel = t.sel;
        self.sel_wires = t.sel_wires;
        self.sel_paint = t.sel_paint;
        self.disk_mtime = t.disk_mtime;
        self.external_change = t.external_change;
        self.issues = t.issues;
        self.sim = t.sim;
        self.show_sim = t.show_sim;
        self.plot_analysis = t.plot_analysis;
        self.plot_vectors = t.plot_vectors;
        self.markers = t.markers;
        // transient interaction state belongs to no tab
        self.drag = None;
        self.grab = None;
        self.box_start = None;
        self.wire_pts.clear();
        self.autowire_first = None;
        self.paint_start = None;
        self.prop_for = None;
        self.plot_tex = None;
        self.plot_dirty = true;
        self.marker_out = serde_json::Value::Null;
        self.plot_area = None;
        self.last_poll = Instant::now() - std::time::Duration::from_secs(1);
        if self.show_issues {
            self.issues = check::check(&self.doc.sch, &self.conn).issues;
        }
    }

    fn new_tab(&mut self, d: Doc) {
        let cur = self.stash_tab();
        self.tabs[self.active] = Some(cur);
        self.tabs.push(None);
        self.active = self.tabs.len() - 1;
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.install_tab(TabState::new(d, id));
    }

    fn switch_tab(&mut self, i: usize) {
        if i == self.active || i >= self.tabs.len() {
            return;
        }
        let cur = self.stash_tab();
        self.tabs[self.active] = Some(cur);
        let t = self.tabs[i].take().expect("inactive tab state");
        self.active = i;
        self.install_tab(t);
        self.poll_disk();
    }

    /// Close a tab; asks first when it has unsaved changes.
    fn close_tab(&mut self, i: usize) {
        if i < self.tabs.len() && self.tab_unsaved(i) {
            self.confirm = Some(Confirm::Tab(i));
            return;
        }
        self.force_close_tab(i);
    }

    fn force_close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            let id = self.tab_id;
            self.install_tab(TabState::new(Doc::new(Schematic::new_empty("untitled"), None), id));
            return;
        }
        if i == self.active {
            let next = if i + 1 < self.tabs.len() { i + 1 } else { i - 1 };
            let t = self.tabs[next].take().expect("inactive tab state");
            self.tabs.remove(i);
            self.active = if next > i { next - 1 } else { next };
            self.install_tab(t);
        } else {
            self.tabs.remove(i);
            if i < self.active {
                self.active -= 1;
            }
        }
    }

    fn tab_title(&self, i: usize) -> String {
        let (path, dirty) = if i == self.active {
            (self.doc.path.clone(), self.unsaved(&self.doc))
        } else {
            match &self.tabs[i] {
                Some(t) => (t.doc.path.clone(), self.unsaved(&t.doc)),
                None => (None, false),
            }
        };
        let name = path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "無題".into());
        format!("{name}{}", if dirty { " *" } else { "" })
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut switch = None;
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for i in 0..self.tabs.len() {
                let title = self.tab_title(i);
                let full = if i == self.active { self.doc.path.clone() } else { self.tabs[i].as_ref().and_then(|t| t.doc.path.clone()) };
                let r = ui.selectable_label(i == self.active, &title).on_hover_text(full.map(|p| p.display().to_string()).unwrap_or_default());
                if r.clicked() {
                    switch = Some(i);
                }
                if r.middle_clicked() {
                    close = Some(i);
                }
                if ui.small_button("✖").on_hover_text("タブを閉じる (Ctrl+W)").clicked() {
                    close = Some(i);
                }
                ui.separator();
            }
            if ui.small_button("＋").on_hover_text("新しいタブ (Ctrl+T)").clicked() {
                self.new_tab(Doc::new(Schematic::new_empty("untitled"), None));
            }
        });
        if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = switch {
            self.switch_tab(i);
        }
    }

    // ------------------------------------------------------------------ single instance

    /// Tell MCP servers that this editor is alive (they then send files here instead of starting another window).
    fn heartbeat(&mut self) {
        if self.last_heartbeat.elapsed().as_millis() < 1000 {
            return;
        }
        self.last_heartbeat = Instant::now();
        let p = crate::mcp::gui_heartbeat_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, std::process::id().to_string());
    }

    fn poll_open_requests(&mut self) {
        let p = crate::mcp::gui_open_request_path();
        let Ok(text) = std::fs::read_to_string(&p) else { return };
        let _ = std::fs::remove_file(&p);
        for line in text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
            self.open_path(std::path::Path::new(line));
        }
    }

    // ------------------------------------------------------------------ Monte Carlo

    fn mc_textures(&mut self, ctx: &egui::Context) {
        let pal = theme::pal(ctx);
        self.mc.tex = self
            .mc
            .raw
            .iter()
            .enumerate()
            .map(|(k, img)| {
                let ci = egui::ColorImage::from_rgba_premultiplied([img.0 as usize, img.1 as usize], &pal.map_image(&img.2));
                ctx.load_texture(format!("mc{k}"), ci, egui::TextureOptions::LINEAR)
            })
            .collect();
    }

    fn mc_window(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.mc.rx {
            if let Ok(r) = rx.try_recv() {
                self.mc.rx = None;
                match r {
                    Ok((summary, ov, hist)) => {
                        self.mc.raw = [ov, hist].into_iter().flatten().collect();
                        self.mc_textures(ctx);
                        self.status = format!("モンテカルロ完了: {} 回", summary["successful"]);
                        self.mc.summary = Some(summary);
                        self.mc.error.clear();
                    }
                    Err(e) => self.mc.error = e,
                }
            }
        }
        if !self.mc.open {
            return;
        }
        let mut open = true;
        let mut start = false;
        egui::Window::new("モンテカルロ解析").open(&mut open).default_size([900.0, 700.0]).show(ctx, |ui| {
            let m = &mut self.mc;
            ui.small("回路の解析（AC・過渡など）を、部品値をばらつかせて何度も実行します。1 回目は公称値です。");
            egui::Grid::new("mcg").num_columns(4).show(ui, |ui| {
                ui.label("回数");
                ui.add(egui::DragValue::new(&mut m.runs).range(2..=2000));
                ui.label("分布");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut m.gauss, true, "正規 (許容差=3σ)");
                    ui.radio_value(&mut m.gauss, false, "一様");
                });
                ui.end_row();
                ui.label("許容差 R [%]");
                ui.add(egui::DragValue::new(&mut m.tol_r).range(0.0..=50.0).speed(0.1));
                ui.label("C [%]");
                ui.add(egui::DragValue::new(&mut m.tol_c).range(0.0..=50.0).speed(0.1));
                ui.end_row();
                ui.label("L [%]");
                ui.add(egui::DragValue::new(&mut m.tol_l).range(0.0..=50.0).speed(0.1));
                ui.label("解析");
                ui.add(egui::TextEdit::singleline(&mut m.analysis).hint_text("ac1 / tr1（空=最初）").desired_width(120.0));
                ui.end_row();
                ui.label("ベクトル");
                ui.add(egui::TextEdit::singleline(&mut m.vector).hint_text("v(out)").desired_width(120.0));
                ui.label("測定点 (x)");
                ui.add(egui::TextEdit::singleline(&mut m.at).hint_text("10 MHz, 20 MHz").desired_width(160.0));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if m.rx.is_some() {
                    ui.spinner();
                    ui.label("実行中…");
                } else if ui.button("▶ 実行").clicked() {
                    start = true;
                }
            });
            if !m.error.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &m.error);
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                if let Some(s) = &m.summary {
                    ui.label(format!("成功 {} / {} 回   分布: {}", s["successful"], s["runs"], s["distribution"].as_str().unwrap_or("")));
                    if let Some(y) = s.get("yield").filter(|y| !y.is_null()) {
                        ui.label(format!("歩留まり: {}%", y["yield_percent"]));
                    }
                    egui::Grid::new("mcstats").striped(true).show(ui, |ui| {
                        for h in ["ベクトル", "x", "公称", "平均", "σ", "最小", "最大"] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for mm in s["measurements"].as_array().cloned().unwrap_or_default() {
                            let st = &mm["stats"];
                            let f = |v: &serde_json::Value| v.as_f64().map(|x| format!("{x:.4}")).unwrap_or_default();
                            ui.label(format!("{} {}", mm["vector"].as_str().unwrap_or(""), mm["unit"].as_str().unwrap_or("")));
                            ui.label(mm["at"].as_f64().map(crate::plot::eng).unwrap_or_else(|| "終点".into()));
                            ui.label(f(&mm["nominal"]));
                            ui.label(f(&st["mean"]));
                            ui.label(f(&st["std"]));
                            ui.label(f(&st["min"]));
                            ui.label(f(&st["max"]));
                            ui.end_row();
                        }
                    });
                }
                for t in &m.tex {
                    let w = ui.available_width();
                    let sz = t.size_vec2();
                    ui.image((t.id(), sz * (w / sz.x).min(1.0)));
                }
            });
        });
        self.mc.open = open;
        if start {
            let (p, tmp) = match self.sim_source("mc") {
                Ok(x) => x,
                Err(e) => {
                    self.mc.error = e;
                    return;
                }
            };
            let m = &self.mc;
            let at: Vec<serde_json::Value> = m.at.split(',').filter_map(|x| crate::rftools::parse_value(x.trim())).map(|x| serde_json::json!(x)).collect();
            let args = serde_json::json!({
                "runs": m.runs, "distribution": if m.gauss { "gauss" } else { "uniform" },
                "tolerances": {"R": m.tol_r, "C": m.tol_c, "L": m.tol_l},
                "analysis": if m.analysis.trim().is_empty() { serde_json::Value::Null } else { serde_json::json!(m.analysis.trim()) },
                "vectors": if m.vector.trim().is_empty() { serde_json::json!([]) } else { serde_json::json!([m.vector.trim()]) },
                "at": at,
            });
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let spec = crate::montecarlo::spec_from_json(&args);
                let r = crate::montecarlo::run(&p, &spec).map(|res| {
                    let ov = crate::montecarlo::overlay_svg(&res, &spec, 200).ok().and_then(|s| crate::render::svg_to_rgba(&s).ok());
                    let first = res.summary["measurements"].as_array().and_then(|m| m.first()).cloned();
                    let hist = first.and_then(|m| crate::montecarlo::histogram_svg(&res, &spec, m["vector"].as_str().unwrap_or(""), m["at"].as_f64())).and_then(|s| crate::render::svg_to_rgba(&s).ok());
                    (res.summary, ov, hist)
                });
                if let Some(t) = tmp {
                    let _ = std::fs::remove_file(t);
                }
                let _ = tx.send(r);
            });
            self.mc.rx = Some(rx);
        }
    }

    /// Ask for a file name and save there. False when cancelled or failed.
    fn save_as(&mut self) -> bool {
        let name = self.doc.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "untitled.sch".into());
        let Some(mut p) = rfd::FileDialog::new().add_filter("Qucs schematic", &["sch"]).set_file_name(name).save_file() else { return false };
        if p.extension().is_none() {
            p.set_extension("sch");
        }
        let old = self.doc.path.replace(p.clone());
        match self.doc.save() {
            Ok(_) => {
                self.disk_mtime = mtime(&p);
                self.status = format!("保存: {}", p.display());
                self.settings.add_recent(&p);
                self.settings.save();
                true
            }
            Err(e) => {
                self.doc.path = old;
                self.status = format!("保存エラー: {e}");
                false
            }
        }
    }

    // ------------------------------------------------------------------ painting

    fn rebuild_scene(&mut self) {
        let key = (self.gen, self.color_nets, self.pin_names, self.show_issues, self.sel.clone(), self.sel_wires.clone());
        if key == self.scene_key {
            return;
        }
        let ov = Overlay {
            color_nets: self.color_nets,
            net_tags: self.color_nets,
            pin_names: self.pin_names,
            selected: self.sel.clone(),
            selected_wires: self.sel_wires.clone(),
            markers: if self.show_issues {
                self.issues.iter().filter(|i| i.severity == "error").map(|i| (i.at.0, i.at.1, i.message.clone())).collect()
            } else {
                vec![]
            },
            wire_color: Some(theme::to_color(self.pal().wire)),
            ..Default::default()
        };
        self.scene = build_scene(&self.doc.sch, &self.conn, &ov);
        self.scene_key = key;
    }

    fn pal(&self) -> &'static Palette {
        match self.tex_theme {
            egui::Theme::Dark => &theme::DARK,
            egui::Theme::Light => &theme::LIGHT,
        }
    }

    fn paint_shapes(&self, painter: &egui::Painter, canvas: Rect, shapes: &[Shape], alpha: f32) {
        paint_shapes_xf(painter, shapes, alpha, self.zoom, &|x, y| self.to_screen(canvas, x, y));
    }

    fn paint_grid(&self, painter: &egui::Painter, canvas: Rect) {
        let step = if self.zoom * 10.0 >= 7.0 { 10.0 } else if self.zoom * 50.0 >= 7.0 { 50.0 } else { 100.0 };
        let (x0, y0) = self.to_sch_f(canvas, canvas.min);
        let (x1, y1) = self.to_sch_f(canvas, canvas.max);
        let gx0 = (x0 / step).floor() as i64;
        let gx1 = (x1 / step).ceil() as i64;
        let gy0 = (y0 / step).floor() as i64;
        let gy1 = (y1 / step).ceil() as i64;
        if (gx1 - gx0) * (gy1 - gy0) > 40_000 {
            return;
        }
        let c = self.pal().grid;
        for gx in gx0..=gx1 {
            for gy in gy0..=gy1 {
                let p = self.to_screen(canvas, gx as f64 * step, gy as f64 * step);
                painter.rect_filled(Rect::from_center_size(p, Vec2::splat(1.4)), 0.0, c);
            }
        }
    }

    // ------------------------------------------------------------------ UI parts

    fn theme_menu(&mut self, ui: &mut egui::Ui) {
        let cur = match self.theme_pref {
            egui::ThemePreference::Dark => "☾ ダーク",
            egui::ThemePreference::Light => "☀ ライト",
            egui::ThemePreference::System => "◐ システム",
        };
        ui.menu_button(cur, |ui| {
            for (p, label) in [
                (egui::ThemePreference::Light, "☀ ライト（アイボリー）"),
                (egui::ThemePreference::Dark, "☾ ダーク（オブシディアン・ゴールド）"),
                (egui::ThemePreference::System, "◐ システム設定に合わせる"),
            ] {
                if ui.selectable_label(self.theme_pref == p, label).clicked() {
                    self.theme_pref = p;
                    ui.ctx().set_theme(p);
                    theme::save_pref(p);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("表示テーマ");
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, canvas_hint: Rect) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("📂 開く").on_hover_text("Ctrl+O").clicked() {
                self.open_dialog();
            }
            let recent = self.settings.existing_recent();
            ui.add_enabled_ui(!recent.is_empty(), |ui| {
                ui.menu_button("🕘 最近", |ui| {
                    for p in &recent {
                        let label = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        if ui.button(label).on_hover_text(p.display().to_string()).clicked() {
                            self.open_path(p);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("履歴を消去").clicked() {
                        self.settings.recent.clear();
                        self.settings.save();
                        ui.close();
                    }
                })
                .response
                .on_hover_text("最近使ったファイル");
            });
            if ui.button("💾 保存").on_hover_text("Ctrl+S").clicked() {
                self.save_current();
            }
            if ui.button("名前を付けて保存").on_hover_text("Ctrl+Shift+S").clicked() {
                self.save_as();
            }
            ui.separator();
            if ui.button("↶").on_hover_text("元に戻す (Ctrl+Z)").clicked() && self.doc.undo() {
                self.changed();
            }
            if ui.button("↷").on_hover_text("やり直し (Ctrl+Y)").clicked() && self.doc.redo() {
                self.changed();
            }
            ui.separator();
            for (t, label, tip) in [
                (Tool::Select, "⬉ 選択", "選択・移動 (Esc)"),
                (Tool::Wire, "╱ 配線", "クリックで折れ点、ダブルクリック/右クリックで終了 (W)"),
                (Tool::AutoWire, "⚡ 自動配線", "ピンを2つクリックするとルーターが配線 (A)"),
                (Tool::Label, "🏷 ラベル", "配線/ピンをクリックしてネット名を付ける (L)"),
            ] {
                if ui.selectable_label(self.tool == t, label).on_hover_text(tip).clicked() {
                    self.tool = t;
                    self.wire_pts.clear();
                    self.autowire_first = None;
                }
            }
            ui.separator();
            if ui.button("⟲ 全再配線").on_hover_text("全ネットを引き直す (交差・折れを最小化)").clicked() {
                self.doc.checkpoint();
                match self.doc.reroute(&[]) {
                    Ok(r) => self.status = format!("再配線: {r}"),
                    Err(e) => self.status = e,
                }
                self.changed();
            }
            if ui.button("✨ 整頓").on_hover_text("グリッド吸着・配線結合・ラベル再配置").clicked() {
                self.doc.checkpoint();
                match self.doc.tidy(true, false) {
                    Ok(r) => self.status = format!("整頓: {r}"),
                    Err(e) => self.status = e,
                }
                self.changed();
            }
            if ui.button("🔍 チェック").clicked() {
                self.issues = check::check(&self.doc.sch, &self.conn).issues;
                self.show_issue_panel = true;
                self.show_issues = true;
                self.gen += 1;
            }
            let pal = self.pal();
            let run = egui::Button::new(egui::RichText::new("▶ シミュレーション").color(pal.on_accent)).fill(pal.accent).stroke(Stroke::NONE);
            if ui.add(run).on_hover_text("F5").clicked() {
                self.run_sim();
            }
            let op_on = self.op_visible();
            if ui.selectable_label(op_on, "⚡ 動作点").on_hover_text("DC 動作点を計算して、各ネットの電圧を回路図に表示").clicked() {
                if op_on {
                    self.op = None;
                    self.op_window = false;
                } else {
                    self.run_op();
                }
            }
            ui.menu_button("✏ 図形", |ui| {
                for (k, label) in [("line", "線"), ("rect", "矩形"), ("ellipse", "楕円"), ("arrow", "矢印"), ("text", "テキスト (T)")] {
                    if ui.button(label).clicked() {
                        self.tool = Tool::Paint(k.into());
                        ui.close();
                    }
                }
            });
            ui.menu_button("✂ 編集", |ui| {
                if ui.button("複製 (Ctrl+D)").clicked() {
                    self.duplicate_selection();
                    ui.close();
                }
                if ui.button("Y軸ミラー (Y)").clicked() {
                    self.mirror_y_selected();
                    ui.close();
                }
                if ui.button("検索… (Ctrl+F)").clicked() {
                    self.find_open = true;
                    self.find_focus = true;
                    ui.close();
                }
                ui.separator();
                let can_sub = !self.sel.is_empty() && self.doc.path.is_some();
                if ui
                    .add_enabled(can_sub, egui::Button::new("サブ回路を作成…"))
                    .on_disabled_hover_text("部品を選択し、回路図を保存してから実行します")
                    .on_hover_text("選択した部品を新しいサブ回路ファイルへ移し、Sub 部品で置き換える")
                    .clicked()
                {
                    let used: std::collections::HashSet<String> = std::fs::read_dir(self.doc.dir().unwrap_or_default())
                        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect())
                        .unwrap_or_default();
                    let name = (1..).map(|k| format!("sub{k}.sch")).find(|n| !used.contains(n)).unwrap_or_else(|| "sub.sch".into());
                    self.subckt_prompt = Some(name);
                    ui.close();
                }
                let sub_sel = self.sel.len() == 1 && self.doc.sch.components.get(self.sel[0]).map(|c| c.typ == "Sub").unwrap_or(false);
                if ui.add_enabled(sub_sel, egui::Button::new("サブ回路を開く")).on_hover_text("Sub 部品をダブルクリックしても開けます").clicked() {
                    self.open_selected_sub();
                    ui.close();
                }
                ui.separator();
                for (m, label) in [("left", "左揃え"), ("right", "右揃え"), ("top", "上揃え"), ("bottom", "下揃え"), ("vcenter", "縦中心揃え"), ("hcenter", "横中心揃え"), ("distribute_h", "横に等間隔"), ("distribute_v", "縦に等間隔")] {
                    if ui.add_enabled(self.sel.len() >= 2, egui::Button::new(label)).clicked() {
                        self.align_selected(m);
                        ui.close();
                    }
                }
            });
            self.tools_menu(ui);
            ui.menu_button("🖼 書き出し", |ui| {
                if ui.button("PNG / SVG…").clicked() {
                    self.export_image();
                    ui.close();
                }
                if ui.button("PDF…").clicked() {
                    self.export_pdf();
                    ui.close();
                }
                if ui.button("印刷… (Ctrl+P)").on_hover_text("PDF を作成して既定のビューアで開きます（ビューアから印刷）").clicked() {
                    self.print();
                    ui.close();
                }
            });
            if ui.button("⛶ 全体表示").on_hover_text("F").clicked() {
                self.fit(canvas_hint);
            }
            ui.separator();
            let mut ch = false;
            ch |= ui.checkbox(&mut self.color_nets, "ネット色").changed();
            ch |= ui.checkbox(&mut self.pin_names, "ピン名").changed();
            ch |= ui.checkbox(&mut self.show_issues, "問題表示").changed();
            let mut other = ui.checkbox(&mut self.show_grid, "グリッド").changed();
            other |= ui.checkbox(&mut self.auto_reroute, "移動時に再配線").changed();
            other |= ui.checkbox(&mut self.auto_save, "自動保存 (AI同期)").on_hover_text("編集を即座に保存し、MCP 側（AIエージェント）と同じファイルを共有します").changed();
            if ch && self.show_issues {
                self.issues = check::check(&self.doc.sch, &self.conn).issues;
            }
            if ch || other {
                self.store_settings();
            }
        });
    }

    /// Symbol drawing of a component type at the origin (cached).
    fn icon(&mut self, typ: &str) -> &(Vec<Shape>, [f64; 4]) {
        self.icons.entry(typ.to_string()).or_insert_with(|| {
            let mut shapes = Vec::new();
            let mut bb = [-20.0, -20.0, 20.0, 20.0];
            if let Ok(c) = spec_component(typ, 0, 0) {
                let p = library::place(&c, None);
                for prim in &p.prims {
                    crate::render::prim_shapes(prim, &mut shapes, None);
                }
                bb = p.bbox;
                for pin in &p.pins {
                    bb = [bb[0].min(pin.x as f64), bb[1].min(pin.y as f64), bb[2].max(pin.x as f64), bb[3].max(pin.y as f64)];
                }
            }
            (shapes, bb)
        })
    }

    /// Palette entry: symbol icon + name, like the Qucs-S component list.
    fn palette_row(&mut self, ui: &mut egui::Ui, typ: &str, caption: &str, sub_line: &str, selected: bool) -> egui::Response {
        const ICON: Vec2 = Vec2::new(46.0, 34.0);
        let w = ui.available_width();
        // lay the text out first: long captions wrap, and the row grows to fit them
        let text_color = ui.visuals().text_color();
        let galley = ui.painter().layout(format!("{caption}\n{sub_line}"), FontId::proportional(12.5), text_color, (w - ICON.x - 12.0).max(40.0));
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ICON.y.max(galley.size().y) + 4.0), Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let vis = ui.style().interact_selectable(&resp, selected);
        if selected || resp.hovered() {
            ui.painter().rect_filled(rect, 3.0, vis.weak_bg_fill);
        }
        let icon_rect = Rect::from_center_size(Pos2::new(rect.min.x + 2.0 + ICON.x / 2.0, rect.center().y), ICON);
        let (shapes, bb) = self.icon(typ).clone();
        let (bw, bh) = ((bb[2] - bb[0]).max(1.0), (bb[3] - bb[1]).max(1.0));
        let scale = ((ICON.x as f64 - 4.0) / bw).min((ICON.y as f64 - 4.0) / bh).min(0.8);
        let (cx, cy) = ((bb[0] + bb[2]) / 2.0, (bb[1] + bb[3]) / 2.0);
        let center = icon_rect.center();
        let painter = ui.painter().with_clip_rect(icon_rect);
        paint_shapes_xf(&painter, &shapes, 1.0, scale as f32, &|x, y| center + Vec2::new(((x - cx) * scale) as f32, ((y - cy) * scale) as f32));
        let text_pos = Pos2::new(icon_rect.max.x + 6.0, rect.center().y);
        ui.painter().galley(text_pos - Vec2::new(0.0, galley.size().y / 2.0), galley, vis.text_color());
        resp
    }

    fn palette(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.palette_tab, 0, egui::RichText::new("部品").heading());
            ui.selectable_value(&mut self.palette_tab, 1, egui::RichText::new("ライブラリ").heading());
        });
        if self.palette_tab == 1 {
            self.library_browser(ui);
            return;
        }
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("検索 (R, opamp, diode…)"));
        let q = self.query.to_ascii_lowercase();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut cats: Vec<String> = lib().comps.iter().map(|c| c.category.clone()).collect();
            cats.sort();
            cats.dedup();
            for cat in cats {
                let items: Vec<&library::CompInfo> = lib()
                    .comps
                    .iter()
                    .filter(|c| c.category == cat)
                    .filter(|c| q.is_empty() || format!("{} {} {}", c.typ, c.caption, c.description).to_ascii_lowercase().contains(&q))
                    .collect();
                if items.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(format!("{cat} ({})", items.len())).default_open(!q.is_empty() || cat == "lumped components" || cat == "sources").show(ui, |ui| {
                    for c in items {
                        let selected = self.tool == Tool::Place(c.typ.clone());
                        if self.palette_row(ui, &c.typ, &c.caption, &c.typ, selected).on_hover_text(&c.description).clicked() {
                            self.tool = Tool::Place(c.typ.clone());
                            self.place_rot = library::new_component(&c.typ, "X", 0, 0).map(|c| c.rotate).unwrap_or(0);
                        }
                    }
                });
            }
        });
    }

    /// Qucs-S component libraries (real parts) with symbol icons.
    fn library_browser(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::TextEdit::singleline(&mut self.lib_query).hint_text("検索 (lm358, 1n4148, irf…)"));
        let libs = crate::qlib::libraries();
        if libs.is_empty() {
            ui.label("Qucs-S のライブラリが見つかりません（QUCS_S_LIBRARY で指定できます）");
            return;
        }
        let words: Vec<String> = self.lib_query.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
        egui::ScrollArea::vertical().id_salt("libs").show(ui, |ui| {
            for l in libs {
                let items: Vec<&crate::qlib::LibComp> = l
                    .comps
                    .iter()
                    .filter(|c| {
                        let hay = format!("{} {} {}", l.name, c.name, c.description).to_lowercase();
                        words.iter().all(|w| hay.contains(w))
                    })
                    .collect();
                if items.is_empty() {
                    continue;
                }
                let title = format!("{} ({}){}", l.name, items.len(), if l.ngspice_ok { "" } else { "  ⚠ngspice非対応" });
                egui::CollapsingHeader::new(title).id_salt(&l.name).default_open(!words.is_empty() && items.len() <= 30).show(ui, |ui| {
                    for c in items.iter().take(400) {
                        let key = format!("Lib:{}/{}", l.name, c.name);
                        let selected = self.tool == Tool::Place(key.clone());
                        let desc: String = c.description.chars().take(60).collect();
                        if self.palette_row(ui, &key, &c.name, &desc, selected).on_hover_text(&c.description).clicked() {
                            self.tool = Tool::Place(key);
                            self.place_rot = 0;
                        }
                    }
                });
            }
        });
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        if self.sel.len() != 1 {
            ui.heading("プロパティ");
            if self.sel.is_empty() {
                ui.label("部品を選択すると編集できます。");
            } else {
                ui.label(format!("{} 個選択中", self.sel.len()));
                ui.horizontal_wrapped(|ui| {
                    for (m, label) in [("left", "⇤ 左"), ("right", "⇥ 右"), ("top", "⤒ 上"), ("bottom", "⤓ 下"), ("vcenter", "縦中心"), ("hcenter", "横中心"), ("distribute_h", "横等間隔"), ("distribute_v", "縦等間隔")] {
                        if ui.button(label).clicked() {
                            self.align_selected(m);
                        }
                    }
                });
            }
            ui.separator();
            ui.label("ショートカット");
            ui.small("Ctrl+C/X/V/D コピー/切り取り/貼り付け/複製\nCtrl+A 全選択 / Y Y軸ミラー / T テキスト\nR 回転 / M ミラー / Del 削除\nW 配線 / A 自動配線 / L ラベル\nCtrl+Z/Y 元に戻す/やり直し\nCtrl+S 保存 / Ctrl+Shift+S 名前を付けて保存\nCtrl+F 検索 / Ctrl+P 印刷 / F5 シミュレーション\nホイール・Ctrl +/-: ズーム, 中/右ドラッグ: パン\nF・Ctrl+0 全体表示 / F1 バージョン情報");
            return;
        }
        let ci = self.sel[0];
        if self.prop_for != Some(ci) {
            self.prop_for = Some(ci);
            self.prop_buf = self.doc.sch.components[ci].props.iter().map(|p| p.value.clone()).collect();
            self.name_buf = self.doc.sch.components[ci].name.clone();
        }
        let c = self.doc.sch.components[ci].clone();
        let info = lib().info(&c.typ);
        ui.heading(format!("{} ({})", c.name, c.typ));
        if let Some(i) = info {
            ui.small(&i.description);
        }
        ui.horizontal(|ui| {
            ui.label("名前");
            let r = ui.text_edit_singleline(&mut self.name_buf);
            if r.lost_focus() && self.name_buf != c.name && !self.name_buf.is_empty() {
                self.doc.checkpoint();
                self.doc.sch.components[ci].name = self.name_buf.clone();
                self.changed();
            }
        });
        ui.label(format!("位置 ({}, {})  回転 {}  ミラー {}", c.x, c.y, c.rotate, c.mirror));
        if c.typ == "Sub" && ui.button("⤵ サブ回路を開く").on_hover_text("ダブルクリックでも開けます").clicked() {
            self.open_selected_sub();
            return;
        }
        ui.horizontal(|ui| {
            if ui.button("⟲ 回転 (R)").clicked() {
                self.rotate_selected();
            }
            if ui.button("⇅ ミラー (M)").clicked() {
                self.mirror_selected();
            }
            let mut on = c.active & 3 == 1;
            if ui.checkbox(&mut on, "有効").changed() {
                self.doc.checkpoint();
                let cc = &mut self.doc.sch.components[ci];
                cc.active = (cc.active & !3) | if on { 1 } else { 0 };
                self.changed();
            }
        });
        ui.separator();
        let mut commit: Option<(usize, String)> = None;
        let mut toggle: Option<usize> = None;
        egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| {
            // One row per property: name | value | "show" checkbox. The value field takes whatever width is left after
            // the checkbox (right-to-left layout), so it follows the panel when it is dragged wider or narrower and can
            // never push the panel open on its own.
            let body = egui::TextStyle::Body.resolve(ui.style());
            let label_w = (0..c.props.len())
                .map(|k| ui.painter().layout_no_wrap(library::prop_name(&c, k), body.clone(), egui::Color32::PLACEHOLDER).size().x)
                .fold(0.0_f32, f32::max);
            let row_h = ui.spacing().interact_size.y;
            for (k, p) in c.props.iter().enumerate() {
                let pname = library::prop_name(&c, k);
                let desc = info.and_then(|i| i.props.get(k)).map(|p| p.description.clone()).unwrap_or_default();
                if k >= self.prop_buf.len() {
                    self.prop_buf.push(p.value.clone());
                }
                // "[a, b, c]" in the description -> choice list (like Qucs-S' property dialog)
                let options: Vec<String> = desc
                    .rsplit_once('[')
                    .and_then(|(_, r)| r.split_once(']'))
                    .map(|(o, _)| o.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                    .unwrap_or_default();
                let fill = if k % 2 == 1 { ui.visuals().faint_bg_color } else { egui::Color32::TRANSPARENT };
                egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(2, 2)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(label_w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // pad every name to the longest one so all value fields start at the same x
                            ui.set_min_width(label_w);
                            ui.label(&pname).on_hover_text(&desc);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut vis = p.visible();
                            if ui.checkbox(&mut vis, "").on_hover_text("回路図に表示").changed() {
                                toggle = Some(k);
                            }
                            let w = ui.available_width().max(40.0);
                            if options.len() >= 2 && options.len() <= 30 {
                                let mut v = p.value.clone();
                                egui::ComboBox::from_id_salt(("prop", k)).width(w).selected_text(&v).show_ui(ui, |ui| {
                                    for o in &options {
                                        ui.selectable_value(&mut v, o.clone(), o);
                                    }
                                });
                                if v != p.value {
                                    self.prop_buf[k] = v.clone();
                                    commit = Some((k, v));
                                }
                            } else {
                                let r = ui.add_sized([w, row_h], egui::TextEdit::singleline(&mut self.prop_buf[k]));
                                if r.lost_focus() && self.prop_buf[k] != p.value {
                                    commit = Some((k, self.prop_buf[k].clone()));
                                }
                            }
                        });
                    });
                });
            }
        });
        if let Some((k, v)) = commit {
            self.doc.checkpoint();
            self.doc.sch.components[ci].props[k].value = v;
            self.changed();
            self.sel = vec![ci];
        }
        if let Some(k) = toggle {
            self.doc.checkpoint();
            let p = &mut self.doc.sch.components[ci].props[k];
            p.display = if p.visible() { "0".into() } else { "1".into() };
            self.changed();
            self.sel = vec![ci];
        }
    }

    fn sim_window(&mut self, ctx: &egui::Context) {
        use crate::diagram::DiagramKind as K;
        if !self.show_sim {
            return;
        }
        let mut open = true;
        egui::Window::new("シミュレーション結果").open(&mut open).default_size([940.0, 700.0]).show(ctx, |ui| {
            let Some(r) = &self.sim else {
                ui.label("結果なし");
                return;
            };
            if !r.errors.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, r.errors.join("\n"));
            }
            if r.plots.is_empty() {
                return;
            }
            // sweep steps are grouped under their base analysis
            let mut entries: Vec<(String, usize)> = Vec::new();
            for (k, p) in r.plots.iter().enumerate() {
                if p.sweep.is_some() {
                    if !entries.iter().any(|e| e.0.starts_with(&format!("{} ", p.base))) {
                        entries.push((format!("{} ({}, スイープ)", p.base, p.kind), k));
                    }
                } else {
                    entries.push((format!("{} ({})", p.name, p.kind), k));
                }
            }
            let mut changed = false;
            ui.horizontal(|ui| {
                let cur = entries.iter().find(|e| e.1 == self.plot_analysis).map(|e| e.0.clone()).unwrap_or_default();
                egui::ComboBox::from_label("解析").selected_text(cur).show_ui(ui, |ui| {
                    for (n, k) in &entries {
                        if ui.selectable_value(&mut self.plot_analysis, *k, n).changed() {
                            changed = true;
                            self.markers.clear();
                        }
                    }
                });
                ui.separator();
                for (k, label) in [(K::Rect, "直交"), (K::Polar, "極座標"), (K::Smith, "スミス"), (K::Table, "表")] {
                    if ui.selectable_label(self.diag_kind == k, label).clicked() {
                        self.diag_kind = k;
                        changed = true;
                    }
                }
                ui.separator();
                if ui.button("マーカー消去").clicked() {
                    self.markers.clear();
                    changed = true;
                }
            });
            let p = &r.plots[self.plot_analysis.min(r.plots.len() - 1)];
            ui.horizontal_wrapped(|ui| {
                for v in p.vectors.iter().skip(if p.kind == "op" { 0 } else { 1 }) {
                    let mut on = self.plot_vectors.contains(&v.name);
                    if ui.checkbox(&mut on, &v.name).changed() {
                        if on {
                            self.plot_vectors.push(v.name.clone());
                        } else {
                            self.plot_vectors.retain(|n| n != &v.name);
                        }
                        changed = true;
                    }
                }
            });
            if changed {
                self.plot_dirty = true;
            }
            if self.plot_dirty || self.plot_tex.is_none() {
                self.plot_dirty = false;
                let plots = r.find_all(Some(if p.sweep.is_some() { &p.base } else { &p.name })).unwrap_or_default();
                let kind = if p.kind == "op" { K::Table } else { self.diag_kind.clone() };
                let spec = crate::diagram::DiagramSpec {
                    kind,
                    vectors: self.plot_vectors.iter().filter(|n| p.vector(n).is_some()).cloned().collect(),
                    markers: self.markers.clone(),
                    ..Default::default()
                };
                match crate::diagram::diagram_svg(&plots, &spec, 900.0, if spec.kind == K::Rect { 520.0 } else { 640.0 }) {
                    Ok(out) => {
                        self.marker_out = out.markers.clone();
                        self.plot_area = out.plot_area;
                        if let Ok((w, h, rgba)) = crate::render::svg_to_rgba(&out.svg) {
                            let img = egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], &theme::pal(ctx).map_image(&rgba));
                            self.plot_px = (w as f32, h as f32);
                            self.plot_tex = Some(ctx.load_texture("plot", img, egui::TextureOptions::LINEAR));
                        }
                    }
                    Err(e) => {
                        self.status = e;
                        self.plot_tex = None;
                        self.plot_area = None;
                    }
                }
            }
            if let Some(t) = &self.plot_tex {
                let avail = ui.available_width();
                let sz = t.size_vec2();
                let k = (avail / sz.x).min(1.0);
                let resp = ui.add(egui::Image::new((t.id(), sz * k)).sense(Sense::click()));
                // click inside the plot area places a marker at that x
                if let (true, Some(pos), Some((l, _tp, w, _h, x0, x1, log))) = (resp.clicked(), resp.interact_pointer_pos(), self.plot_area) {
                    let px = ((pos.x - resp.rect.min.x) / k) as f64;
                    let f = ((px - l) / w).clamp(0.0, 1.0);
                    let x = if log { (x0.ln() + f * (x1.ln() - x0.ln())).exp() } else { x0 + f * (x1 - x0) };
                    self.markers.push(x);
                    self.plot_dirty = true;
                }
                if self.plot_area.is_some() {
                    ui.small("グラフをクリックするとマーカーを追加します");
                }
            }
            if let Some(arr) = self.marker_out.as_array() {
                if !arr.is_empty() {
                    egui::Grid::new("markers").striped(true).show(ui, |ui| {
                        for m in arr {
                            ui.label(crate::plot::eng(m.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0)));
                            ui.label(m.get("label").or(m.get("vector")).and_then(|v| v.as_str()).unwrap_or(""));
                            let v = m.get("value").cloned().unwrap_or_default();
                            let txt = match &v {
                                serde_json::Value::Number(n) => crate::plot::eng(n.as_f64().unwrap_or(0.0)),
                                serde_json::Value::Object(o) => format!(
                                    "{} dB, {:.1}°",
                                    o.get("db").and_then(|x| x.as_f64()).map(|x| format!("{x:.2}")).unwrap_or_default(),
                                    o.get("phase_deg").and_then(|x| x.as_f64()).unwrap_or(0.0)
                                ),
                                _ => "-".into(),
                            };
                            ui.label(txt);
                            ui.end_row();
                        }
                    });
                }
            }
        });
        if !open {
            self.show_sim = false;
        }
    }

    // ------------------------------------------------------------------ tools

    fn tools_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("⚙ ツール", |ui| {
            if ui.button("フィルタ設計…").clicked() {
                self.tools.filter_open = true;
                ui.close();
            }
            if ui.button("減衰器設計…").clicked() {
                self.tools.atten_open = true;
                ui.close();
            }
            if ui.button("伝送線路計算…").clicked() {
                self.tools.tline_open = true;
                ui.close();
            }
            ui.separator();
            if ui.button("パラメータスイープ…").clicked() {
                if let Some(&ci) = self.sel.first() {
                    let c = &self.doc.sch.components[ci];
                    self.tools.s_param = c.name.clone();
                    if let Some(v) = c.props.first().and_then(|p| crate::rftools::parse_value(&p.value)) {
                        self.tools.s_start = crate::plot::eng(v / 2.0);
                        self.tools.s_stop = crate::plot::eng(v * 2.0);
                    }
                }
                if self.tools.s_sim.is_empty() {
                    self.tools.s_sim = self.doc.sch.components.iter().find(|c| c.typ.starts_with('.') && c.typ != ".SW").map(|c| c.name.clone()).unwrap_or_default();
                }
                self.tools.sweep_open = true;
                ui.close();
            }
            if ui.button("モンテカルロ解析…").clicked() {
                self.mc.open = true;
                ui.close();
            }
            if ui.button("チューニング…").clicked() {
                self.tools.tune_open = true;
                ui.close();
            }
        });
    }

    fn place_design(&mut self, d: Result<crate::rftools::Design, String>, bench: Option<(f64, f64, f64, f64)>) {
        let d = match d {
            Ok(d) => d,
            Err(e) => {
                self.tools.result = e;
                return;
            }
        };
        let conn = self.doc.conn();
        let maxy = conn.placed.iter().map(|p| p.bbox[3]).fold(0.0f64, f64::max);
        let (x, y) = (300, (maxy as i32 + 120).max(120));
        self.doc.checkpoint();
        match crate::synth::place_design(&mut self.doc, &d, x, y, bench) {
            Ok(_) => {
                self.tools.result = format!("{}\n{}", d.title, d.notes.join("\n"));
                self.need_fit = true;
                self.changed();
            }
            Err(e) => self.tools.result = e,
        }
    }

    fn tool_windows(&mut self, ctx: &egui::Context) {
        // ---------- filter
        let mut open = self.tools.filter_open;
        egui::Window::new("フィルタ設計").open(&mut open).resizable(false).show(ctx, |ui| {
            let t = &mut self.tools;
            egui::Grid::new("filt").num_columns(2).show(ui, |ui| {
                ui.label("特性");
                egui::ComboBox::from_id_salt("fr").selected_text(["Butterworth", "Chebyshev", "Bessel"][t.f_response]).show_ui(ui, |ui| {
                    for (k, n) in ["Butterworth", "Chebyshev", "Bessel"].iter().enumerate() {
                        ui.selectable_value(&mut t.f_response, k, *n);
                    }
                });
                ui.end_row();
                ui.label("種類");
                egui::ComboBox::from_id_salt("fc").selected_text(["ローパス", "ハイパス", "バンドパス", "バンドストップ"][t.f_class]).show_ui(ui, |ui| {
                    for (k, n) in ["ローパス", "ハイパス", "バンドパス", "バンドストップ"].iter().enumerate() {
                        ui.selectable_value(&mut t.f_class, k, *n);
                    }
                });
                ui.end_row();
                ui.label("次数");
                ui.add(egui::DragValue::new(&mut t.f_order).range(1..=10));
                ui.end_row();
                if t.f_response == 1 {
                    ui.label("リップル [dB]");
                    ui.add(egui::DragValue::new(&mut t.f_ripple).range(0.01..=3.0).speed(0.01));
                    ui.end_row();
                }
                ui.label(if t.f_class >= 2 { "下側周波数" } else { "カットオフ周波数" });
                ui.text_edit_singleline(&mut t.f_f1);
                ui.end_row();
                if t.f_class >= 2 {
                    ui.label("上側周波数");
                    ui.text_edit_singleline(&mut t.f_f2);
                    ui.end_row();
                }
                ui.label("インピーダンス [Ω]");
                ui.add(egui::DragValue::new(&mut t.f_z0).range(1.0..=1e6));
                ui.end_row();
                ui.label("構成");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut t.f_series, false, "π (並列から)");
                    ui.radio_value(&mut t.f_series, true, "T (直列から)");
                });
                ui.end_row();
            });
            ui.checkbox(&mut t.f_bench, "テスト回路（信号源・負荷・AC解析）も追加");
            if ui.button("設計して配置").clicked() {
                let args = serde_json::json!({
                    "response": (["butterworth", "chebyshev", "bessel"][t.f_response]), "ripple_db": t.f_ripple,
                    "class": (["lowpass", "highpass", "bandpass", "bandstop"][t.f_class]), "order": t.f_order,
                    "f1": t.f_f1, "f2": t.f_f2, "z0": t.f_z0, "first_series": t.f_series,
                });
                let bench_on = t.f_bench;
                let spec = crate::synth::filter_spec(&args);
                match spec {
                    Ok(spec) => {
                        let d = crate::rftools::lc_filter(&spec);
                        let zl = d.as_ref().ok().and_then(|d| d.values.get("z_load").and_then(|v| v.as_f64())).unwrap_or(spec.z0);
                        let (lo, hi) = crate::synth::bench_range(&spec);
                        self.place_design(d, bench_on.then_some((spec.z0, zl, lo, hi)));
                    }
                    Err(e) => self.tools.result = e,
                }
            }
            if !self.tools.result.is_empty() {
                ui.label(&self.tools.result);
            }
        });
        self.tools.filter_open = open;

        // ---------- attenuator
        let mut open = self.tools.atten_open;
        egui::Window::new("減衰器設計").open(&mut open).resizable(false).show(ctx, |ui| {
            let t = &mut self.tools;
            egui::Grid::new("att").num_columns(2).show(ui, |ui| {
                ui.label("構成");
                egui::ComboBox::from_id_salt("at").selected_text(["π型", "T型", "ブリッジT型"][t.a_topo]).show_ui(ui, |ui| {
                    for (k, n) in ["π型", "T型", "ブリッジT型"].iter().enumerate() {
                        ui.selectable_value(&mut t.a_topo, k, *n);
                    }
                });
                ui.end_row();
                ui.label("減衰量 [dB]");
                ui.add(egui::DragValue::new(&mut t.a_db).range(0.1..=100.0).speed(0.1));
                ui.end_row();
                ui.label("入力 Z [Ω]");
                ui.add(egui::DragValue::new(&mut t.a_zin).range(1.0..=1e6));
                ui.end_row();
                ui.label("出力 Z [Ω]");
                ui.add(egui::DragValue::new(&mut t.a_zout).range(1.0..=1e6));
                ui.end_row();
            });
            if ui.button("設計して配置").clicked() {
                let topo = [crate::rftools::AttenuatorTopology::Pi, crate::rftools::AttenuatorTopology::Tee, crate::rftools::AttenuatorTopology::BridgedTee][t.a_topo];
                let (db, zi, zo) = (t.a_db, t.a_zin, t.a_zout);
                let d = crate::rftools::attenuator(topo, db, zi, zo);
                self.place_design(d, None);
            }
            if !self.tools.result.is_empty() {
                ui.label(&self.tools.result);
            }
        });
        self.tools.atten_open = open;

        // ---------- transmission line
        let mut open = self.tools.tline_open;
        egui::Window::new("伝送線路計算").open(&mut open).resizable(false).show(ctx, |ui| {
            let t = &mut self.tools;
            ui.horizontal(|ui| {
                ui.radio_value(&mut t.t_coax, false, "マイクロストリップ");
                ui.radio_value(&mut t.t_coax, true, "同軸");
            });
            egui::Grid::new("tl").num_columns(2).show(ui, |ui| {
                ui.label("εr");
                ui.add(egui::DragValue::new(&mut t.t_er).range(1.0..=100.0).speed(0.01));
                ui.end_row();
                if t.t_coax {
                    ui.label("内導体径 d [mm]");
                    ui.add(egui::DragValue::new(&mut t.t_d).speed(0.01));
                    ui.end_row();
                    ui.label("外導体内径 D [mm]");
                    ui.add(egui::DragValue::new(&mut t.t_dd).speed(0.01));
                    ui.end_row();
                } else {
                    ui.label("基板厚 h [mm]");
                    ui.add(egui::DragValue::new(&mut t.t_h).speed(0.01));
                    ui.end_row();
                    ui.label("銅箔厚 t [µm]");
                    ui.add(egui::DragValue::new(&mut t.t_t).speed(1.0));
                    ui.end_row();
                    ui.label("線幅 w [mm]");
                    ui.add(egui::DragValue::new(&mut t.t_w).speed(0.01));
                    ui.end_row();
                    ui.label("目標 Z0 [Ω]");
                    ui.add(egui::DragValue::new(&mut t.t_z0).speed(0.1));
                    ui.end_row();
                }
                ui.label("周波数");
                ui.text_edit_singleline(&mut t.t_freq);
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if !t.t_coax {
                    if ui.button("解析 (w → Z0)").clicked() {
                        let a = serde_json::json!({"kind": "microstrip", "er": t.t_er, "h_mm": t.t_h, "t_um": t.t_t, "w_mm": t.t_w, "freq": t.t_freq});
                        t.t_out = crate::synth::tline(&a).map(|v| serde_json::to_string_pretty(&v).unwrap()).unwrap_or_else(|e| e);
                    }
                    if ui.button("合成 (Z0 → w)").clicked() {
                        let a = serde_json::json!({"kind": "microstrip", "er": t.t_er, "h_mm": t.t_h, "t_um": t.t_t, "z0": t.t_z0, "freq": t.t_freq});
                        let r = crate::synth::tline(&a);
                        if let Ok(v) = &r {
                            if let Some(w) = v.get("w_mm").and_then(|x| x.as_f64()) {
                                t.t_w = w;
                            }
                        }
                        t.t_out = r.map(|v| serde_json::to_string_pretty(&v).unwrap()).unwrap_or_else(|e| e);
                    }
                } else if ui.button("計算").clicked() {
                    let a = serde_json::json!({"kind": "coax", "er": t.t_er, "d_inner_mm": t.t_d, "d_outer_mm": t.t_dd, "freq": t.t_freq});
                    t.t_out = crate::synth::tline(&a).map(|v| serde_json::to_string_pretty(&v).unwrap()).unwrap_or_else(|e| e);
                }
            });
            if !t.t_out.is_empty() {
                ui.monospace(&t.t_out);
            }
        });
        self.tools.tline_open = open;

        // ---------- parameter sweep
        let mut open = self.tools.sweep_open;
        let mut add = false;
        egui::Window::new("パラメータスイープ").open(&mut open).resizable(false).show(ctx, |ui| {
            let t = &mut self.tools;
            egui::Grid::new("sw").num_columns(2).show(ui, |ui| {
                ui.label("部品名 / 変数");
                ui.text_edit_singleline(&mut t.s_param);
                ui.end_row();
                ui.label("対象の解析");
                ui.text_edit_singleline(&mut t.s_sim);
                ui.end_row();
                ui.label("開始");
                ui.text_edit_singleline(&mut t.s_start);
                ui.end_row();
                ui.label("終了");
                ui.text_edit_singleline(&mut t.s_stop);
                ui.end_row();
                ui.label("点数");
                ui.text_edit_singleline(&mut t.s_points);
                ui.end_row();
                ui.label("刻み");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut t.s_type, 0, "lin");
                    ui.radio_value(&mut t.s_type, 1, "log");
                });
                ui.end_row();
            });
            if ui.button("スイープを追加").clicked() {
                add = true;
            }
        });
        if add {
            let t = &self.tools;
            let (sim, param, a, b, n, ty) = (t.s_sim.clone(), t.s_param.clone(), t.s_start.clone(), t.s_stop.clone(), t.s_points.clone(), ["lin", "log"][t.s_type]);
            self.doc.checkpoint();
            match self.doc.add_parameter_sweep(&sim, &param, &a, &b, &n, ty, None) {
                Ok(n) => {
                    self.status = format!("{n} を追加しました");
                    self.changed();
                }
                Err(e) => self.status = e,
            }
        }
        self.tools.sweep_open = open;

        // ---------- tuning
        let mut open = self.tools.tune_open;
        let mut apply: Option<usize> = None;
        let mut remove: Option<usize> = None;
        egui::Window::new("チューニング").open(&mut open).default_width(380.0).show(ctx, |ui| {
            ui.small("部品またはパラメータ（SpicePar / Eqn）を選んで「追加」→ スライダーを動かすと値を書き換えて再シミュレーションします");
            // the selected component's values first, then every parameter of the schematic
            let mut cands: Vec<Tune> = Vec::new();
            if self.sel.len() == 1 {
                if let Some(c) = self.doc.sch.components.get(self.sel[0]) {
                    cands.extend(tune_targets(c));
                }
            }
            for c in self.doc.sch.components.iter().filter(|c| c.typ == "SpicePar" || c.typ == "Eqn") {
                for t in tune_targets(c) {
                    if !cands.iter().any(|o| o.same_target(&t)) {
                        cands.push(t);
                    }
                }
            }
            cands.retain(|t| !self.tools.tunes.iter().any(|o| o.same_target(t)));
            if self.tools.tune_pick >= cands.len() {
                self.tools.tune_pick = 0;
            }
            ui.horizontal(|ui| {
                let shown = cands.get(self.tools.tune_pick).map(|t| format!("{} = {}", t.label, t.value_text())).unwrap_or_else(|| "（候補なし）".into());
                egui::ComboBox::from_id_salt("tune_pick").width(260.0).selected_text(shown).show_ui(ui, |ui| {
                    for (i, t) in cands.iter().enumerate() {
                        ui.selectable_value(&mut self.tools.tune_pick, i, format!("{} = {}", t.label, t.value_text()));
                    }
                });
                if ui.add_enabled(!cands.is_empty(), egui::Button::new("追加")).clicked() {
                    self.tools.tunes.push(cands.swap_remove(self.tools.tune_pick));
                    self.tools.tune_pick = 0;
                }
            });
            if cands.is_empty() && self.tools.tunes.is_empty() {
                ui.small("数値の値を持つ部品を1つ選ぶか、SpicePar（例: Uin=1.5）を配置してください");
            }
            ui.checkbox(&mut self.tools.tune_auto, "値を変えたら自動でシミュレーション");
            for (k, t) in self.tools.tunes.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{} = {}", t.label, t.value_text()));
                    if ui.small_button("✖").clicked() {
                        remove = Some(k);
                    }
                });
                let r = ui.add(egui::Slider::new(&mut t.value, t.min..=t.max).logarithmic(t.min > 0.0).show_value(false));
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    apply = Some(k);
                }
            }
        });
        if let Some(k) = remove {
            self.tools.tunes.remove(k);
        }
        if let Some(k) = apply {
            let t = &self.tools.tunes[k];
            let (comp, v) = (t.comp.clone(), t.text());
            // a parameter is found again by its name, in case lines were added or removed
            let target = self.doc.sch.component_index(&comp).and_then(|ci| {
                let props = &self.doc.sch.components[ci].props;
                let pi = match &t.key {
                    Some(key) => props.iter().position(|p| p.value.split_once('=').is_some_and(|(n, _)| n.trim() == key)),
                    None => (t.prop < props.len()).then_some(t.prop),
                };
                pi.map(|pi| (ci, pi))
            });
            if let Some((ci, prop)) = target {
                self.doc.checkpoint();
                self.doc.sch.components[ci].props[prop].value = v.clone();
                self.changed();
                self.status = format!("{comp} = {v}");
                if self.tools.tune_auto && self.sim_rx.is_none() {
                    self.run_sim();
                }
            } else {
                self.status = format!("{comp} が見つかりません（削除または名前変更された可能性があります）");
            }
        }
        self.tools.tune_open = open;
    }

    // ------------------------------------------------------------------ canvas

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let canvas = resp.rect;
        self.canvas_rect = canvas;
        painter.rect_filled(canvas, 0.0, self.pal().canvas);
        self.rebuild_scene();
        if self.need_fit {
            self.fit(canvas);
            self.need_fit = false;
        }
        let ctx = ui.ctx().clone();
        let hover = resp.hover_pos();
        if let Some(h) = hover {
            self.cursor = self.snapped(canvas, h);
            // zoom
            let zd = ctx.input(|i| i.zoom_delta());
            let scroll = ctx.input(|i| i.smooth_scroll_delta);
            let factor = if (zd - 1.0).abs() > 1e-4 { zd } else if scroll.y.abs() > 0.0 { (scroll.y * 0.0025).exp() } else { 1.0 };
            if (factor - 1.0).abs() > 1e-4 {
                let before = self.to_sch_f(canvas, h);
                self.zoom = (self.zoom * factor).clamp(0.1, 12.0);
                let after = self.to_sch_f(canvas, h);
                self.offset += Vec2::new((before.0 - after.0) as f32, (before.1 - after.1) as f32);
            }
        }
        // pan with middle / right drag
        if resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged_by(egui::PointerButton::Secondary) && self.tool == Tool::Select) {
            self.offset -= resp.drag_delta() / self.zoom;
        }

        if self.show_grid {
            self.paint_grid(&painter, canvas);
        }
        let shapes = std::mem::take(&mut self.scene.shapes);
        self.paint_shapes(&painter, canvas, &shapes, 1.0);
        self.scene.shapes = shapes;
        self.paint_op(&painter, canvas);

        let pointer = hover.map(|h| self.to_sch_f(canvas, h));
        match self.tool.clone() {
            Tool::Select => self.select_tool(&resp, canvas, &painter, pointer),
            Tool::Place(t) => {
                if let Some((px, py)) = pointer {
                    let (x, y) = (snap(px.round() as i32), snap(py.round() as i32));
                    if let Ok(mut c) = spec_component(&t, x, y) {
                        c.rotate = self.place_rot;
                        let pl = library::place(&c, None);
                        let mut ghost = Vec::new();
                        for p in &pl.prims {
                            crate::render::prim_shapes(p, &mut ghost, Some(theme::to_color(self.pal().accent)));
                        }
                        self.paint_shapes(&painter, canvas, &ghost, 0.5);
                    }
                    if resp.clicked() {
                        self.doc.checkpoint();
                        let (typ, props) = match t.strip_prefix("Lib:").and_then(|r| r.split_once('/')) {
                            Some((l, c)) => {
                                let mut m = serde_json::Map::new();
                                m.insert("Lib".into(), serde_json::json!(l));
                                m.insert("Comp".into(), serde_json::json!(c));
                                ("Lib".to_string(), m)
                            }
                            None => (t.clone(), serde_json::Map::new()),
                        };
                        match self.doc.add_component(&typ, None, x, y, Some(self.place_rot), None, &props) {
                            Ok(n) => {
                                self.status = format!("{n} を配置");
                                self.changed();
                                self.sel = vec![self.doc.sch.components.len() - 1];
                            }
                            Err(e) => self.status = e,
                        }
                    }
                }
                if resp.secondary_clicked() {
                    self.tool = Tool::Select;
                }
            }
            Tool::Wire => {
                if let Some((px, py)) = pointer {
                    let cur = (snap(px.round() as i32), snap(py.round() as i32));
                    if let Some(&last) = self.wire_pts.last() {
                        let mid = (cur.0, last.1);
                        let s = Stroke::new(2.0_f32, self.pal().accent);
                        painter.line_segment([self.to_screen(canvas, last.0 as f64, last.1 as f64), self.to_screen(canvas, mid.0 as f64, mid.1 as f64)], s);
                        painter.line_segment([self.to_screen(canvas, mid.0 as f64, mid.1 as f64), self.to_screen(canvas, cur.0 as f64, cur.1 as f64)], s);
                    }
                    painter.circle_stroke(self.to_screen(canvas, cur.0 as f64, cur.1 as f64), 4.0, Stroke::new(1.0_f32, self.pal().accent));
                    if resp.double_clicked() || resp.secondary_clicked() {
                        self.finish_wire();
                    } else if resp.clicked() {
                        if let Some(&last) = self.wire_pts.last() {
                            if last.0 != cur.0 && last.1 != cur.1 {
                                self.wire_pts.push((cur.0, last.1));
                            }
                        }
                        if self.wire_pts.last() != Some(&cur) {
                            self.wire_pts.push(cur);
                        }
                    }
                }
            }
            Tool::AutoWire => {
                if let Some((px, py)) = pointer {
                    if let Some(pin) = self.pin_at(px, py) {
                        if let Some(p) = self.conn.find_pin(&pin) {
                            painter.circle_stroke(self.to_screen(canvas, p.x as f64, p.y as f64), 7.0, Stroke::new(2.0_f32, self.pal().accent_hi));
                            painter.text(self.to_screen(canvas, p.x as f64, p.y as f64) + Vec2::new(8.0, -18.0), Align2::LEFT_TOP, &pin, FontId::proportional(13.0), self.pal().accent_hi);
                        }
                        if resp.clicked() {
                            match self.autowire_first.take() {
                                None => self.autowire_first = Some(pin),
                                Some(a) => {
                                    self.doc.checkpoint();
                                    match self.doc.connect(&[a.clone(), pin.clone()], None) {
                                        Ok(r) => self.status = format!("{a} — {pin}: {}", r.get("routes").map(|x| x.to_string()).unwrap_or_default()),
                                        Err(e) => self.status = e,
                                    }
                                    self.changed();
                                }
                            }
                        }
                    }
                    if let Some(a) = &self.autowire_first {
                        if let Some(p) = self.conn.find_pin(a) {
                            painter.line_segment([self.to_screen(canvas, p.x as f64, p.y as f64), hover.unwrap()], Stroke::new(1.0_f32, self.pal().accent_hi));
                        }
                    }
                }
                if resp.secondary_clicked() {
                    self.autowire_first = None;
                }
            }
            Tool::Paint(kind) => {
                if let Some((px, py)) = pointer {
                    let cur = (snap(px.round() as i32), snap(py.round() as i32));
                    if kind == "text" {
                        if resp.clicked() {
                            self.text_prompt = Some((cur, String::new(), None));
                        }
                    } else {
                        if resp.drag_started_by(egui::PointerButton::Primary) {
                            let press = resp.ctx.input(|i| i.pointer.press_origin()).unwrap_or(hover.unwrap());
                            self.paint_start = Some(self.snapped(canvas, press));
                        }
                        if let Some(st) = self.paint_start {
                            let a = self.to_screen(canvas, st.0 as f64, st.1 as f64);
                            let b = self.to_screen(canvas, cur.0 as f64, cur.1 as f64);
                            let s = Stroke::new(1.5_f32, self.pal().accent);
                            match kind.as_str() {
                                "rect" => {
                                    painter.rect_stroke(Rect::from_two_pos(a, b), 0.0, s, egui::StrokeKind::Inside);
                                }
                                "ellipse" => {
                                    let r = Rect::from_two_pos(a, b);
                                    painter.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, s));
                                }
                                _ => {
                                    painter.line_segment([a, b], s);
                                }
                            }
                            if resp.drag_stopped() {
                                self.paint_start = None;
                                let (x, y, w, h) = match kind.as_str() {
                                    "rect" | "ellipse" => (st.0.min(cur.0), st.1.min(cur.1), (cur.0 - st.0).abs(), (cur.1 - st.1).abs()),
                                    _ => (st.0, st.1, cur.0 - st.0, cur.1 - st.1),
                                };
                                if w != 0 || h != 0 {
                                    if let Ok(line) = crate::edit::make_painting(&kind, x, y, w, h, "#000080", 2, "", 12, false) {
                                        self.doc.checkpoint();
                                        self.doc.sch.paintings.push(format!("  {line}"));
                                        self.changed();
                                    }
                                }
                            }
                        }
                    }
                }
                if resp.secondary_clicked() {
                    self.tool = Tool::Select;
                }
            }
            Tool::Label => {
                if let Some((px, py)) = pointer {
                    if resp.clicked() {
                        let target = if let Some(pin) = self.pin_at(px, py) {
                            Some(pin)
                        } else if let Some(wi) = self.wire_at(px, py) {
                            let w = &self.doc.sch.wires[wi];
                            Some(format!("[{},{}]", w.x1, w.y1))
                        } else {
                            None
                        };
                        if let Some(t) = target {
                            self.label_prompt = Some((t, String::new()));
                        }
                    }
                }
            }
        }

        // status text overlay (coordinates)
        painter.text(canvas.right_bottom() - Vec2::new(8.0, 6.0), Align2::RIGHT_BOTTOM, format!("x={} y={}  zoom {:.0}%", self.cursor.0, self.cursor.1, self.zoom * 100.0 / 1.5), FontId::proportional(12.0), self.pal().muted);
        self.write_state(canvas);
    }

    fn finish_wire(&mut self) {
        if self.wire_pts.len() >= 2 {
            self.doc.checkpoint();
            let pts = std::mem::take(&mut self.wire_pts);
            let n = self.doc.add_wire_path(&pts);
            self.status = format!("配線 {n} 本を追加");
            self.changed();
        }
        self.wire_pts.clear();
    }

    fn select_tool(&mut self, resp: &egui::Response, canvas: Rect, painter: &egui::Painter, pointer: Option<(f64, f64)>) {
        let primary_down = resp.ctx.input(|i| i.pointer.primary_down());
        // a wire under the pointer (and no part) can be dragged: show the direction it moves
        let wire_dir = match &self.grab {
            Some(Grab::Wire { wire, orig, .. }) => orig.get(*wire).cloned(),
            Some(Grab::Group { .. }) => Some(crate::schematic::Wire::new(0, 0, 0, 0)),
            Some(_) => None,
            None if self.drag.is_none() && self.box_start.is_none() => pointer
                .filter(|&(px, py)| self.comp_at(px, py).is_none() && crate::edit::wire_label_at(&self.doc, px, py).is_none())
                // (matches the drag-start order: wire label, wire, component text, part)
                .and_then(|(px, py)| self.wire_at(px, py))
                .map(|wi| {
                    if self.sel_wires.contains(&wi) && self.sel.len() + self.sel_wires.len() + self.sel_paint.len() > 1 {
                        // part of a selection: the whole block moves freely
                        crate::schematic::Wire::new(0, 0, 0, 0)
                    } else {
                        self.doc.sch.wires[wi].clone()
                    }
                }),
            None => None,
        };
        if let Some(w) = wire_dir {
            resp.ctx.set_cursor_icon(if w.is_point() || (!w.is_horizontal() && !w.is_vertical()) {
                egui::CursorIcon::Move
            } else if w.is_horizontal() {
                egui::CursorIcon::ResizeVertical
            } else {
                egui::CursorIcon::ResizeHorizontal
            });
        }
        if resp.double_clicked() {
            if let Some((px, py)) = pointer {
                if let Some(ci) = self.comp_at(px, py).filter(|&ci| self.doc.sch.components[ci].typ == "Sub") {
                    self.sel = vec![ci];
                    self.open_selected_sub();
                    return;
                }
                if let Some(pi) = crate::edit::painting_at(&self.doc, px, py) {
                    let line = &self.doc.sch.paintings[pi];
                    if line.trim_start().starts_with("<Text") {
                        let t = crate::schematic::painting_tokens(line);
                        let cur = t.get(6).map(|s| crate::schematic::unquote(s).replace("\\n", "\n")).unwrap_or_default();
                        self.text_prompt = Some(((0, 0), cur, Some(pi)));
                    }
                }
            }
        }
        // selected paintings: dashed outline
        for &k in &self.sel_paint {
            if let Some(b) = self.doc.sch.paintings.get(k).and_then(|p| crate::edit::painting_bbox(p)) {
                let r = Rect::from_two_pos(self.to_screen(canvas, b[0] - 3.0, b[1] - 3.0), self.to_screen(canvas, b[2] + 3.0, b[3] + 3.0));
                painter.rect_stroke(r, 2.0, Stroke::new(1.5_f32, self.pal().accent_hi), egui::StrokeKind::Outside);
            }
        }
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(press) = resp.ctx.input(|i| i.pointer.press_origin()) {
                let (px, py) = self.to_sch_f(canvas, press);
                if let Some(wi) = crate::edit::wire_label_at(&self.doc, px, py) {
                    let w = &self.doc.sch.wires[wi];
                    self.grab = Some(Grab::WireLabel { wire: wi, orig: (w.lx, w.ly), start: (px, py) });
                } else if let Some(wi) = self.wire_at(px, py).filter(|_| self.comp_at(px, py).is_none()) {
                    // a wire wins over a component text block lying across it
                    if self.sel_wires.contains(&wi) && self.sel.len() + self.sel_wires.len() + self.sel_paint.len() > 1 {
                        self.start_group(px, py);
                        return;
                    }
                    self.sel.clear();
                    self.sel_paint.clear();
                    self.sel_wires = vec![wi];
                    let pins = self.conn.pins.iter().map(|p| (p.x, p.y)).collect();
                    self.grab = Some(Grab::Wire { wire: wi, orig: self.doc.sch.wires.clone(), pins, start: (px.round() as i32, py.round() as i32), moved: (0, 0) });
                } else if let Some(ci) = crate::edit::comp_label_at(&self.doc, px, py).filter(|ci| self.sel.contains(ci) || self.comp_at(px, py).is_none()) {
                    let c = &self.doc.sch.components[ci];
                    self.grab = Some(Grab::CompLabel { comp: ci, orig: (c.tx, c.ty), start: (px, py) });
                } else if let Some(ci) = self.comp_at(px, py) {
                    if !self.sel.contains(&ci) {
                        self.sel = vec![ci];
                        self.sel_wires.clear();
                        self.sel_paint.clear();
                    } else if !self.sel_wires.is_empty() || !self.sel_paint.is_empty() {
                        self.start_group(px, py);
                        return;
                    }
                    let orig = self.sel.iter().map(|&i| (i, self.doc.sch.components[i].x, self.doc.sch.components[i].y)).collect();
                    self.drag = Some(Drag { start: (px.round() as i32, py.round() as i32), orig, moved: false });
                } else if let Some(pi) = crate::edit::painting_at(&self.doc, px, py) {
                    if !self.sel_paint.contains(&pi) {
                        self.sel_paint = vec![pi];
                    } else if !self.sel.is_empty() || !self.sel_wires.is_empty() {
                        self.start_group(px, py);
                        return;
                    }
                    let orig = self.sel_paint.iter().map(|&k| (k, self.doc.sch.paintings[k].clone())).collect();
                    self.grab = Some(Grab::Paintings { orig, start: (px.round() as i32, py.round() as i32), moved: (0, 0) });
                } else {
                    self.box_start = Some(press);
                }
            }
        }
        if let (Some(d), Some((px, py))) = (&mut self.drag, pointer) {
            if primary_down {
                let dx = snap(px.round() as i32 - d.start.0);
                let dy = snap(py.round() as i32 - d.start.1);
                for &(i, ox, oy) in &d.orig {
                    let c = &mut self.doc.sch.components[i];
                    if (c.x, c.y) != (ox + dx, oy + dy) {
                        c.x = ox + dx;
                        c.y = oy + dy;
                        d.moved = true;
                    }
                }
                if d.moved {
                    self.conn = self.doc.conn();
                    self.gen += 1;
                }
            }
        }
        if let (Some(g), Some((px, py))) = (&mut self.grab, pointer) {
            if primary_down {
                match g {
                    Grab::WireLabel { wire, orig, start } => {
                        let w = &mut self.doc.sch.wires[*wire];
                        w.lx = orig.0 + (px - start.0).round() as i32;
                        w.ly = orig.1 + (py - start.1).round() as i32;
                    }
                    Grab::CompLabel { comp, orig, start } => {
                        let c = &mut self.doc.sch.components[*comp];
                        c.tx = orig.0 + (px - start.0).round() as i32;
                        c.ty = orig.1 + (py - start.1).round() as i32;
                    }
                    Grab::Paintings { orig, start, moved } => {
                        let d = (snap(px.round() as i32 - start.0), snap(py.round() as i32 - start.1));
                        if d != *moved {
                            *moved = d;
                            for (k, line) in orig.iter() {
                                self.doc.sch.paintings[*k] = crate::edit::translate_painting(line, d.0, d.1);
                            }
                        }
                    }
                    Grab::Wire { wire, orig, pins, start, moved } => {
                        let d = (snap(px.round() as i32 - start.0), snap(py.round() as i32 - start.1));
                        if d != *moved {
                            *moved = d;
                            self.doc.sch.wires = crate::edit::drag_wire(orig, pins, *wire, d.0, d.1);
                        }
                    }
                    Grab::Group { orig, pins, comps, wires, paints, start, moved } => {
                        let d = (snap(px.round() as i32 - start.0), snap(py.round() as i32 - start.1));
                        if d != *moved {
                            *moved = d;
                            let mut s = (**orig).clone();
                            crate::edit::move_group(&mut s, pins, comps, wires, paints, d.0, d.1);
                            self.doc.sch = s;
                        }
                    }
                }
                self.conn = self.doc.conn();
                self.gen += 1;
            }
        }
        if let Some(b) = self.box_start {
            if let Some(h) = resp.hover_pos().or(resp.interact_pointer_pos()) {
                let r = Rect::from_two_pos(b, h);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, self.pal().accent), egui::StrokeKind::Inside);
                painter.rect_filled(r, 0.0, self.pal().accent.gamma_multiply(0.12));
            }
        }
        if resp.drag_stopped() {
            if let Some(g) = self.grab.take() {
                // put the original back, checkpoint, re-apply: one undo step
                let snapshot = self.doc.sch.clone();
                match &g {
                    Grab::WireLabel { wire, orig, .. } => {
                        self.doc.sch.wires[*wire].lx = orig.0;
                        self.doc.sch.wires[*wire].ly = orig.1;
                    }
                    Grab::CompLabel { comp, orig, .. } => {
                        self.doc.sch.components[*comp].tx = orig.0;
                        self.doc.sch.components[*comp].ty = orig.1;
                    }
                    Grab::Paintings { orig, .. } => {
                        for (k, line) in orig {
                            self.doc.sch.paintings[*k] = line.clone();
                        }
                    }
                    Grab::Wire { orig, .. } => self.doc.sch.wires = orig.clone(),
                    Grab::Group { orig, .. } => self.doc.sch = (**orig).clone(),
                }
                self.doc.checkpoint();
                self.doc.sch = snapshot;
                if let Grab::Wire { wire, pins, .. } = &g {
                    // merge the stretched / split wires and keep the dragged one selected
                    let moved = self.doc.sch.wires[*wire].clone();
                    self.doc.sch.wires = crate::doc::normalize_wires(std::mem::take(&mut self.doc.sch.wires), pins);
                    let on = |w: &crate::schematic::Wire, p: (i32, i32)| {
                        (w.x1.min(w.x2)..=w.x1.max(w.x2)).contains(&p.0) && (w.y1.min(w.y2)..=w.y1.max(w.y2)).contains(&p.1)
                    };
                    self.sel_wires = self.doc.sch.wires.iter().position(|w| on(w, moved.p1()) && on(w, moved.p2())).into_iter().collect();
                }
                if let Grab::Group { wires, moved: d, .. } = &g {
                    // merge stretched wires; keep the moved wires selected
                    let moved: Vec<crate::schematic::Wire> = wires.iter().filter_map(|&i| self.doc.sch.wires.get(i).cloned()).collect();
                    let pins = self.doc.conn().pins.iter().map(|p| (p.x, p.y)).collect();
                    self.doc.sch.wires = crate::doc::normalize_wires(std::mem::take(&mut self.doc.sch.wires), &pins);
                    self.sel_wires = wire_cover(&self.doc.sch.wires, &moved);
                    self.status = format!("{} 個を移動 ({}, {})", self.sel.len() + moved.len() + self.sel_paint.len(), d.0, d.1);
                }
                self.changed();
            }
            if let Some(d) = self.drag.take() {
                if d.moved {
                    // restore, then apply as one undoable move (+ re-route)
                    let finals: Vec<(String, i32, i32)> = d.orig.iter().map(|&(i, _, _)| {
                        let c = &self.doc.sch.components[i];
                        (c.name.clone(), c.x, c.y)
                    }).collect();
                    for &(i, ox, oy) in &d.orig {
                        self.doc.sch.components[i].x = ox;
                        self.doc.sch.components[i].y = oy;
                    }
                    self.doc.checkpoint();
                    if let Err(e) = self.doc.move_components(&finals, self.auto_reroute) {
                        self.status = e;
                    }
                    self.changed();
                }
            }
            if let Some(b) = self.box_start.take() {
                if let Some(h) = resp.interact_pointer_pos().or(resp.hover_pos()) {
                    let (x0, y0) = self.to_sch_f(canvas, b);
                    let (x1, y1) = self.to_sch_f(canvas, h);
                    let r = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
                    self.sel = self.conn.placed.iter().enumerate().filter(|(_, p)| p.bbox[0] < r[2] && p.bbox[2] > r[0] && p.bbox[1] < r[3] && p.bbox[3] > r[1]).map(|(i, _)| i).collect();
                    self.sel_paint = self.doc.sch.paintings.iter().enumerate().filter(|(_, p)| {
                        crate::edit::painting_bbox(p).map(|b| b[0] >= r[0] && b[2] <= r[2] && b[1] >= r[1] && b[3] <= r[3]).unwrap_or(false)
                    }).map(|(i, _)| i).collect();
                    self.sel_wires = self.doc.sch.wires.iter().enumerate().filter(|(_, w)| {
                        let (a, bb) = ((w.x1.min(w.x2)) as f64, (w.x1.max(w.x2)) as f64);
                        let (c, dd) = ((w.y1.min(w.y2)) as f64, (w.y1.max(w.y2)) as f64);
                        a >= r[0] && bb <= r[2] && c >= r[1] && dd <= r[3]
                    }).map(|(i, _)| i).collect();
                }
            }
        }
        if resp.clicked() {
            if let Some((px, py)) = pointer {
                let shift = resp.ctx.input(|i| i.modifiers.shift);
                if let Some(ci) = self.comp_at(px, py) {
                    if shift {
                        if let Some(k) = self.sel.iter().position(|&x| x == ci) {
                            self.sel.remove(k);
                        } else {
                            self.sel.push(ci);
                        }
                    } else {
                        self.sel = vec![ci];
                        self.sel_wires.clear();
                    }
                    self.sel_paint.clear();
                } else if let Some(wi) = self.wire_at(px, py) {
                    if !shift {
                        self.sel.clear();
                        self.sel_wires.clear();
                        self.sel_paint.clear();
                    }
                    self.sel_wires.push(wi);
                } else if let Some(pi) = crate::edit::painting_at(&self.doc, px, py) {
                    if !shift {
                        self.sel.clear();
                        self.sel_wires.clear();
                        self.sel_paint.clear();
                    }
                    self.sel_paint.push(pi);
                } else {
                    self.sel.clear();
                    self.sel_wires.clear();
                    self.sel_paint.clear();
                }
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context, canvas: Rect) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (ctrl, keys) = ctx.input(|i| (i.modifiers.command, i.keys_down.clone()));
        let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        let _ = keys;
        if ctrl && pressed(egui::Key::Z) {
            if self.doc.undo() {
                self.changed();
            }
            return;
        }
        if ctrl && pressed(egui::Key::Y) {
            if self.doc.redo() {
                self.changed();
            }
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Copy => self.copy_selection(ctx, false),
                egui::Event::Cut => self.copy_selection(ctx, true),
                egui::Event::Paste(text) => self.paste(&text),
                _ => {}
            }
        }
        if ctrl && pressed(egui::Key::T) {
            self.new_tab(Doc::new(Schematic::new_empty("untitled"), None));
            return;
        }
        if ctrl && pressed(egui::Key::W) {
            self.close_tab(self.active);
            return;
        }
        if ctrl && pressed(egui::Key::Tab) {
            let n = self.tabs.len();
            let shift = ctx.input(|i| i.modifiers.shift);
            self.switch_tab(if shift { (self.active + n - 1) % n } else { (self.active + 1) % n });
            return;
        }
        if ctrl && pressed(egui::Key::O) {
            self.open_dialog();
            return;
        }
        if ctrl && pressed(egui::Key::D) {
            self.duplicate_selection();
            return;
        }
        if ctrl && pressed(egui::Key::A) {
            self.sel = (0..self.doc.sch.components.len()).collect();
            self.sel_wires = (0..self.doc.sch.wires.len()).collect();
            self.sel_paint = (0..self.doc.sch.paintings.len()).collect();
            return;
        }
        if ctrl && pressed(egui::Key::S) {
            if ctx.input(|i| i.modifiers.shift) {
                self.save_as();
            } else {
                self.save_current();
            }
            return;
        }
        if ctrl && pressed(egui::Key::F) {
            self.find_open = true;
            self.find_focus = true;
            return;
        }
        if ctrl && pressed(egui::Key::P) {
            self.print();
            return;
        }
        if ctrl && (pressed(egui::Key::Plus) || pressed(egui::Key::Equals)) {
            self.zoom_by(1.25);
            return;
        }
        if ctrl && pressed(egui::Key::Minus) {
            self.zoom_by(0.8);
            return;
        }
        if ctrl && pressed(egui::Key::Num0) {
            self.fit(canvas);
            return;
        }
        if pressed(egui::Key::F5) {
            self.run_sim();
            return;
        }
        if pressed(egui::Key::F1) {
            self.about_open = true;
            return;
        }
        if ctrl {
            return;
        }
        if pressed(egui::Key::Escape) {
            if self.tool == Tool::Wire {
                self.finish_wire();
            }
            self.tool = Tool::Select;
            self.autowire_first = None;
        }
        if pressed(egui::Key::R) {
            if let Tool::Place(_) = self.tool {
                self.place_rot = (self.place_rot + 1) % 4;
            } else {
                self.rotate_selected();
            }
        }
        if pressed(egui::Key::M) {
            self.mirror_selected();
        }
        if pressed(egui::Key::Y) {
            self.mirror_y_selected();
        }
        if pressed(egui::Key::T) {
            self.tool = Tool::Paint("text".into());
        }
        if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
            self.delete_selected();
        }
        if pressed(egui::Key::W) {
            self.tool = Tool::Wire;
        }
        if pressed(egui::Key::A) {
            self.tool = Tool::AutoWire;
        }
        if pressed(egui::Key::L) {
            self.tool = Tool::Label;
        }
        if pressed(egui::Key::F) {
            self.fit(canvas);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && (0..self.tabs.len()).any(|i| self.tab_unsaved(i)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(Confirm::Quit);
        }
        self.poll_op();
        self.poll_disk();
        self.poll_open_requests();
        self.heartbeat();
        if let Some((tid, rx)) = &self.sim_rx {
            if let Ok(r) = rx.try_recv() {
                let tid = *tid;
                self.sim_rx = None;
                if tid != self.tab_id {
                    // finished for a tab that is not shown: keep it there
                    if let Some(t) = self.tabs.iter_mut().flatten().find(|t| t.tab_id == tid) {
                        if let Ok(res) = r {
                            t.sim = Some(res);
                            t.show_sim = true;
                        }
                    }
                    return;
                }
                match r {
                    Ok(res) => {
                        self.status = format!("シミュレーション完了 ({:.2} s): {}", res.elapsed, res.plots.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", "));
                        self.plot_vectors = res.plots.first().map(|p| p.vectors.iter().skip(1).filter(|v| v.name.starts_with("v(")).take(4).map(|v| v.name.clone()).collect()).unwrap_or_default();
                        self.sim = Some(res);
                        self.plot_analysis = 0;
                        self.plot_dirty = true;
                        self.show_sim = true;
                    }
                    Err(e) => self.status = format!("シミュレーション失敗: {e}"),
                }
            }
        }
        let title = format!(
            "Qucs-S RS — {}{}",
            self.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "無題".into()),
            if self.unsaved(&self.doc) { " *" } else { "" }
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        let th = ctx.theme();
        if th != self.tex_theme {
            self.tex_theme = th;
            self.gen += 1;
            self.plot_dirty = true;
            self.mc_textures(ctx);
        }

        let screen = ctx.content_rect();
        let pal = self.pal();
        let bar = egui::Frame::new().fill(pal.bar).inner_margin(egui::Margin::symmetric(10, 6)).stroke(egui::Stroke::new(1.0_f32, pal.border));
        egui::TopBottomPanel::top("toolbar").frame(bar).show(ctx, |ui| self.toolbar(ui, screen));
        egui::TopBottomPanel::top("tabs").frame(egui::Frame::new().fill(pal.bar).inner_margin(egui::Margin::symmetric(10, 3))).show(ctx, |ui| self.tab_bar(ui));
        egui::TopBottomPanel::bottom("status").frame(bar.inner_margin(egui::Margin::symmetric(10, 3))).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.sim_rx.is_some() || self.mc.rx.is_some() {
                    ui.spinner();
                }
                ui.label(&self.status);
                if self.sim.is_some() && ui.small_button("結果").clicked() {
                    self.show_sim = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("ⓘ").on_hover_text("バージョン情報 (F1)").clicked() {
                        self.about_open = true;
                    }
                    self.theme_menu(ui);
                    let errs = self.issues.iter().filter(|i| i.severity == "error").count();
                    let warns = self.issues.len() - errs;
                    if ui.small_button(format!("⚠ エラー {errs} / 警告 {warns}")).clicked() {
                        self.show_issue_panel = !self.show_issue_panel;
                    }
                    ui.label(format!("部品 {}  配線 {}  ネット {}", self.doc.sch.components.len(), self.doc.sch.wires.len(), self.conn.nets.len()));
                });
            });
        });
        if self.external_change {
            egui::TopBottomPanel::top("ext").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(self.pal().warn, "ファイルが外部（AIエージェント）で変更されました。");
                    if ui.button("再読み込み").clicked() {
                        self.reload_from_disk();
                    }
                    if ui.button("無視").clicked() {
                        self.external_change = false;
                        self.disk_mtime = self.doc.path.as_ref().and_then(mtime);
                    }
                });
            });
        }
        if self.show_issue_panel {
            egui::TopBottomPanel::bottom("issues").resizable(true).default_height(140.0).show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut goto = None;
                    for (k, i) in self.issues.iter().enumerate() {
                        let c = if i.severity == "error" { self.pal().error } else { self.pal().warn };
                        if ui.add(egui::Label::new(egui::RichText::new(format!("{}. [{}] {}", k + 1, i.kind, i.message)).color(c)).sense(Sense::click())).clicked() {
                            goto = Some(i.at);
                        }
                    }
                    if let Some((x, y)) = goto {
                        let c = screen.center();
                        self.offset = Vec2::new(x as f32, y as f32) - (c - screen.min) / self.zoom;
                    }
                });
            });
        }
        let lw = egui::SidePanel::left("palette").default_width(self.settings.left_panel).show(ctx, |ui| self.palette(ui)).response.rect.width();
        let rw = egui::SidePanel::right("props").default_width(self.settings.right_panel).show(ctx, |ui| self.properties(ui)).response.rect.width();
        self.settings.left_panel = lw;
        self.settings.right_panel = rw;
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
            let canvas = ui.max_rect();
            self.keys(ctx, canvas);
            self.canvas(ui);
        });
        self.sim_window(ctx);
        self.tool_windows(ctx);
        self.mc_window(ctx);
        self.op_window(ctx);
        self.find_window(ctx);
        self.about_window(ctx);
        self.subckt_dialog(ctx);
        self.confirm_dialog(ctx);
        if let Some((pos, mut text, editing)) = self.text_prompt.take() {
            let mut done = false;
            let mut cancel = false;
            egui::Window::new("テキスト").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3));
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        done = true;
                    }
                    if ui.button("キャンセル").clicked() {
                        cancel = true;
                    }
                });
            });
            if done && !text.trim().is_empty() {
                self.doc.checkpoint();
                match editing {
                    Some(k) => self.doc.sch.paintings[k] = crate::edit::set_painting_text(&self.doc.sch.paintings[k], &text),
                    None => {
                        if let Ok(l) = crate::edit::make_painting("text", pos.0, pos.1, 0, 0, "#000000", 1, &text, 12, false) {
                            self.doc.sch.paintings.push(format!("  {l}"));
                        }
                    }
                }
                self.changed();
            } else if !done && !cancel {
                self.text_prompt = Some((pos, text, editing));
            }
        }
        if let Some((t, mut text)) = self.label_prompt.take() {
            let mut done = false;
            let mut cancel = false;
            egui::Window::new("ネット名").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label(format!("対象: {t}"));
                let r = ui.text_edit_singleline(&mut text);
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        done = true;
                    }
                    if ui.button("キャンセル").clicked() {
                        cancel = true;
                    }
                });
            });
            if done && !text.trim().is_empty() {
                self.doc.checkpoint();
                match self.doc.add_net_label(&t, text.trim()) {
                    Ok(_) => self.status = format!("ネット名 {}", text.trim()),
                    Err(e) => self.status = e,
                }
                self.changed();
            } else if !done && !cancel {
                self.label_prompt = Some((t, text));
            }
        }
        self.drive_shot(ctx);
        // keep polling the file even when idle
        ctx.request_repaint_after(std::time::Duration::from_millis(350));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.store_settings();
        // no longer accepting files: let the next launch open its own window
        let _ = std::fs::remove_file(crate::mcp::gui_heartbeat_path());
    }
}

/// Indices of the wires in `wires` that lie on one of the `moved` wires (after merging).
fn wire_cover(wires: &[crate::schematic::Wire], moved: &[crate::schematic::Wire]) -> Vec<usize> {
    let inside = |w: &crate::schematic::Wire, p: (i32, i32)| (w.x1.min(w.x2)..=w.x1.max(w.x2)).contains(&p.0) && (w.y1.min(w.y2)..=w.y1.max(w.y2)).contains(&p.1);
    wires.iter().enumerate().filter(|(_, w)| moved.iter().any(|m| inside(m, w.p1()) && inside(m, w.p2()) || inside(w, m.p1()) && inside(w, m.p2()))).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tune_tests {
    use super::*;

    #[test]
    fn spice_parameters_are_tunable() {
        let mut c = crate::library::new_component("SpicePar", "SpicePar1", 0, 0).unwrap();
        c.props = vec![
            crate::schematic::Prop { value: "Uin=1.5".into(), display: "1".into() },
            crate::schematic::Prop { value: "Rl=2k".into(), display: "1".into() },
        ];
        let t = tune_targets(&c);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].key.as_deref(), Some("Uin"));
        assert_eq!(t[1].label, "SpicePar1: Rl");
        assert_eq!(t[1].text(), "Rl=2k");
        let mut big = Tune { value: 4.7e6, ..tune_targets(&c).remove(1) };
        assert_eq!(big.text(), "Rl=4.7Meg");
        big.value = 3.3e-3;
        assert_eq!(big.text(), "Rl=3.3m");
    }

    #[test]
    fn every_numeric_property_is_offered() {
        let c = crate::library::new_component("R", "R1", 0, 0).unwrap();
        let t = tune_targets(&c);
        assert!(!t.is_empty());
        assert!(t.iter().all(|t| t.key.is_none()));
        assert!(t[0].label.starts_with("R1."), "{}", t[0].label);
        assert_eq!(value_unit("10 pF"), "F");
        assert_eq!(value_unit("1 MegOhm"), "Ohm");
    }
}
