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
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 780.0]).with_maximized(true).with_title("Qucs-S RS"),
        ..Default::default()
    };
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
    result: String,
}

struct Tune {
    comp: String,
    prop: usize,
    unit: String,
    value: f64,
    min: f64,
    max: f64,
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
        ("hiragino", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
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
            color_nets: false,
            pin_names: false,
            show_issues: true,
            show_grid: true,
            auto_save: true,
            auto_reroute: true,
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
        }
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
            let _ = std::fs::create_dir_all(p.parent().unwrap());
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
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
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

    fn run_sim(&mut self) {
        let Some(p) = self.doc.path.clone() else {
            self.status = "先に保存してください".into();
            return;
        };
        if let Err(e) = self.doc.save() {
            self.status = e;
            return;
        }
        self.disk_mtime = mtime(&p);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(sim::simulate(&p, 300));
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
            }
            Err(e) => self.status = e,
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

    fn close_tab(&mut self, i: usize) {
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
            (self.doc.path.clone(), self.doc.dirty && !self.auto_save)
        } else {
            match &self.tabs[i] {
                Some(t) => (t.doc.path.clone(), t.doc.dirty && !self.auto_save),
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
        let _ = std::fs::create_dir_all(p.parent().unwrap());
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
            let Some(p) = self.doc.path.clone() else {
                self.mc.error = "先に保存してください".into();
                return;
            };
            if let Err(e) = self.doc.save() {
                self.mc.error = e;
                return;
            }
            self.disk_mtime = mtime(&p);
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
                let _ = tx.send(r);
            });
            self.mc.rx = Some(rx);
        }
    }

    fn save_as(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Qucs schematic", &["sch"]).set_file_name("untitled.sch").save_file() {
            self.doc.path = Some(p.clone());
            match self.doc.save() {
                Ok(_) => {
                    self.disk_mtime = mtime(&p);
                    self.status = format!("保存: {}", p.display());
                }
                Err(e) => self.status = e,
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
            if ui.button("📂 開く").clicked() {
                self.open_dialog();
            }
            if ui.button("💾 保存").on_hover_text("Ctrl+S").clicked() {
                if self.doc.path.is_none() {
                    self.save_as();
                } else {
                    match self.doc.save() {
                        Ok(p) => {
                            self.disk_mtime = mtime(&p);
                            self.status = format!("保存: {}", p.display());
                        }
                        Err(e) => self.status = e,
                    }
                }
            }
            if ui.button("名前を付けて保存").clicked() {
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
            if ui.add(run).clicked() {
                self.run_sim();
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
                ui.separator();
                for (m, label) in [("left", "左揃え"), ("right", "右揃え"), ("top", "上揃え"), ("bottom", "下揃え"), ("vcenter", "縦中心揃え"), ("hcenter", "横中心揃え"), ("distribute_h", "横に等間隔"), ("distribute_v", "縦に等間隔")] {
                    if ui.add_enabled(self.sel.len() >= 2, egui::Button::new(label)).clicked() {
                        self.align_selected(m);
                        ui.close();
                    }
                }
            });
            self.tools_menu(ui);
            if ui.button("🖼 書き出し").on_hover_text("PNG / SVG").clicked() {
                self.export_image();
            }
            if ui.button("⛶ 全体表示").on_hover_text("F").clicked() {
                self.fit(canvas_hint);
            }
            ui.separator();
            let mut ch = false;
            ch |= ui.checkbox(&mut self.color_nets, "ネット色").changed();
            ch |= ui.checkbox(&mut self.pin_names, "ピン名").changed();
            ch |= ui.checkbox(&mut self.show_issues, "問題表示").changed();
            ui.checkbox(&mut self.show_grid, "グリッド");
            ui.checkbox(&mut self.auto_reroute, "移動時に再配線");
            ui.checkbox(&mut self.auto_save, "自動保存 (AI同期)").on_hover_text("編集を即座に保存し、MCP 側（AIエージェント）と同じファイルを共有します");
            if ch && self.show_issues {
                self.issues = check::check(&self.doc.sch, &self.conn).issues;
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
            ui.small("Ctrl+C/X/V/D コピー/切り取り/貼り付け/複製\nCtrl+A 全選択 / Y Y軸ミラー / T テキスト\nR 回転 / M ミラー / Del 削除\nW 配線 / A 自動配線 / L ラベル\nCtrl+Z/Y 元に戻す/やり直し\nホイール: ズーム, 中/右ドラッグ: パン\nF 全体表示");
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
            egui::Grid::new("propgrid").num_columns(3).striped(true).show(ui, |ui| {
                for (k, p) in c.props.iter().enumerate() {
                    let pname = library::prop_name(&c, k);
                    let desc = info.and_then(|i| i.props.get(k)).map(|p| p.description.clone()).unwrap_or_default();
                    ui.label(&pname).on_hover_text(&desc);
                    if k >= self.prop_buf.len() {
                        self.prop_buf.push(p.value.clone());
                    }
                    // "[a, b, c]" in the description -> choice list (like Qucs-S' property dialog)
                    let options: Vec<String> = desc
                        .rsplit_once('[')
                        .and_then(|(_, r)| r.split_once(']'))
                        .map(|(o, _)| o.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                        .unwrap_or_default();
                    if options.len() >= 2 && options.len() <= 30 {
                        let mut v = p.value.clone();
                        egui::ComboBox::from_id_salt(("prop", k)).width(130.0).selected_text(&v).show_ui(ui, |ui| {
                            for o in &options {
                                ui.selectable_value(&mut v, o.clone(), o);
                            }
                        });
                        if v != p.value {
                            self.prop_buf[k] = v.clone();
                            commit = Some((k, v));
                        }
                    } else {
                        let r = ui.add(egui::TextEdit::singleline(&mut self.prop_buf[k]).desired_width(130.0));
                        if r.lost_focus() && self.prop_buf[k] != p.value {
                            commit = Some((k, self.prop_buf[k].clone()));
                        }
                    }
                    let mut vis = p.visible();
                    if ui.checkbox(&mut vis, "").on_hover_text("回路図に表示").changed() {
                        toggle = Some(k);
                    }
                    ui.end_row();
                }
            });
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
            ui.small("部品を選んで「追加」→ スライダーを動かすと値を書き換えて再シミュレーションします");
            if ui.add_enabled(self.sel.len() == 1, egui::Button::new("選択中の部品の主な値を追加")).clicked() {
                let ci = self.sel[0];
                let c = &self.doc.sch.components[ci];
                if let Some(v) = c.props.first().and_then(|p| crate::rftools::parse_value(&p.value)) {
                    let unit: String = c.props[0].value.trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' || ch == 'e' || ch == 'E').trim().trim_start_matches(['f', 'p', 'n', 'u', 'µ', 'm', 'k', 'M', 'G', 'T']).trim().to_string();
                    self.tools.tunes.push(Tune { comp: c.name.clone(), prop: 0, unit, value: v, min: v / 10.0, max: v * 10.0 });
                } else {
                    self.status = "数値の値を持つ部品を選んでください".into();
                }
            }
            ui.checkbox(&mut self.tools.tune_auto, "値を変えたら自動でシミュレーション");
            for (k, t) in self.tools.tunes.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{} = {}", t.comp, crate::rftools::format_value(t.value, &t.unit)));
                    if ui.small_button("✖").clicked() {
                        remove = Some(k);
                    }
                });
                let r = ui.add(egui::Slider::new(&mut t.value, t.min..=t.max).logarithmic(true).show_value(false));
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    apply = Some(k);
                }
            }
        });
        if let Some(k) = remove {
            self.tools.tunes.remove(k);
        }
        if let Some(k) = apply {
            let (comp, prop) = (self.tools.tunes[k].comp.clone(), self.tools.tunes[k].prop);
            let v = crate::rftools::format_value(self.tools.tunes[k].value, &self.tools.tunes[k].unit);
            if let Some(ci) = self.doc.sch.component_index(&comp) {
                self.doc.checkpoint();
                self.doc.sch.components[ci].props[prop].value = v.clone();
                self.changed();
                self.status = format!("{comp} = {v}");
                if self.tools.tune_auto && self.sim_rx.is_none() {
                    self.run_sim();
                }
            }
        }
        self.tools.tune_open = open;
    }

    // ------------------------------------------------------------------ canvas

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let canvas = resp.rect;
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
                        let s = Stroke::new(2.0, self.pal().accent);
                        painter.line_segment([self.to_screen(canvas, last.0 as f64, last.1 as f64), self.to_screen(canvas, mid.0 as f64, mid.1 as f64)], s);
                        painter.line_segment([self.to_screen(canvas, mid.0 as f64, mid.1 as f64), self.to_screen(canvas, cur.0 as f64, cur.1 as f64)], s);
                    }
                    painter.circle_stroke(self.to_screen(canvas, cur.0 as f64, cur.1 as f64), 4.0, Stroke::new(1.0, self.pal().accent));
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
                            painter.circle_stroke(self.to_screen(canvas, p.x as f64, p.y as f64), 7.0, Stroke::new(2.0, self.pal().accent_hi));
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
                            painter.line_segment([self.to_screen(canvas, p.x as f64, p.y as f64), hover.unwrap()], Stroke::new(1.0, self.pal().accent_hi));
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
                            let s = Stroke::new(1.5, self.pal().accent);
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
                painter.rect_stroke(r, 2.0, Stroke::new(1.5, self.pal().accent_hi), egui::StrokeKind::Outside);
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
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, self.pal().accent), egui::StrokeKind::Inside);
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
            match self.doc.save() {
                Ok(p) => {
                    self.disk_mtime = mtime(&p);
                    self.status = format!("保存: {}", p.display());
                }
                Err(_) => self.save_as(),
            }
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
            if self.doc.dirty && !self.auto_save { " *" } else { "" }
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
        let bar = egui::Frame::new().fill(pal.bar).inner_margin(egui::Margin::symmetric(10, 6)).stroke(egui::Stroke::new(1.0, pal.border));
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
        egui::SidePanel::left("palette").default_width(230.0).show(ctx, |ui| self.palette(ui));
        egui::SidePanel::right("props").default_width(290.0).show(ctx, |ui| self.properties(ui));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
            let canvas = ui.max_rect();
            self.keys(ctx, canvas);
            self.canvas(ui);
        });
        self.sim_window(ctx);
        self.tool_windows(ctx);
        self.mc_window(ctx);
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
        // keep polling the file even when idle
        ctx.request_repaint_after(std::time::Duration::from_millis(350));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // no longer accepting files: let the next launch open its own window
        let _ = std::fs::remove_file(crate::mcp::gui_heartbeat_path());
    }
}

/// Indices of the wires in `wires` that lie on one of the `moved` wires (after merging).
fn wire_cover(wires: &[crate::schematic::Wire], moved: &[crate::schematic::Wire]) -> Vec<usize> {
    let inside = |w: &crate::schematic::Wire, p: (i32, i32)| (w.x1.min(w.x2)..=w.x1.max(w.x2)).contains(&p.0) && (w.y1.min(w.y2)..=w.y1.max(w.y2)).contains(&p.1);
    wires.iter().enumerate().filter(|(_, w)| moved.iter().any(|m| inside(m, w.p1()) && inside(m, w.p2()) || inside(w, m.p1()) && inside(w, m.p2()))).map(|(i, _)| i).collect()
}
