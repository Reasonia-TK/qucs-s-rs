//! Desktop editor (egui). It draws the very same scene the MCP snapshots use and
//! follows the .sch file live, so the user can watch – and correct – the agent.

use crate::check::{self, Issue};
use crate::doc::{snap, Doc};
use crate::library::{self, lib};
use crate::netlist::Connectivity;
use crate::render::{build_scene, Color, Overlay, Scene, Shape};
use crate::schematic::Schematic;
use crate::sim::{self, SimResult};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
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
    detach_own_console();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 780.0]).with_title("Qucs-S RS"),
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
    box_start: Option<Pos2>,
    wire_pts: Vec<(i32, i32)>,
    autowire_first: Option<String>,
    place_rot: i32,
    query: String,
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
    sim_rx: Option<Receiver<Result<SimResult, String>>>,
    sim: Option<SimResult>,
    show_sim: bool,
    plot_analysis: usize,
    plot_vectors: Vec<String>,
    plot_tex: Option<egui::TextureHandle>,
    plot_dirty: bool,
    prop_for: Option<usize>,
    prop_buf: Vec<String>,
    name_buf: String,
    label_prompt: Option<(String, String)>, // (terminal, text)
    cursor: (i32, i32),
    last_state: String,
    last_state_write: Instant,
    icons: std::collections::HashMap<String, (Vec<Shape>, [f64; 4])>,
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

