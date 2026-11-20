//! Scene building: one display list that is drawn by the GUI (egui) and
//! rasterised for MCP snapshots (SVG -> resvg). What the agent sees is exactly
//! what the editor shows.

use crate::library::{Prim, FONT_PX, LINE_H};
use crate::netlist::Connectivity;
use crate::schematic::{painting_tokens, Schematic};
use std::fmt::Write as _;

#[derive(Clone, Debug)]
pub enum Shape {
    Line { a: (f64, f64), b: (f64, f64), color: Color, width: f64, dash: bool },
    Poly { pts: Vec<(f64, f64)>, color: Color, width: f64, fill: Option<Color>, closed: bool, dash: bool },
    Ellipse { c: (f64, f64), rx: f64, ry: f64, color: Color, width: f64, fill: Option<Color> },
    Text { pos: (f64, f64), text: String, size: f64, color: Color, bold: bool, angle: f64, bg: Option<Color> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

impl Color {
    pub const BLACK: Color = Color(0, 0, 0, 255);
    pub const WIRE: Color = Color(0, 0, 128, 255);
    pub const RED: Color = Color(220, 0, 0, 255);
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
    pub fn alpha(&self) -> f64 {
        self.3 as f64 / 255.0
    }
    pub fn parse(s: &str) -> Color {
        let s = s.trim();
        if let Some(h) = s.strip_prefix('#') {
            if h.len() >= 6 {
                let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
                return Color(v(0), v(2), v(4), 255);
            }
        }
        Color::WIRE
    }
}

#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub shapes: Vec<Shape>,
    pub bounds: [f64; 4],
}

impl Scene {
    fn push(&mut self, s: Shape) {
        self.shapes.push(s);
    }
    fn grow(&mut self, x: f64, y: f64) {
        let b = &mut self.bounds;
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub color_nets: bool,
    pub net_tags: bool,
    pub pin_names: bool,
    pub highlight: Vec<usize>,      // component indices
    pub highlight_nets: Vec<usize>, // net indices
    pub selected: Vec<usize>,
    pub selected_wires: Vec<usize>,
    pub markers: Vec<(f64, f64, String)>, // issue markers
    /// Wire / junction color override (GUI themes); `None` keeps Qucs dark blue.
    pub wire_color: Option<Color>,
}

pub const NET_PALETTE: [Color; 10] = [
    Color(0x1f, 0x77, 0xb4, 255),
    Color(0xd6, 0x27, 0x28, 255),
    Color(0x2c, 0xa0, 0x2c, 255),
    Color(0x94, 0x67, 0xbd, 255),
    Color(0xff, 0x7f, 0x0e, 255),
    Color(0x8c, 0x56, 0x4b, 255),
    Color(0xe3, 0x77, 0xc2, 255),
    Color(0x17, 0xbe, 0xcf, 255),
    Color(0xbc, 0xbd, 0x22, 255),
    Color(0x7f, 0x7f, 0x7f, 255),
];

fn arc_points(x: f64, y: f64, w: f64, h: f64, angle16: i32, len16: i32) -> Vec<(f64, f64)> {
    let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
    let a0 = angle16 as f64 / 16.0;
    let span = len16 as f64 / 16.0;
    let n = ((span.abs() / 8.0).ceil() as usize).clamp(4, 64);
    (0..=n)
        .map(|i| {
            let a = (a0 + span * i as f64 / n as f64).to_radians();
            (cx + rx * a.cos(), cy - ry * a.sin())
        })
        .collect()
}

fn pen_width(w: f64) -> f64 {
    if w <= 0.0 {
        1.0
    } else {
        w
    }
}

pub fn prim_shapes(p: &Prim, out: &mut Vec<Shape>, override_color: Option<Color>) {
    let col = |c: &str| override_color.unwrap_or_else(|| Color::parse(c));
    match p {
        Prim::Line { x1, y1, x2, y2, pen } => out.push(Shape::Line {
            a: (*x1, *y1),
            b: (*x2, *y2),
            color: col(&pen.color),
            width: pen_width(pen.width),
            dash: pen.style.contains("Dash") || pen.style.contains("Dot"),
        }),
        Prim::Arc { x, y, w, h, angle, arclen, pen } => out.push(Shape::Poly {
            pts: arc_points(*x, *y, *w, *h, *angle, *arclen),
            color: col(&pen.color),
            width: pen_width(pen.width),
            fill: None,
            closed: false,
            dash: false,
        }),
        Prim::Rect { x, y, w, h, pen, brush } => out.push(Shape::Poly {
            pts: vec![(*x, *y), (x + w, *y), (x + w, y + h), (*x, y + h)],
            color: col(&pen.color),
            width: pen_width(pen.width),
            fill: brush.as_ref().filter(|b| b.as_str() != "none").map(|b| col(b)),
            closed: true,
            dash: pen.style.contains("Dash"),
        }),
        Prim::Ellipse { x, y, w, h, pen, brush } => out.push(Shape::Ellipse {
            c: (x + w / 2.0, y + h / 2.0),
            rx: w / 2.0,
            ry: h / 2.0,
            color: col(&pen.color),
            width: if pen.color == "none" { 0.0 } else { pen_width(pen.width) },
            fill: brush.as_ref().filter(|b| b.as_str() != "none").map(|b| col(b)),
        }),
        Prim::Polyline { points, pen, brush } => out.push(Shape::Poly {
            pts: points.iter().map(|q| (q[0], q[1])).collect(),
            color: col(&pen.color),
            width: if pen.color == "none" { 0.0 } else { pen_width(pen.width) },
            fill: brush.as_ref().filter(|b| b.as_str() != "none").map(|b| col(b)),
            closed: brush.is_some(),
            dash: false,
        }),
        Prim::Text { x, y, s, color, size, mcos, msin } => {
            let (c, bold) = match color.split_once('|') {
                Some((c, b)) => (c, b == "bold"),
                None => (color.as_str(), false),
            };
            out.push(Shape::Text {
                pos: (*x, *y),
                text: s.clone(),
                size: *size,
                color: override_color.unwrap_or_else(|| Color::parse(c)),
                bold,
                angle: -msin.atan2(*mcos).to_degrees(),
                bg: None,
            })
        }
    }
}

/// Build the display list of a schematic.
pub fn build_scene(sch: &Schematic, conn: &Connectivity, ov: &Overlay) -> Scene {
    let mut sc = Scene { shapes: Vec::new(), bounds: [f64::MAX, f64::MAX, f64::MIN, f64::MIN] };

    // highlight halos first (under everything)
    for &ci in ov.highlight.iter().chain(ov.selected.iter()) {
        if let Some(p) = conn.placed.get(ci) {
            let b = p.bbox;
            let col = if ov.selected.contains(&ci) { Color(255, 140, 0, 90) } else { Color(255, 230, 0, 120) };
            sc.push(Shape::Poly {
                pts: vec![(b[0] - 4.0, b[1] - 4.0), (b[2] + 4.0, b[1] - 4.0), (b[2] + 4.0, b[3] + 4.0), (b[0] - 4.0, b[3] + 4.0)],
                color: col,
                width: 1.0,
                fill: Some(col),
                closed: true,
                dash: false,
            });
        }
    }

    // paintings
    for line in &sch.paintings {
        painting_shapes(line, &mut sc);
    }

    // embedded result diagrams (<Diagrams>): frame, grid and trace names like Qucs-S
    diagram_shapes(&sch.diagrams, &mut sc);

    // wires
    for (wi, w) in sch.wires.iter().enumerate() {
        let net = conn.wire_net.get(wi).copied().unwrap_or(0);
        let mut color = if ov.color_nets { NET_PALETTE[net % NET_PALETTE.len()] } else { ov.wire_color.unwrap_or(Color::WIRE) };
        let mut width = 2.0;
        if ov.highlight_nets.contains(&net) {
            sc.push(Shape::Line {
                a: (w.x1 as f64, w.y1 as f64),
                b: (w.x2 as f64, w.y2 as f64),
                color: Color(255, 230, 0, 160),
                width: 8.0,
                dash: false,
            });
        }
        if ov.selected_wires.contains(&wi) {
            color = Color(255, 120, 0, 255);
            width = 3.0;
        }
        sc.push(Shape::Line { a: (w.x1 as f64, w.y1 as f64), b: (w.x2 as f64, w.y2 as f64), color, width, dash: false });
        sc.grow(w.x1 as f64, w.y1 as f64);
        sc.grow(w.x2 as f64, w.y2 as f64);
    }

    // components
    for (ci, (c, p)) in sch.components.iter().zip(conn.placed.iter()).enumerate() {
        let off = c.active & 3 == 0;
        let ov_col = if off { Some(Color(160, 160, 160, 255)) } else { None };
        for prim in &p.prims {
            prim_shapes(prim, &mut sc.shapes, ov_col);
        }
        if c.active & 3 != 1 {
            let b = p.bbox;
            let col = if off { Color::RED } else { Color(0, 128, 0, 255) };
            sc.push(Shape::Line { a: (b[0], b[1]), b: (b[2], b[3]), color: col, width: 1.0, dash: false });
            sc.push(Shape::Line { a: (b[0], b[3]), b: (b[2], b[1]), color: col, width: 1.0, dash: false });
        }
        for (k, l) in p.label_lines.iter().enumerate() {
            sc.push(Shape::Text {
                pos: (p.label_pos.0, p.label_pos.1 + k as f64 * LINE_H),
                text: l.clone(),
                size: FONT_PX,
                color: Color::BLACK,
                bold: false,
                angle: 0.0,
                bg: None,
            });
        }
        sc.grow(p.bbox[0], p.bbox[1]);
        sc.grow(p.bbox[2], p.bbox[3]);
        if let Some(lb) = p.label_bbox {
            sc.grow(lb[0], lb[1]);
            sc.grow(lb[2], lb[3]);
        }
        let _ = ci;
    }

    // wire labels
    for w in &sch.wires {
        if w.label.is_empty() {
            continue;
        }
        let (ax, ay) = w.label_anchor();
        let (cx, cy) = (ax as f64, ay as f64);
        let (lx, ly) = (w.lx as f64, w.ly as f64);
        let tw = crate::library::text_width(&w.label, FONT_PX);
        // like WireLabel::paint: leader from the wire to the nearest text corner + edge lines + arc
        let br = [lx - 3.0, ly - 3.0, lx + tw + 3.0, ly + LINE_H + 3.0];
        let right = lx + tw < cx;
        let bottom = ly + LINE_H < cy;
        let kx = if right { br[2] } else { br[0] };
        let (ky, ky2) = if bottom { (br[3], br[1]) } else { (br[1], br[3]) };
        let kx2 = if right { br[0] } else { br[2] };
        let mag = Color(128, 0, 128, 255);
        sc.push(Shape::Line { a: (cx, cy), b: (kx, ky), color: mag, width: 1.0, dash: false });
        sc.push(Shape::Line { a: (kx, ky), b: (kx, ky2), color: mag, width: 1.0, dash: false });
        sc.push(Shape::Line { a: (kx, ky), b: (kx2, ky), color: mag, width: 1.0, dash: false });
        if !w.is_point() {
            let start = if w.is_horizontal() { if right { 45 } else { 225 } } else if bottom { -45 } else { 135 };
            sc.push(Shape::Poly { pts: arc_points(cx - 4.0, cy - 4.0, 8.0, 8.0, start * 16, 270 * 16), color: mag, width: 1.0, fill: None, closed: false, dash: false });
        }
        sc.push(Shape::Text {
            pos: (lx, ly),
            text: w.label.clone(),
            size: FONT_PX,
            color: Color::BLACK,
            bold: false,
            angle: 0.0,
            bg: None,
        });
        sc.grow(lx, ly);
        sc.grow(lx + tw, ly + LINE_H);
    }

    // nodes: junction dots (>=3 connections), open pins (red circle)
    let deg = conn.degree_map(sch);
    for (&(x, y), &d) in &deg {
        if d >= 3 {
            sc.push(Shape::Ellipse { c: (x as f64, y as f64), rx: 4.0, ry: 4.0, color: ov.wire_color.unwrap_or(Color::WIRE), width: 0.0, fill: Some(ov.wire_color.unwrap_or(Color::WIRE)) });
        }
    }
    for p in &conn.pins {
        let d = deg.get(&(p.x, p.y)).copied().unwrap_or(0);
        if d <= 1 {
            sc.push(Shape::Ellipse { c: (p.x as f64, p.y as f64), rx: 4.0, ry: 4.0, color: Color::RED, width: 1.0, fill: None });
        }
    }

    // ---- overlays for agents
    if ov.pin_names {
        for p in &conn.pins {
            let name = p.label.rsplit_once('.').map(|x| x.1).unwrap_or("");
            let (tw, _) = crate::library::text_size(name, 9.0);
            let (dx, dy) = p.dir;
            let (tx, ty) = (p.x as f64 + dx as f64 * 6.0 - if dx < 0 { tw } else { 0.0 }, p.y as f64 + dy as f64 * 6.0 - if dy < 0 { 12.0 } else { 0.0 } - if dy == 0 { 12.0 } else { 0.0 });
            sc.push(Shape::Ellipse { c: (p.x as f64, p.y as f64), rx: 2.5, ry: 2.5, color: Color(200, 0, 160, 255), width: 0.0, fill: Some(Color(200, 0, 160, 255)) });
            sc.push(Shape::Text { pos: (tx, ty), text: name.to_string(), size: 9.0, color: Color(200, 0, 160, 255), bold: false, angle: 0.0, bg: Some(Color(255, 255, 255, 200)) });
        }
    }
    if ov.net_tags {
        for (ni, n) in conn.nets.iter().enumerate() {
            // tag on the longest wire of the net
            let best = n.wires.iter().max_by_key(|&&wi| sch.wires[wi].len());
            let color = if ov.color_nets { NET_PALETTE[ni % NET_PALETTE.len()] } else { Color(60, 60, 60, 255) };
            if let Some(&wi) = best {
                let w = &sch.wires[wi];
                let (mx, my) = ((w.x1 + w.x2) as f64 / 2.0, (w.y1 + w.y2) as f64 / 2.0);
                sc.push(Shape::Text { pos: (mx + 3.0, my + 2.0), text: n.name.clone(), size: 9.0, color, bold: true, angle: 0.0, bg: Some(Color(255, 255, 255, 210)) });
            }
        }
    }
    for (k, (x, y, _)) in ov.markers.iter().enumerate() {
        sc.push(Shape::Ellipse { c: (*x, *y), rx: 9.0, ry: 9.0, color: Color::RED, width: 2.0, fill: Some(Color(255, 255, 255, 220)) });
        let t = (k + 1).to_string();
        let (tw, _) = crate::library::text_size(&t, 10.0);
        sc.push(Shape::Text { pos: (x - tw / 2.0, y - 6.5), text: t, size: 10.0, color: Color::RED, bold: true, angle: 0.0, bg: None });
    }

    if sc.bounds[0] > sc.bounds[2] {
        sc.bounds = [0.0, 0.0, 400.0, 300.0];
    }
    sc
}

/// Qucs diagrams: `<Rect x y w h ...>` (x, y = lower-left corner) followed by
/// `<"dataset/vector" color ...>` graph lines and `</Rect>`.
fn diagram_shapes(lines: &[String], sc: &mut Scene) {
    let mut cur: Option<(String, f64, f64, f64, f64, Vec<(String, Color)>)> = None;
    let flush = |d: (String, f64, f64, f64, f64, Vec<(String, Color)>), sc: &mut Scene| {
        let (kind, x, y, w, h, graphs) = d;
        let top = y - h;
        let frame = Color(0x60, 0x60, 0x60, 255);
        let grid = Color(0xd0, 0xd0, 0xd0, 255);
        match kind.as_str() {
            "Smith" | "Polar" | "PS" | "SP" => {
                let r = w.min(h) / 2.0;
                let c = (x + w / 2.0, top + h / 2.0);
                sc.push(Shape::Ellipse { c, rx: r, ry: r, color: frame, width: 1.0, fill: Some(Color(255, 255, 255, 255)) });
                for k in [0.25, 0.5, 0.75] {
                    sc.push(Shape::Ellipse { c: (c.0 + r * k, c.1), rx: r * (1.0 - k), ry: r * (1.0 - k), color: grid, width: 1.0, fill: None });
                }
                sc.push(Shape::Line { a: (c.0 - r, c.1), b: (c.0 + r, c.1), color: grid, width: 1.0, dash: false });
            }
            _ => {
                sc.push(Shape::Poly { pts: vec![(x, top), (x + w, top), (x + w, y), (x, y)], color: frame, width: 1.0, fill: Some(Color(255, 255, 255, 255)), closed: true, dash: false });
                if kind != "Tab" {
                    for k in 1..10 {
                        let gx = x + w * k as f64 / 10.0;
                        sc.push(Shape::Line { a: (gx, top), b: (gx, y), color: grid, width: 1.0, dash: false });
                    }
                    for k in 1..5 {
                        let gy = top + h * k as f64 / 5.0;
                        sc.push(Shape::Line { a: (x, gy), b: (x + w, gy), color: grid, width: 1.0, dash: false });
                    }
                }
            }
        }
        for (k, (name, col)) in graphs.iter().enumerate() {
            sc.push(Shape::Text { pos: (x + 6.0, top + 4.0 + k as f64 * 16.0), text: name.clone(), size: 12.0, color: *col, bold: false, angle: 0.0, bg: Some(Color(255, 255, 255, 220)) });
        }
        sc.push(Shape::Text { pos: (x + 6.0, y - 18.0), text: "（シミュレーション結果は結果ウィンドウ / plot_results で表示）".into(), size: 10.0, color: Color(150, 150, 150, 255), bold: false, angle: 0.0, bg: None });
        sc.grow(x, top);
        sc.grow(x + w, y);
    };
    for raw in lines {
        let l = raw.trim();
        if l.starts_with("</") {
            if let Some(d) = cur.take() {
                flush(d, sc);
            }
            continue;
        }
        if let Some(rest) = l.strip_prefix("<\"") {
            if let Some(d) = cur.as_mut() {
                let name = rest.split('"').next().unwrap_or("").to_string();
                let col = l.split_whitespace().nth(1).map(Color::parse).unwrap_or(Color::WIRE);
                d.5.push((name, col));
            }
            continue;
        }
        let t = painting_tokens(l);
        if t.len() >= 5 {
            let f = |i: usize| t[i].parse::<f64>().unwrap_or(0.0);
            if let Some(d) = cur.take() {
                flush(d, sc);
            }
            cur = Some((t[0].clone(), f(1), f(2), f(3), f(4), Vec::new()));
        }
    }
    if let Some(d) = cur.take() {
        flush(d, sc);
    }
}

fn painting_shapes(line: &str, sc: &mut Scene) {
    let t = painting_tokens(line);
    if t.is_empty() {
        return;
    }
    let f = |i: usize| t.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let s = |i: usize| t.get(i).cloned().unwrap_or_default();
    match t[0].as_str() {
        "Line" => sc.push(Shape::Line {
            a: (f(1), f(2)),
            b: (f(1) + f(3), f(2) + f(4)),
            color: Color::parse(&s(5)),
            width: pen_width(f(6)),
            dash: f(7) as i32 > 1,
        }),
        "Rectangle" | "Ellipse" => {
            let (x, y, w, h) = (f(1), f(2), f(3), f(4));
            let fill = if f(10) as i32 == 1 { Some(Color::parse(&s(8))) } else { None };
            if t[0] == "Rectangle" {
                sc.push(Shape::Poly {
                    pts: vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
                    color: Color::parse(&s(5)),
                    width: pen_width(f(6)),
                    fill,
                    closed: true,
                    dash: f(7) as i32 > 1,
                });
            } else {
                sc.push(Shape::Ellipse { c: (x + w / 2.0, y + h / 2.0), rx: w / 2.0, ry: h / 2.0, color: Color::parse(&s(5)), width: pen_width(f(6)), fill });
            }
        }
        "EArc" => sc.push(Shape::Poly {
            pts: arc_points(f(1), f(2), f(3), f(4), f(5) as i32, f(6) as i32),
            color: Color::parse(&s(7)),
            width: pen_width(f(8)),
            fill: None,
            closed: false,
            dash: false,
        }),
        "Arrow" => {
            let (x, y, dx, dy) = (f(1), f(2), f(3), f(4));
            let col = Color::parse(&s(7));
            sc.push(Shape::Line { a: (x, y), b: (x + dx, y + dy), color: col, width: pen_width(f(8)), dash: false });
            let len = (dx * dx + dy * dy).sqrt().max(1e-9);
            let (ux, uy) = (dx / len, dy / len);
            let (hh, hw) = (f(5), f(6) / 2.0);
            let tip = (x + dx, y + dy);
            let base = (tip.0 - ux * hh, tip.1 - uy * hh);
            sc.push(Shape::Poly {
                pts: vec![tip, (base.0 - uy * hw, base.1 + ux * hw), (base.0 + uy * hw, base.1 - ux * hw)],
                color: col,
                width: 1.0,
                fill: Some(col),
                closed: true,
                dash: false,
            });
        }
        "Text" => {
            let text = crate::schematic::unquote(&s(6)).replace("\\n", "\n");
            let size = f(3).max(6.0);
            for (k, l) in text.split('\n').enumerate() {
                sc.push(Shape::Text {
                    pos: (f(1), f(2) + k as f64 * size * 1.25),
                    text: l.to_string(),
                    size,
                    color: Color::parse(&s(4)),
                    bold: false,
                    angle: -f(5),
                    bg: None,
                });
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// SVG / PNG output
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct View {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub scale: f64, // pixels per schematic unit
}

impl View {
    pub fn width_px(&self) -> u32 {
        ((self.x1 - self.x0) * self.scale).round().max(1.0) as u32
    }
    pub fn height_px(&self) -> u32 {
        ((self.y1 - self.y0) * self.scale).round().max(1.0) as u32
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[derive(Clone, Debug, Default)]
pub struct Decor {
    pub grid: bool,
    pub rulers: bool,
    pub title: Option<String>,
}

pub const RULER: f64 = 26.0; // px margin used by rulers

/// SVG document of a scene. Coordinates in the SVG are pixels; the schematic
/// point (x, y) maps to ((x - x0) * scale + ox, (y - y0) * scale + oy).
pub fn scene_svg(sc: &Scene, v: &View, d: &Decor) -> (String, (f64, f64)) {
    let (ox, oy) = if d.rulers { (RULER, RULER) } else { (0.0, 0.0) };
    let w = v.width_px() as f64 + ox;
    let h = v.height_px() as f64 + oy;
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="Segoe UI, Arial, DejaVu Sans, sans-serif">"#
    );
    let _ = write!(s, r##"<rect x="0" y="0" width="{w}" height="{h}" fill="#ffffff"/>"##);
    let px = |x: f64| (x - v.x0) * v.scale + ox;
    let py = |y: f64| (y - v.y0) * v.scale + oy;
    if d.grid {
        let step = 10.0;
        let gx0 = (v.x0 / step).floor() as i64;
        let gx1 = (v.x1 / step).ceil() as i64;
        let gy0 = (v.y0 / step).floor() as i64;
        let gy1 = (v.y1 / step).ceil() as i64;
        s.push_str(r##"<g stroke-width="1">"##);
        for gx in gx0..=gx1 {
            let x = gx as f64 * step;
            let (col, op) = if gx % 10 == 0 { ("#9db4d0", 0.9) } else if gx % 5 == 0 { ("#c3d2e4", 0.8) } else { ("#e6edf5", 0.8) };
            let _ = write!(s, r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{col}" stroke-opacity="{op}"/>"#, px(x), py(v.y0), px(x), py(v.y1));
        }
        for gy in gy0..=gy1 {
            let y = gy as f64 * step;
            let (col, op) = if gy % 10 == 0 { ("#9db4d0", 0.9) } else if gy % 5 == 0 { ("#c3d2e4", 0.8) } else { ("#e6edf5", 0.8) };
            let _ = write!(s, r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{col}" stroke-opacity="{op}"/>"#, px(v.x0), py(y), px(v.x1), py(y));
        }
        s.push_str("</g>");
    }
    for sh in &sc.shapes {
        match sh {
            Shape::Line { a, b, color, width, dash } => {
                let _ = write!(
                    s,
                    r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-opacity="{:.3}" stroke-width="{:.2}" stroke-linecap="round"{}/>"#,
                    px(a.0),
                    py(a.1),
                    px(b.0),
                    py(b.1),
                    color.hex(),
                    color.alpha(),
                    width * v.scale,
                    if *dash { format!(r#" stroke-dasharray="{:.1},{:.1}""#, 4.0 * v.scale, 3.0 * v.scale) } else { String::new() }
                );
            }
            Shape::Poly { pts, color, width, fill, closed, dash } => {
                let tag = if *closed { "polygon" } else { "polyline" };
                let p: Vec<String> = pts.iter().map(|q| format!("{:.2},{:.2}", px(q.0), py(q.1))).collect();
                let fill_s = match fill {
                    Some(f) => format!(r#"fill="{}" fill-opacity="{:.3}""#, f.hex(), f.alpha()),
                    None => r#"fill="none""#.to_string(),
                };
                let stroke = if *width > 0.0 {
                    format!(
                        r#"stroke="{}" stroke-opacity="{:.3}" stroke-width="{:.2}" stroke-linejoin="round" stroke-linecap="round"{}"#,
                        color.hex(),
                        color.alpha(),
                        width * v.scale,
                        if *dash { format!(r#" stroke-dasharray="{:.1},{:.1}""#, 4.0 * v.scale, 3.0 * v.scale) } else { String::new() }
                    )
                } else {
                    r#"stroke="none""#.to_string()
                };
                let _ = write!(s, r#"<{tag} points="{}" {fill_s} {stroke}/>"#, p.join(" "));
            }
            Shape::Ellipse { c, rx, ry, color, width, fill } => {
                let fill_s = match fill {
                    Some(f) => format!(r#"fill="{}" fill-opacity="{:.3}""#, f.hex(), f.alpha()),
                    None => r#"fill="none""#.to_string(),
                };
                let stroke = if *width > 0.0 {
                    format!(r#"stroke="{}" stroke-width="{:.2}""#, color.hex(), width * v.scale)
                } else {
                    r#"stroke="none""#.into()
                };
                let _ = write!(s, r#"<ellipse cx="{:.2}" cy="{:.2}" rx="{:.2}" ry="{:.2}" {fill_s} {stroke}/>"#, px(c.0), py(c.1), rx * v.scale, ry * v.scale);
            }
            Shape::Text { pos, text, size, color, bold, angle, bg } => {
                let fs = size * v.scale;
                let (x, y) = (px(pos.0), py(pos.1));
                if let Some(b) = bg {
                    let (tw, th) = crate::library::text_size(text, *size);
                    let _ = write!(
                        s,
                        r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{}" fill-opacity="{:.2}" rx="{:.1}"/>"#,
                        x - 1.5 * v.scale,
                        y,
                        (tw + 3.0) * v.scale,
                        th * 0.9 * v.scale,
                        b.hex(),
                        b.alpha(),
                        2.0 * v.scale
                    );
                }
                let rot = if angle.abs() > 0.01 { format!(r#" transform="rotate({angle:.1} {x:.2} {y:.2})""#) } else { String::new() };
                let _ = write!(
                    s,
                    r#"<text x="{:.2}" y="{:.2}" font-size="{:.2}" fill="{}"{}{}>{}</text>"#,
                    x,
                    y + fs * 0.92,
                    fs,
                    color.hex(),
                    if *bold { r#" font-weight="600""# } else { "" },
                    rot,
                    esc(text)
                );
            }
        }
    }
    if d.rulers {
        // coordinate rulers (schematic units) on top and left edges
        let _ = write!(s, r##"<rect x="0" y="0" width="{w}" height="{RULER}" fill="#f4f6f9"/><rect x="0" y="0" width="{RULER}" height="{h}" fill="#f4f6f9"/>"##);
        let span = (v.x1 - v.x0).max(v.y1 - v.y0);
        let step = if span * v.scale / 50.0 > 40.0 { 100.0 } else { 50.0 };
        let step = if v.scale * step < 45.0 { step * 2.0 } else { step };
        let mut x = (v.x0 / step).ceil() * step;
        while x <= v.x1 {
            let _ = write!(s, r##"<line x1="{0:.1}" y1="{1}" x2="{0:.1}" y2="{2}" stroke="#555"/><text x="{0:.1}" y="12" font-size="10" fill="#333" text-anchor="middle">{3}</text>"##, px(x), RULER - 6.0, RULER, x as i64);
            x += step;
        }
        let mut y = (v.y0 / step).ceil() * step;
        while y <= v.y1 {
            let _ = write!(s, r##"<line x1="{1}" y1="{0:.1}" x2="{2}" y2="{0:.1}" stroke="#555"/><text x="13" y="{0:.1}" font-size="10" fill="#333" text-anchor="middle" transform="rotate(-90 13 {0:.1})">{3}</text>"##, py(y), RULER - 6.0, RULER, y as i64);
            y += step;
        }
        let _ = write!(s, r##"<text x="2" y="11" font-size="8" fill="#888">x→</text><text x="2" y="22" font-size="8" fill="#888">y↓</text>"##);
    }
    if let Some(t) = &d.title {
        let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="12" fill="#666">{}</text>"##, ox + 6.0, oy + 16.0, esc(t));
    }
    s.push_str("</svg>");
    (s, (ox, oy))
}

static FONTDB: std::sync::OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> = std::sync::OnceLock::new();

pub fn fontdb() -> std::sync::Arc<resvg::usvg::fontdb::Database> {
    FONTDB
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            db.set_sans_serif_family("Segoe UI");
            std::sync::Arc::new(db)
        })
        .clone()
}

pub fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb = fontdb();
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|e| format!("svg parse: {e}"))?;
    let size = tree.size().to_int_size();
    let mut pm = resvg::tiny_skia::Pixmap::new(size.width(), size.height()).ok_or("pixmap alloc failed")?;
    pm.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(&tree, resvg::tiny_skia::Transform::default(), &mut pm.as_mut());
    pm.encode_png().map_err(|e| e.to_string())
}

/// Rasterise an SVG to premultiplied RGBA (for GUI textures).
pub fn svg_to_rgba(svg: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb = fontdb();
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|e| format!("svg parse: {e}"))?;
    let size = tree.size().to_int_size();
    let mut pm = resvg::tiny_skia::Pixmap::new(size.width(), size.height()).ok_or("pixmap alloc failed")?;
    pm.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(&tree, resvg::tiny_skia::Transform::default(), &mut pm.as_mut());
    Ok((size.width(), size.height(), pm.take()))
}

/// View fitting `bounds` (+ margin) into at most `max_px` pixels.
pub fn fit_view(bounds: [f64; 4], margin: f64, scale: Option<f64>, max_px: f64) -> View {
    let x0 = ((bounds[0] - margin) / 10.0).floor() * 10.0;
    let y0 = ((bounds[1] - margin) / 10.0).floor() * 10.0;
    let x1 = ((bounds[2] + margin) / 10.0).ceil() * 10.0;
    let y1 = ((bounds[3] + margin) / 10.0).ceil() * 10.0;
    let span = (x1 - x0).max(y1 - y0).max(1.0);
    let s = scale.unwrap_or_else(|| (max_px / span).clamp(0.3, 3.0)).min(max_px / span);
    View { x0, y0, x1, y1, scale: s }
}
