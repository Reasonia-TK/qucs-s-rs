//! Component database (properties, pins) and symbol geometry ported from Qucs-S.
//!
//! `data/components.json` – property names/defaults/descriptions and pins (measured with qucs-s)
//! `data/symbols.json`    – drawing primitives extracted from the Qucs-S C++ sources
//!                          (see tools/extract_symbols.py)

use crate::schematic::{Component, Schematic};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Deserialize, Clone, Debug)]
pub struct PropInfo {
    pub name: String,
    pub default: String,
    #[serde(default)]
    pub display: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct CompInfo {
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub props: Vec<PropInfo>,
    #[serde(default)]
    pub pins: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub pin_names: Option<Vec<String>>,
    #[serde(default)]
    pub ngspice: bool,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Pen {
    pub color: String,
    pub width: f64,
    #[serde(default)]
    pub style: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "k", rename_all = "lowercase")]
pub enum Prim {
    Line { x1: f64, y1: f64, x2: f64, y2: f64, pen: Pen },
    Arc { x: f64, y: f64, w: f64, h: f64, angle: i32, arclen: i32, pen: Pen },
    Rect { x: f64, y: f64, w: f64, h: f64, pen: Pen, brush: Option<String> },
    Ellipse { x: f64, y: f64, w: f64, h: f64, pen: Pen, brush: Option<String> },
    Polyline { points: Vec<[f64; 2]>, pen: Pen, brush: Option<String> },
    Text { x: f64, y: f64, s: String, color: String, size: f64, mcos: f64, msin: f64 },
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SymbolShape {
    #[serde(default)]
    pub prims: Vec<Prim>,
    #[serde(default)]
    pub ports: Vec<[f64; 2]>,
    #[serde(default)]
    pub bbox: [f64; 4],
    #[serde(default)]
    pub tx: f64,
    #[serde(default)]
    pub ty: f64,
    #[serde(default)]
    pub rotated: i32,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Variant {
    pub when: HashMap<String, String>,
    #[serde(flatten)]
    pub shape: SymbolShape,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SymbolDef {
    #[serde(flatten)]
    pub shape: SymbolShape,
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub pin_names: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

pub struct Library {
    pub comps: Vec<CompInfo>,
    by_type: HashMap<String, usize>,
    pub symbols: HashMap<String, SymbolDef>,
}

static LIB: OnceLock<Library> = OnceLock::new();

pub fn lib() -> &'static Library {
    LIB.get_or_init(|| {
        let comps_map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(include_str!("../data/components.json")).expect("components.json");
        let mut comps = Vec::new();
        for (_, v) in comps_map {
            if let Ok(c) = serde_json::from_value::<CompInfo>(v) {
                comps.push(c);
            }
        }
        let by_type = comps.iter().enumerate().map(|(i, c)| (c.typ.clone(), i)).collect();
        let symbols: HashMap<String, SymbolDef> =
            serde_json::from_str(include_str!("../data/symbols.json")).expect("symbols.json");
        Library { comps, by_type, symbols }
    })
}

impl Library {
    pub fn info(&self, typ: &str) -> Option<&CompInfo> {
        self.by_type.get(typ).map(|&i| &self.comps[i])
    }
}

// ---------------------------------------------------------------------------
// Font metrics (approximation of the Qucs default UI font)
// ---------------------------------------------------------------------------

/// Qucs-S draws schematic text with its default 12 pt font (16 px at 96 dpi).
pub const FONT_PX: f64 = 16.0;
pub const LINE_H: f64 = 21.0;

pub fn text_width(s: &str, px: f64) -> f64 {
    let mut w = 0.0;
    for ch in s.chars() {
        w += match ch {
            'i' | 'l' | 'j' | '!' | '.' | ',' | ':' | ';' | '\'' | '|' | 'I' => 0.30,
            'f' | 't' | 'r' | '(' | ')' | '[' | ']' | ' ' | '-' => 0.38,
            'm' | 'w' | 'M' | 'W' => 0.85,
            'A'..='Z' => 0.66,
            c if (c as u32) > 0x2E80 => 1.0,
            _ => 0.56,
        };
    }
    w * px
}

/// Width/height of a (multi-line) text block.
pub fn text_size(s: &str, px: f64) -> (f64, f64) {
    let lines: Vec<&str> = s.split('\n').collect();
    let w = lines.iter().map(|l| text_width(l, px)).fold(0.0, f64::max);
    (w, lines.len() as f64 * px * 1.31)
}

// ---------------------------------------------------------------------------
// Placed symbols
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PlacedPin {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    /// unit vector pointing away from the symbol body (where a wire should leave)
    pub dir: (i32, i32),
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Placed {
    pub prims: Vec<Prim>, // absolute coordinates
    pub pins: Vec<PlacedPin>,
    /// symbol body (absolute), without the text labels
    pub bbox: [f64; 4],
    /// label lines (name + visible properties) and their absolute top-left position
    pub label_lines: Vec<String>,
    pub label_pos: (f64, f64),
    pub label_bbox: Option<[f64; 4]>,
    pub kind: SymbolKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SymbolKind {
    Normal,
    Simulation,
    TextBlock,
    Generic,
}

fn qucs_rotate_shape(prims: &mut [Prim], ports: &mut [[f64; 2]], bbox: &mut [f64; 4]) {
    for p in prims.iter_mut() {
        match p {
            Prim::Line { x1, y1, x2, y2, .. } => {
                let (a, b) = (*y1, -*x1);
                *x1 = a;
                *y1 = b;
                let (a, b) = (*y2, -*x2);
                *x2 = a;
                *y2 = b;
            }
            Prim::Arc { x, y, w, h, angle, .. } => {
                let (nx, ny) = (*y, -*x - *w);
                *x = nx;
                *y = ny;
                std::mem::swap(w, h);
                *angle += 16 * 90;
                if *angle >= 16 * 360 {
                    *angle -= 16 * 360;
                }
            }
            Prim::Rect { x, y, w, h, .. } | Prim::Ellipse { x, y, w, h, .. } => {
                let (nx, ny) = (*y, -*x - *w);
                *x = nx;
                *y = ny;
                std::mem::swap(w, h);
            }
            Prim::Polyline { points, .. } => {
                for q in points.iter_mut() {
                    *q = [q[1], -q[0]];
                }
            }
            Prim::Text { x, y, mcos, msin, .. } => {
                let (nx, ny) = (*y, -*x);
                *x = nx;
                *y = ny;
                let (c, s) = (-*msin, *mcos);
                *msin = s;
                *mcos = c;
            }
        }
    }
    for q in ports.iter_mut() {
        *q = [q[1], -q[0]];
    }
    let [x1, y1, x2, y2] = *bbox;
    *bbox = [y1, -x2, y2, -x1];
}

fn qucs_mirror_shape(prims: &mut [Prim], ports: &mut [[f64; 2]], bbox: &mut [f64; 4]) {
    for p in prims.iter_mut() {
        match p {
            Prim::Line { y1, y2, .. } => {
                *y1 = -*y1;
                *y2 = -*y2;
            }
            Prim::Arc { y, h, angle, arclen, .. } => {
                *y = -*y - *h;
                if *angle > 16 * 180 {
                    *angle -= 16 * 360;
                }
                *angle = -*angle;
                *angle -= *arclen;
                if *angle < 0 {
                    *angle += 16 * 360;
                }
            }
            Prim::Rect { y, h, .. } | Prim::Ellipse { y, h, .. } => *y = -*y - *h,
            Prim::Polyline { points, .. } => {
                for q in points.iter_mut() {
                    q[1] = -q[1];
                }
            }
            Prim::Text { y, s, size, mcos, msin, .. } => {
                let (w, h) = text_size(s, *size);
                *y = -*y - (mcos.trunc()) * h + (msin.trunc()) * w;
            }
        }
    }
    for q in ports.iter_mut() {
        q[1] = -q[1];
    }
    let [x1, y1, x2, y2] = *bbox;
    *bbox = [x1, -y2, x2, -y1];
}

fn translate(p: &mut Prim, dx: f64, dy: f64) {
    match p {
        Prim::Line { x1, y1, x2, y2, .. } => {
            *x1 += dx;
            *x2 += dx;
            *y1 += dy;
            *y2 += dy;
        }
        Prim::Arc { x, y, .. } | Prim::Rect { x, y, .. } | Prim::Ellipse { x, y, .. } | Prim::Text { x, y, .. } => {
            *x += dx;
            *y += dy;
        }
        Prim::Polyline { points, .. } => {
            for q in points.iter_mut() {
                q[0] += dx;
                q[1] += dy;
            }
        }
    }
}

/// Property name for display (`R=10k`).
pub fn prop_name(c: &Component, i: usize) -> String {
    if let Some(n) = crate::qlib::param_name(c, i, None) {
        return n;
    }
    let lib = lib();
    if let Some(info) = lib.info(&c.typ) {
        if let Some(p) = info.props.get(i) {
            return p.name.clone();
        }
        // repeated trailing properties (Eqn y=..., SpicePar ...)
        if let Some(p) = info.props.last() {
            return p.name.clone();
        }
    }
    format!("p{}", i + 1)
}

pub fn is_text_block(typ: &str) -> bool {
    lib().symbols.get(typ).map(|s| s.title.is_some() && s.shape.prims.is_empty()).unwrap_or(false)
        || typ == "Eqn"
}

/// Text lines shown next to a component (name + displayed properties).
pub fn label_lines(c: &Component) -> Vec<String> {
    let mut lines = Vec::new();
    if c.show_name() && c.name != "*" && !c.name.is_empty() {
        lines.push(c.name.clone());
    }
    let text_block = is_text_block(&c.typ);
    for (i, p) in c.props.iter().enumerate() {
        if !p.visible() {
            continue;
        }
        if text_block && p.value.contains('=') {
            lines.push(p.value.clone());
        } else if text_block && c.typ == "NutmegEq" && i == 0 {
            lines.push(format!("Simulation={}", p.value));
        } else {
            lines.push(format!("{}={}", prop_name(c, i), p.value));
        }
    }
    lines
}

fn pick_shape<'a>(def: &'a SymbolDef, c: &Component) -> &'a SymbolShape {
    if def.variants.is_empty() {
        return &def.shape;
    }
    let info = lib().info(&c.typ);
    let value_of = |name: &str| -> Option<String> {
        let info = info?;
        let idx = info.props.iter().position(|p| p.name == name)?;
        Some(c.props.get(idx).map(|p| p.value.clone()).unwrap_or_else(|| info.props[idx].default.clone()))
    };
    for v in &def.variants {
        if v.when.iter().all(|(k, val)| {
            let cur = value_of(k).unwrap_or_default();
            if k == "in" {
                let n: i32 = cur.trim().parse().unwrap_or(2);
                n.clamp(2, 8).to_string() == *val
            } else {
                cur.trim() == val
            }
        }) {
            return &v.shape;
        }
    }
    &def.shape
}

fn wrap_label(label: &str) -> String {
    // like SimulationComponent::wrapLabel – break near every 14th character
    let words: Vec<&str> = label.split_whitespace().collect();
    let mut out = String::new();
    let mut cur = String::new();
    for w in words {
        if !cur.is_empty() && cur.len() + 1 + w.len() > 16 {
            out.push_str(&cur);
            out.push('\n');
            cur.clear();
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    out.push_str(&cur);
    out
}

const SIM_FONT: f64 = 16.0;

/// Geometry of a component placed in a schematic (absolute coordinates).
pub fn place(c: &Component, sch_dir: Option<&Path>) -> Placed {
    let lib = lib();
    let def = lib.symbols.get(&c.typ);
    let mut kind = SymbolKind::Normal;
    let ext = match c.typ.as_str() {
        "Lib" => crate::qlib::lib_symbol(c),
        "Sub" => crate::qlib::sub_symbol(c, sch_dir),
        _ => None,
    };
    let (mut prims, mut ports, mut bbox, rotated) = match def {
        _ if ext.is_some() => {
            let s = &ext.as_ref().unwrap().shape;
            (s.prims.clone(), s.ports.clone(), s.bbox, 0)
        }
        Some(d) if !d.shape.prims.is_empty() => {
            let s = pick_shape(d, c);
            (s.prims.clone(), s.ports.clone(), s.bbox, s.rotated)
        }
        Some(d) if c.typ.starts_with('.') => {
            kind = SymbolKind::Simulation;
            let label = wrap_label(d.description.as_deref().unwrap_or(&c.typ));
            let (w, h) = text_size(&label, SIM_FONT);
            let color = d.color.clone().unwrap_or_else(|| "#000080".into());
            let pen = Pen { color: color.clone(), width: 2.0, style: "SolidLine".into() };
            let (ax, ay, bw, bh) = (-5.0, -5.0, w + 10.0, h + 10.0);
            let mut prims = vec![Prim::Rect { x: ax, y: ay, w: bw, h: bh, pen: pen.clone(), brush: None }];
            let pts = vec![
                [ax + bw, ay],
                [ax + bw + 5.0, ay + 5.0],
                [ax + bw + 5.0, ay + bh + 5.0],
                [ax + 5.0, ay + bh + 5.0],
                [ax, ay + bh],
            ];
            prims.push(Prim::Polyline { points: pts, pen: pen.clone(), brush: None });
            prims.push(Prim::Line { x1: ax + bw, y1: ay + bh, x2: ax + bw + 5.0, y2: ay + bh + 5.0, pen: pen.clone() });
            for (k, l) in label.split('\n').enumerate() {
                prims.push(Prim::Text {
                    x: 0.0,
                    y: k as f64 * SIM_FONT * 1.25,
                    s: l.to_string(),
                    color: format!("{color}|bold"),
                    size: SIM_FONT,
                    mcos: 1.0,
                    msin: 0.0,
                });
            }
            (prims, vec![], [ax - 3.0, ay - 3.0, ax + bw + 8.0, ay + bh + 8.0], 0)
        }
        Some(d) if d.title.is_some() || c.typ == "Eqn" => {
            kind = SymbolKind::TextBlock;
            let title = d.title.clone().unwrap_or_else(|| "Equation".into());
            let color = d.color.clone().unwrap_or_else(|| "#000080".into());
            let (w, h) = text_size(&title, 16.0);
            let (xb, yb) = ((w / 2.0).round(), (h / 2.0).round());
            let pen = Pen { color: color.clone(), width: 2.0, style: "SolidLine".into() };
            let prims = vec![
                Prim::Line { x1: -xb, y1: -yb, x2: -xb, y2: yb, pen: pen.clone() },
                Prim::Line { x1: -xb, y1: yb, x2: xb + 3.0, y2: yb, pen },
                Prim::Text { x: -xb + 4.0, y: -yb - 3.0, s: title, color: "#000000".into(), size: 16.0, mcos: 1.0, msin: 0.0 },
            ];
            (prims, vec![], [-xb - 3.0, -yb - 5.0, xb + 9.0, yb + 3.0], 0)
        }
        _ => {
            kind = SymbolKind::Generic;
            generic_symbol(c, sch_dir)
        }
    };

    // Qucs load semantics: mirror first, then rotate from the constructor's orientation
    let k = rotated & 3;
    if kind == SymbolKind::Normal || kind == SymbolKind::Generic {
        if c.mirror != 0 {
            qucs_mirror_shape(&mut prims, &mut ports, &mut bbox);
        }
        let mut t = c.rotate & 3;
        if k > t {
            t += 4;
        }
        for _ in k..t {
            qucs_rotate_shape(&mut prims, &mut ports, &mut bbox);
        }
    }
    let (cx, cy) = (c.x as f64, c.y as f64);
    for p in prims.iter_mut() {
        translate(p, cx, cy);
    }
    let names = match &ext {
        Some(e) => e.pin_names.clone(),
        None => def.map(|d| d.pin_names.clone()).unwrap_or_default(),
    };
    let body = [bbox[0] + cx, bbox[1] + cy, bbox[2] + cx, bbox[3] + cy];
    let pins: Vec<PlacedPin> = ports
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let (x, y) = ((q[0] + cx).round() as i32, (q[1] + cy).round() as i32);
            PlacedPin {
                index: i,
                name: names.get(i).cloned().unwrap_or_else(|| (i + 1).to_string()),
                x,
                y,
                dir: pin_direction(&prims, x, y, &body),
            }
        })
        .collect();
    let lines = label_lines(c);
    // simulation blocks always put their text right below the box (SimulationComponent::updateComponentBounds)
    let label_pos = if kind == SymbolKind::Simulation { (cx, cy + bbox[3]) } else { (cx + c.tx as f64, cy + c.ty as f64) };
    let label_bbox = if lines.is_empty() {
        None
    } else {
        let w = lines.iter().map(|l| text_width(l, FONT_PX)).fold(0.0, f64::max);
        let h = lines.len() as f64 * LINE_H;
        Some([label_pos.0, label_pos.1, label_pos.0 + w, label_pos.1 + h])
    };
    Placed { prims, pins, bbox: body, label_lines: lines, label_pos, label_bbox, kind }
}

/// Direction in which a wire should leave the pin: opposite to the symbol lead that ends there.
fn pin_direction(prims: &[Prim], x: i32, y: i32, body: &[f64; 4]) -> (i32, i32) {
    let (fx, fy) = (x as f64, y as f64);
    for p in prims {
        if let Prim::Line { x1, y1, x2, y2, .. } = p {
            let (ox, oy) = if (x1 - fx).abs() < 0.5 && (y1 - fy).abs() < 0.5 {
                (*x2, *y2)
            } else if (x2 - fx).abs() < 0.5 && (y2 - fy).abs() < 0.5 {
                (*x1, *y1)
            } else {
                continue;
            };
            let (dx, dy) = (fx - ox, fy - oy);
            if dx.abs() > dy.abs() && dx.abs() > 0.5 {
                return (dx.signum() as i32, 0);
            } else if dy.abs() > 0.5 {
                return (0, dy.signum() as i32);
            }
        }
    }
    // fall back to the side of the body the pin is closest to
    let cx = (body[0] + body[2]) / 2.0;
    let cy = (body[1] + body[3]) / 2.0;
    let d = [
        ((fx - body[0]).abs(), (-1, 0)),
        ((fx - body[2]).abs(), (1, 0)),
        ((fy - body[1]).abs(), (0, -1)),
        ((fy - body[3]).abs(), (0, 1)),
    ];
    let mut best = d[0];
    for e in d {
        if e.0 < best.0 {
            best = e;
        }
    }
    if (fx - cx).abs() < 1.0 && (fy - cy).abs() < 1.0 {
        return (0, 1);
    }
    best.1
}

/// Box symbol for components whose geometry is dynamic (Sub, SPICE, Verilog ...).
fn generic_symbol(c: &Component, sch_dir: Option<&Path>) -> (Vec<Prim>, Vec<[f64; 2]>, [f64; 4], i32) {
    let lib = lib();
    let pen = Pen { color: "#000080".into(), width: 2.0, style: "SolidLine".into() };
    let mut ports: Vec<[f64; 2]> = lib
        .info(&c.typ)
        .and_then(|i| i.pins.clone())
        .unwrap_or_default();
    let mut title = c.typ.clone();
    if c.typ == "Sub" {
        if let Some(n) = subcircuit_port_count(c, sch_dir) {
            ports = default_sub_ports(n);
        }
        if let Some(f) = c.props.first() {
            title = Path::new(&f.value).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(title);
        }
    }
    let (mut x1, mut y1, mut x2, mut y2) = (-20.0f64, -20.0f64, 20.0f64, 20.0f64);
    for p in &ports {
        x1 = x1.min(p[0] + 10.0);
        x2 = x2.max(p[0] - 10.0);
        y1 = y1.min(p[1] - 10.0);
        y2 = y2.max(p[1] + 10.0);
    }
    if !ports.is_empty() {
        let xs: Vec<f64> = ports.iter().map(|p| p[0]).collect();
        let minx = xs.iter().cloned().fold(f64::MAX, f64::min);
        let maxx = xs.iter().cloned().fold(f64::MIN, f64::max);
        if maxx - minx >= 40.0 {
            x1 = minx + 10.0;
            x2 = maxx - 10.0;
        }
    }
    let mut prims = vec![Prim::Rect { x: x1, y: y1, w: x2 - x1, h: y2 - y1, pen: pen.clone(), brush: None }];
    for p in &ports {
        let ex = p[0].clamp(x1, x2);
        let ey = p[1].clamp(y1, y2);
        if (ex, ey) != (p[0], p[1]) {
            prims.push(Prim::Line { x1: p[0], y1: p[1], x2: ex, y2: ey, pen: pen.clone() });
        }
    }
    let (tw, _) = text_size(&title, 10.0);
    prims.push(Prim::Text {
        x: (x1 + x2) / 2.0 - tw / 2.0,
        y: (y1 + y2) / 2.0 - 7.0,
        s: title,
        color: "#000080".into(),
        size: 10.0,
        mcos: 1.0,
        msin: 0.0,
    });
    (prims, ports, [x1 - 2.0, y1 - 2.0, x2 + 2.0, y2 + 2.0], 0)
}

fn subcircuit_port_count(c: &Component, sch_dir: Option<&Path>) -> Option<usize> {
    let file = c.props.first()?.value.clone();
    let p = Path::new(&file);
    let path = if p.is_absolute() { p.to_path_buf() } else { sch_dir?.join(p) };
    let sub = Schematic::load(&path).ok()?;
    Some(sub.components.iter().filter(|k| k.typ == "Port").count())
}

/// Default Qucs subcircuit symbol pin layout (ports alternate left / right).
fn default_sub_ports(n: usize) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    let h = 30.0 * (((n as f64) - 1.0) / 2.0).floor() + 15.0;
    let mut yl = -h + 15.0;
    let mut yr = -h + 15.0;
    for i in 0..n {
        if i % 2 == 0 {
            out.push([-30.0, yl]);
            yl += 30.0;
        } else {
            out.push([30.0, yr]);
            yr += 30.0;
        }
    }
    out
}

/// Build a fresh component of `typ` with default properties.
pub fn new_component(typ: &str, name: &str, x: i32, y: i32) -> Result<Component, String> {
    let lib = lib();
    let info = lib.info(typ).ok_or_else(|| format!("unknown component type '{typ}' (see list_component_types)"))?;
    let mut c = if !info.template.is_empty() {
        Component::parse(&info.template)?
    } else {
        Component {
            typ: typ.into(),
            name: name.into(),
            active: 1,
            x: 0,
            y: 0,
            tx: 0,
            ty: 0,
            mirror: 0,
            rotate: 0,
            props: info
                .props
                .iter()
                .map(|p| crate::schematic::Prop { value: p.default.clone(), display: if p.display { "1" } else { "0" }.into() })
                .collect(),
        }
    };
    c.name = name.to_string();
    c.x = x;
    c.y = y;
    c.mirror = 0;
    c.rotate = 0;
    if let Some(s) = lib.symbols.get(typ) {
        if !typ.starts_with('.') {
            c.tx = s.shape.tx.round() as i32;
            c.ty = s.shape.ty.round() as i32;
        }
        // symbols that are rotated by their constructor (voltage sources...) are
        // vertical by default; keep Qucs' convention: rotate count = constructor rotation
        c.rotate = s.shape.rotated & 3;
    }
    if typ.starts_with('.') {
        c.tx = 0;
        let label = wrap_label(lib.symbols.get(typ).and_then(|s| s.description.clone()).as_deref().unwrap_or(typ));
        let (_, h) = text_size(&label, SIM_FONT);
        c.ty = (h + 18.0).round() as i32;
    }
    Ok(c)
}

/// Prefix for automatic names (R, C, V, OP ...).
pub fn name_prefix(typ: &str) -> String {
    let lib = lib();
    if let Some(i) = lib.info(typ) {
        if !i.template.is_empty() {
            if let Some(n) = i.template.split_whitespace().nth(1) {
                if n != "*" {
                    return n.trim_end_matches(char::is_numeric).to_string();
                }
            }
        }
        if !i.prefix.is_empty() {
            return i.prefix.clone();
        }
    }
    typ.trim_start_matches('.').to_string()
}
