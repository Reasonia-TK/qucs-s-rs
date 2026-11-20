//! Editing helpers beyond placement/wiring: clipboard (Qucs format), duplicate,
//! align / distribute, mirror-Y, paintings (lines, boxes, text ...), export.

use crate::doc::{snap, unique_name, Doc};
use crate::library::{text_size, LINE_H};
use crate::schematic::{painting_tokens, quote, Component, Schematic};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// clipboard / duplicate
// ---------------------------------------------------------------------------

/// Components + the wires that lie completely between them, in Qucs clipboard format.
pub fn copy_text(doc: &Doc, comps: &[usize], wires: &[usize], paintings: &[usize]) -> String {
    let mut s = Schematic::new_empty("clip");
    s.properties.clear();
    for &i in comps {
        if let Some(c) = doc.sch.components.get(i) {
            s.components.push(c.clone());
        }
    }
    let mut ws: HashSet<usize> = wires.iter().copied().collect();
    // wire groups whose pins all belong to the selected components
    if !comps.is_empty() {
        let conn = doc.conn();
        for isl in &conn.islands {
            if !isl.pins.is_empty() && !isl.wires.is_empty() && isl.pins.iter().all(|&p| comps.contains(&conn.pins[p].comp)) {
                ws.extend(isl.wires.iter().copied());
            }
        }
    }
    let mut ws: Vec<usize> = ws.into_iter().collect();
    ws.sort_unstable();
    for k in ws {
        s.wires.push(doc.sch.wires[k].clone());
    }
    for &k in paintings {
        if let Some(p) = doc.sch.paintings.get(k) {
            s.paintings.push(p.clone());
        }
    }
    s.serialize()
}

/// Paste Qucs clipboard text, offset by (dx, dy); components are renamed when names clash.
/// Returns indices of the new components, wires and paintings.
pub fn paste_text(doc: &mut Doc, text: &str, dx: i32, dy: i32) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>), String> {
    let t = text.trim();
    let clip = if t.starts_with("<Qucs Schematic") {
        Schematic::parse(t)?
    } else {
        // bare component/wire lines
        Schematic::parse(&format!("<Qucs Schematic 0>\n<Components>\n{}\n</Components>\n", t)).map_err(|_| "clipboard does not contain Qucs schematic data".to_string())?
    };
    let mut used: HashSet<String> = doc.sch.components.iter().map(|c| c.name.clone()).collect();
    let mut rename: HashMap<String, String> = HashMap::new();
    let c0 = doc.sch.components.len();
    for mut c in clip.components {
        if used.contains(&c.name) || c.name == "*" {
            let prefix = c.name.trim_end_matches(|ch: char| ch.is_ascii_digit()).to_string();
            let prefix = if prefix.is_empty() || prefix == "*" { crate::library::name_prefix(&c.typ) } else { prefix };
            let n = unique_name(&prefix, &used);
            rename.insert(c.name.clone(), n.clone());
            c.name = n;
        }
        used.insert(c.name.clone());
        c.x += dx;
        c.y += dy;
        doc.sch.components.push(c);
    }
    let w0 = doc.sch.wires.len();
    for mut w in clip.wires {
        w.x1 += dx;
        w.x2 += dx;
        w.y1 += dy;
        w.y2 += dy;
        if !w.label.is_empty() {
            w.lx += dx;
            w.ly += dy;
        }
        doc.sch.wires.push(w);
    }
    let p0 = doc.sch.paintings.len();
    for p in clip.paintings {
        doc.sch.paintings.push(translate_painting(&p, dx, dy));
    }
    Ok(((c0..doc.sch.components.len()).collect(), (w0..doc.sch.wires.len()).collect(), (p0..doc.sch.paintings.len()).collect()))
}

pub fn duplicate(doc: &mut Doc, comps: &[usize], wires: &[usize], paintings: &[usize], dx: i32, dy: i32) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>), String> {
    let t = copy_text(doc, comps, wires, paintings);
    paste_text(doc, &t, snap(dx), snap(dy))
}