fn paint_shapes_xf(painter: &egui::Painter, shapes: &[Shape], alpha: f32, z: f32, xf: &dyn Fn(f64, f64) -> Pos2) {
    let col = |c: &Color| Color32::from_rgba_unmultiplied(c.0, c.1, c.2, (c.3 as f32 * alpha) as u8);
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
        cc.egui_ctx.set_visuals(egui::Visuals::light());
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
            box_start: None,
            wire_pts: vec![],
            autowire_first: None,
            place_rot: 0,
            query: String::new(),
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
            prop_for: None,
            prop_buf: vec![],
            name_buf: String::new(),
            label_prompt: None,
            cursor: (0, 0),
            last_state: String::new(),
            last_state_write: Instant::now(),
            icons: Default::default(),
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
            "tool": match &self.tool { Tool::Select => "select".to_string(), Tool::Wire => "wire".into(), Tool::AutoWire => "autowire".into(), Tool::Label => "label".into(), Tool::Place(t) => format!("place:{t}") },
            "unsaved_changes": self.doc.dirty,
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

    fn delete_selected(&mut self) {
        if self.sel.is_empty() && self.sel_wires.is_empty() {
            return;
        }
        self.doc.checkpoint();
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
        self.sim_rx = Some(rx);
        self.status = "シミュレーション実行中…".into();
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Qucs schematic", &["sch"]).pick_file() {
            match Doc::open(&p) {
                Ok(d) => {
                    self.doc = d;
                    self.disk_mtime = mtime(&p);
                    self.need_fit = true;
                    self.sel.clear();
                    self.sel_wires.clear();
                    self.sim = None;
                    self.changed_no_save();
                }
                Err(e) => self.status = e,
            }
        }
    }

    fn changed_no_save(&mut self) {
        let a = self.auto_save;
        self.auto_save = false;
        self.changed();
        self.auto_save = a;
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
            ..Default::default()
        };
        self.scene = build_scene(&self.doc.sch, &self.conn, &ov);
        self.scene_key = key;
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
        let c = Color32::from_gray(185);
        for gx in gx0..=gx1 {
            for gy in gy0..=gy1 {
                let p = self.to_screen(canvas, gx as f64 * step, gy as f64 * step);
                painter.rect_filled(Rect::from_center_size(p, Vec2::splat(1.4)), 0.0, c);
            }
        }
    }

    // ------------------------------------------------------------------ UI parts

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
            if ui.button("▶ シミュレーション").clicked() {
                self.run_sim();
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
            if let Ok(c) = library::new_component(typ, "", 0, 0) {
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
    fn palette_row(&mut self, ui: &mut egui::Ui, typ: &str, caption: &str, selected: bool) -> egui::Response {
        const ICON: Vec2 = Vec2::new(46.0, 34.0);
        let w = ui.available_width();
        // lay the text out first: long captions wrap, and the row grows to fit them
        let text_color = ui.visuals().text_color();
        let galley = ui.painter().layout(format!("{caption}\n{typ}"), FontId::proportional(12.5), text_color, (w - ICON.x - 12.0).max(40.0));
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
        ui.heading("部品");
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
                        if self.palette_row(ui, &c.typ, &c.caption, selected).on_hover_text(&c.description).clicked() {
                            self.tool = Tool::Place(c.typ.clone());
                            self.place_rot = library::new_component(&c.typ, "X", 0, 0).map(|c| c.rotate).unwrap_or(0);
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
            }
            ui.separator();
            ui.label("ショートカット");
            ui.small("R 回転 / M ミラー / Del 削除\nW 配線 / A 自動配線 / L ラベル\nCtrl+Z/Y 元に戻す/やり直し\nホイール: ズーム, 中/右ドラッグ: パン\nF 全体表示");
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
                    ui.label(&pname).on_hover_text(desc);
                    if k >= self.prop_buf.len() {
                        self.prop_buf.push(p.value.clone());
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut self.prop_buf[k]).desired_width(130.0));
                    if r.lost_focus() && self.prop_buf[k] != p.value {
                        commit = Some((k, self.prop_buf[k].clone()));
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
        if !self.show_sim {
            return;
        }
        let mut open = true;
        egui::Window::new("シミュレーション結果").open(&mut open).default_size([920.0, 640.0]).show(ctx, |ui| {
            let Some(r) = &self.sim else {
                ui.label("結果なし");
                return;
            };
            if !r.errors.is_empty() {
                ui.colored_label(Color32::RED, r.errors.join("\n"));
            }
            if r.plots.is_empty() {
                return;
            }
            let names: Vec<String> = r.plots.iter().map(|p| format!("{} ({})", p.name, p.kind)).collect();
            let mut changed = false;
            ui.horizontal(|ui| {
                egui::ComboBox::from_label("解析").selected_text(names.get(self.plot_analysis).cloned().unwrap_or_default()).show_ui(ui, |ui| {
                    for (k, n) in names.iter().enumerate() {
                        changed |= ui.selectable_value(&mut self.plot_analysis, k, n).changed();
                    }
                });
            });
            let p = &r.plots[self.plot_analysis.min(r.plots.len() - 1)];
            if p.kind == "op" {
                egui::Grid::new("op").striped(true).show(ui, |ui| {
                    for v in &p.vectors {
                        ui.label(&v.name);
                        ui.label(crate::plot::eng(v.re[0]));
                        ui.end_row();
                    }
                });
                return;
            }
            ui.horizontal_wrapped(|ui| {
                for v in p.vectors.iter().skip(1) {
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
                let base = p.base.clone();
                let plots = r.find_all(Some(if p.sweep.is_some() { &base } else { &p.name })).unwrap_or_default();
                let vecs: Vec<String> = self.plot_vectors.iter().filter(|n| p.vector(n).is_some()).cloned().collect();
                if let Ok(svg) = crate::plot::plot_analysis(&plots, &vecs, None) {
                    if let Ok((w, h, rgba)) = crate::render::svg_to_rgba(&svg) {
                        let img = egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba);
                        self.plot_tex = Some(ctx.load_texture("plot", img, egui::TextureOptions::LINEAR));
                    }
                }
            }
            if let Some(t) = &self.plot_tex {
                let avail = ui.available_width();
                let s = t.size_vec2();
                ui.image((t.id(), s * (avail / s.x).min(1.0)));
            }
        });
        if !open {
            self.show_sim = false;
        }
    }

    // ------------------------------------------------------------------ canvas

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let canvas = resp.rect;
        painter.rect_filled(canvas, 0.0, Color32::WHITE);
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
                    if let Ok(mut c) = library::new_component(&t, "?", x, y) {
                        c.rotate = self.place_rot;
                        let pl = library::place(&c, None);
                        let mut ghost = Vec::new();
                        for p in &pl.prims {
                            crate::render::prim_shapes(p, &mut ghost, Some(Color(0, 0, 160, 255)));
                        }
                        self.paint_shapes(&painter, canvas, &ghost, 0.5);
                    }
                    if resp.clicked() {
                        self.doc.checkpoint();
                        match self.doc.add_component(&t, None, x, y, Some(self.place_rot), None, &Default::default()) {
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
                        let s = Stroke::new(2.0, Color32::from_rgb(0, 120, 255));
                        painter.line_segment([self.to_screen(canvas, last.0 as f64, last.1 as f64), self.to_screen(canvas, mid.0 as f64, mid.1 as f64)], s);
                        painter.line_segment([self.to_screen(canvas, mid.0 as f64, mid.1 as f64), self.to_screen(canvas, cur.0 as f64, cur.1 as f64)], s);
                    }
                    painter.circle_stroke(self.to_screen(canvas, cur.0 as f64, cur.1 as f64), 4.0, Stroke::new(1.0, Color32::from_rgb(0, 120, 255)));
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
                            painter.circle_stroke(self.to_screen(canvas, p.x as f64, p.y as f64), 7.0, Stroke::new(2.0, Color32::from_rgb(255, 120, 0)));
                            painter.text(self.to_screen(canvas, p.x as f64, p.y as f64) + Vec2::new(8.0, -18.0), Align2::LEFT_TOP, &pin, FontId::proportional(13.0), Color32::from_rgb(200, 80, 0));
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
                            painter.line_segment([self.to_screen(canvas, p.x as f64, p.y as f64), hover.unwrap()], Stroke::new(1.0, Color32::from_rgb(255, 120, 0)));
                        }
                    }
                }
                if resp.secondary_clicked() {
                    self.autowire_first = None;
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
        painter.text(canvas.right_bottom() - Vec2::new(8.0, 6.0), Align2::RIGHT_BOTTOM, format!("x={} y={}  zoom {:.0}%", self.cursor.0, self.cursor.1, self.zoom * 100.0 / 1.5), FontId::proportional(12.0), Color32::from_gray(110));
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
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(press) = resp.ctx.input(|i| i.pointer.press_origin()) {
                let (px, py) = self.to_sch_f(canvas, press);
                if let Some(ci) = self.comp_at(px, py) {
                    if !self.sel.contains(&ci) {
                        self.sel = vec![ci];
                        self.sel_wires.clear();
                    }
                    let orig = self.sel.iter().map(|&i| (i, self.doc.sch.components[i].x, self.doc.sch.components[i].y)).collect();
                    self.drag = Some(Drag { start: (px.round() as i32, py.round() as i32), orig, moved: false });
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
        if let Some(b) = self.box_start {
            if let Some(h) = resp.hover_pos().or(resp.interact_pointer_pos()) {
                let r = Rect::from_two_pos(b, h);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(0, 120, 255)), egui::StrokeKind::Inside);
                painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(0, 120, 255, 25));
            }
        }
        if resp.drag_stopped() {
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
                } else if let Some(wi) = self.wire_at(px, py) {
                    if !shift {
                        self.sel.clear();
                        self.sel_wires.clear();
                    }
                    self.sel_wires.push(wi);
                } else {
                    self.sel.clear();
                    self.sel_wires.clear();
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
        if let Some(rx) = &self.sim_rx {
            if let Ok(r) = rx.try_recv() {
                self.sim_rx = None;
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

        let screen = ctx.content_rect();
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui, screen));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.sim_rx.is_some() {
                    ui.spinner();
                }
                ui.label(&self.status);
                if self.sim.is_some() && ui.small_button("結果").clicked() {
                    self.show_sim = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    ui.colored_label(Color32::from_rgb(180, 90, 0), "ファイルが外部（AIエージェント）で変更されました。");
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
                        let c = if i.severity == "error" { Color32::RED } else { Color32::from_rgb(170, 110, 0) };
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