// ---------------------------------------------------------------------------
// align / distribute / mirror
// ---------------------------------------------------------------------------

/// mode: left | right | top | bottom | hcenter | vcenter | distribute_h | distribute_v
/// Alignment uses the symbol bodies (like Qucs). Returns the moves (name, x, y).
pub fn align(doc: &Doc, comps: &[usize], mode: &str) -> Result<Vec<(String, i32, i32)>, String> {
    if comps.len() < 2 {
        return Err("select at least two components".into());
    }
    let conn = doc.conn();
    let bb: Vec<[f64; 4]> = comps.iter().map(|&i| conn.placed[i].bbox).collect();
    let mut moves = Vec::new();
    let mv = |i: usize, dx: f64, dy: f64, moves: &mut Vec<(String, i32, i32)>| {
        let c = &doc.sch.components[i];
        moves.push((c.name.clone(), snap(c.x + dx.round() as i32), snap(c.y + dy.round() as i32)));
    };
    match mode {
        "left" | "right" | "top" | "bottom" | "hcenter" | "vcenter" => {
            let target = match mode {
                "left" => bb.iter().map(|b| b[0]).fold(f64::MAX, f64::min),
                "right" => bb.iter().map(|b| b[2]).fold(f64::MIN, f64::max),
                "top" => bb.iter().map(|b| b[1]).fold(f64::MAX, f64::min),
                "bottom" => bb.iter().map(|b| b[3]).fold(f64::MIN, f64::max),
                // centre lines: align on the first component's centre (as Qucs does)
                "hcenter" => (bb[0][1] + bb[0][3]) / 2.0,
                _ => (bb[0][0] + bb[0][2]) / 2.0,
            };
            for (k, &i) in comps.iter().enumerate() {
                let b = bb[k];
                let (dx, dy) = match mode {
                    "left" => (target - b[0], 0.0),
                    "right" => (target - b[2], 0.0),
                    "top" => (0.0, target - b[1]),
                    "bottom" => (0.0, target - b[3]),
                    "hcenter" => (0.0, target - (b[1] + b[3]) / 2.0),
                    _ => (target - (b[0] + b[2]) / 2.0, 0.0),
                };
                mv(i, dx, dy, &mut moves);
            }
        }
        "distribute_h" | "distribute_v" => {
            let h = mode == "distribute_h";
            let mut order: Vec<usize> = (0..comps.len()).collect();
            let center = |b: &[f64; 4]| if h { (b[0] + b[2]) / 2.0 } else { (b[1] + b[3]) / 2.0 };
            order.sort_by(|&a, &b| center(&bb[a]).partial_cmp(&center(&bb[b])).unwrap());
            let first = center(&bb[order[0]]);
            let last = center(&bb[*order.last().unwrap()]);
            let step = (last - first) / (order.len() - 1) as f64;
            for (k, &o) in order.iter().enumerate() {
                let want = first + step * k as f64;
                let d = want - center(&bb[o]);
                if h {
                    mv(comps[o], d, 0.0, &mut moves);
                } else {
                    mv(comps[o], 0.0, d, &mut moves);
                }
            }
        }
        other => return Err(format!("unknown align mode '{other}' (left, right, top, bottom, hcenter, vcenter, distribute_h, distribute_v)")),
    }
    Ok(moves)
}

/// Mirror about the Y axis (Qucs: mirror X + rotate 180°).
pub fn mirror_y(c: &mut Component) {
    c.mirror = 1 - c.mirror.clamp(0, 1);
    c.rotate = (c.rotate + 2) % 4;
}

// ---------------------------------------------------------------------------
// paintings
// ---------------------------------------------------------------------------

/// Create a painting line. kind: line | rect | ellipse | arrow | text.
pub fn make_painting(kind: &str, x: i32, y: i32, w: i32, h: i32, color: &str, width: i32, text: &str, size: i32, filled: bool) -> Result<String, String> {
    let col = if color.is_empty() { "#000000" } else { color };
    Ok(match kind {
        "line" => format!("<Line {x} {y} {w} {h} {col} {width} 1>"),
        "rect" | "rectangle" => format!("<Rectangle {x} {y} {w} {h} {col} {width} 1 {col} 1 {}>", if filled { 1 } else { 0 }),
        "ellipse" => format!("<Ellipse {x} {y} {w} {h} {col} {width} 1 {col} 1 {}>", if filled { 1 } else { 0 }),
        "arrow" => format!("<Arrow {x} {y} {w} {h} 20 8 {col} {width} 1 1>"),
        "text" => {
            if text.is_empty() {
                return Err("text painting needs text".into());
            }
            format!("<Text {x} {y} {} {col} 0 {}>", size.max(6), quote(&text.replace('\n', "\\n")))
        }
        other => return Err(format!("unknown painting kind '{other}' (line, rect, ellipse, arrow, text)")),
    })
}

/// Bounding box of a painting.
pub fn painting_bbox(line: &str) -> Option<[f64; 4]> {
    let t = painting_tokens(line);
    let f = |i: usize| t.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    match t.first()?.as_str() {
        "Line" | "Arrow" => {
            let (x, y, dx, dy) = (f(1), f(2), f(3), f(4));
            Some([x.min(x + dx) - 3.0, y.min(y + dy) - 3.0, x.max(x + dx) + 3.0, y.max(y + dy) + 3.0])
        }
        "Rectangle" | "Ellipse" | "EArc" => Some([f(1), f(2), f(1) + f(3), f(2) + f(4)]),
        "Text" => {
            let s = crate::schematic::unquote(t.get(6).map(|s| s.as_str()).unwrap_or("")).replace("\\n", "\n");
            let (w, h) = text_size(&s, f(3).max(6.0));
            Some([f(1), f(2), f(1) + w, f(2) + h.max(LINE_H)])
        }
        _ => None,
    }
}

pub fn translate_painting(line: &str, dx: i32, dy: i32) -> String {
    let mut t = painting_tokens(line);
    if t.len() < 3 {
        return line.to_string();
    }
    for (k, d) in [(1usize, dx), (2usize, dy)] {
        if let Ok(v) = t[k].parse::<f64>() {
            t[k] = ((v.round() as i32) + d).to_string();
        }
    }
    format!("  <{}>", t.join(" "))
}

/// Replace the text of a Text painting.
pub fn set_painting_text(line: &str, text: &str) -> String {
    let mut t = painting_tokens(line);
    if t.first().map(|s| s == "Text").unwrap_or(false) && t.len() >= 7 {
        t[6] = quote(&text.replace('\n', "\\n"));
        return format!("  <{}>", t.join(" "));
    }
    line.to_string()
}

pub fn painting_at(doc: &Doc, x: f64, y: f64) -> Option<usize> {
    doc.sch.paintings.iter().enumerate().rev().find_map(|(i, p)| {
        let b = painting_bbox(p)?;
        (x >= b[0] - 2.0 && x <= b[2] + 2.0 && y >= b[1] - 2.0 && y <= b[3] + 2.0).then_some(i)
    })
}

/// Which wire label (index of its wire) is under the point.
pub fn wire_label_at(doc: &Doc, x: f64, y: f64) -> Option<usize> {
    doc.sch.wires.iter().enumerate().find_map(|(i, w)| {
        if w.label.is_empty() {
            return None;
        }
        let tw = crate::library::text_width(&w.label, crate::library::FONT_PX);
        let (lx, ly) = (w.lx as f64, w.ly as f64);
        (x >= lx - 3.0 && x <= lx + tw + 3.0 && y >= ly - 3.0 && y <= ly + LINE_H + 3.0).then_some(i)
    })
}

/// Which component text block is under the point.
pub fn comp_label_at(doc: &Doc, x: f64, y: f64) -> Option<usize> {
    let conn = doc.conn();
    conn.placed.iter().enumerate().find_map(|(i, p)| {
        let b = p.label_bbox?;
        (x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]).then_some(i)
    })
}
