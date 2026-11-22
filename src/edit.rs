//! Editing helpers beyond placement/wiring: clipboard (Qucs format), duplicate,
//! align / distribute, mirror-Y, paintings (lines, boxes, text ...), export.

use crate::doc::{snap, unique_name, Doc};
use crate::library::{text_size, LINE_H};
use crate::schematic::{painting_tokens, quote, Component, Schematic, Wire};
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
            order.sort_by(|&a, &b| center(&bb[a]).total_cmp(&center(&bb[b])));
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

/// Drag wire `wi` of `wires` by (dx, dy) and return the new wire list.
///
/// A horizontal wire only moves up/down and a vertical one only left/right
/// (a point or diagonal wire moves freely). At each end, wires meeting it at a
/// right angle stretch along; an end held by a pin, a collinear wire or a
/// non-following wire stays put and a stub wire keeps it connected. A stub that
/// runs along an existing wire of the same junction is merged into it, so no
/// overlapping or mid-wire connections are left behind.
pub fn drag_wire(wires: &[Wire], pins: &HashSet<(i32, i32)>, wi: usize, dx: i32, dy: i32) -> Vec<Wire> {
    let mut out = wires.to_vec();
    let Some(w) = wires.get(wi) else { return out };
    let (dx, dy) = if w.is_point() {
        (dx, dy)
    } else if w.is_horizontal() {
        (0, dy)
    } else if w.is_vertical() {
        (dx, 0)
    } else {
        (dx, dy)
    };
    if (dx, dy) == (0, 0) {
        return out;
    }
    let mv = |p: (i32, i32)| (p.0 + dx, p.1 + dy);
    // a neighbour can follow when moving its end keeps it horizontal/vertical
    let follows = |n: &Wire| n.is_point() || (n.is_vertical() && dx == 0) || (n.is_horizontal() && dy == 0);
    let ends: Vec<(i32, i32)> = if w.is_point() { vec![w.p1()] } else { vec![w.p1(), w.p2()] };
    let mut stubs: Vec<((i32, i32), (i32, i32))> = Vec::new();
    for e in ends {
        let nbrs: Vec<usize> = (0..wires.len()).filter(|&j| j != wi && (wires[j].p1() == e || wires[j].p2() == e)).collect();
        let mid_of_other = wires.iter().enumerate().any(|(j, o)| j != wi && o.p1() != e && o.p2() != e && on_segment(o, e));
        let held = pins.contains(&e) || mid_of_other || nbrs.iter().any(|&j| !follows(&wires[j]));
        if held {
            stubs.push((e, mv(e)));
            continue;
        }
        for j in nbrs {
            let n = &wires[j];
            let (a, b) = (if n.p1() == e { mv(n.p1()) } else { n.p1() }, if n.p2() == e { mv(n.p2()) } else { n.p2() });
            out[j] = rebuilt(n, a, b, if n.is_point() { (dx, dy) } else { (0, 0) });
        }
    }
    out[wi] = rebuilt(w, mv(w.p1()), mv(w.p2()), (dx, dy));
    for (a, b) in stubs {
        // a stub along an unlabelled wire that leaves the same junction: replace
        // both by the segments between their end points (removes the overlap)
        let along = (0..out.len()).find(|&j| {
            let o = &out[j];
            j != wi && o.label.is_empty() && !o.is_point() && (o.p1() == a || o.p2() == a) && collinear(o, a, b) && {
                let far = if o.p1() == a { o.p2() } else { o.p1() };
                (far.0 - a.0).signum() == (b.0 - a.0).signum() && (far.1 - a.1).signum() == (b.1 - a.1).signum()
            }
        });
        match along {
            Some(j) => {
                let far = if out[j].p1() == a { out[j].p2() } else { out[j].p1() };
                let (near, farthest) = if (far.0 - a.0).abs() + (far.1 - a.1).abs() < (b.0 - a.0).abs() + (b.1 - a.1).abs() { (far, b) } else { (b, far) };
                out[j] = Wire::new(a.0, a.1, near.0, near.1);
                out.push(Wire::new(near.0, near.1, farthest.0, farthest.1));
            }
            None => out.push(Wire::new(a.0, a.1, b.0, b.1)),
        }
    }
    out
}

/// Move a selection (parts, wires, paintings) of `sch` by (dx, dy) as one block.
///
/// `pins` are the component pins `(component, x, y)` before the move. Unselected
/// wires attached to a moved point (a pin of a moved part, or any point of a moved
/// wire) follow: with both ends attached they move along, with one end they
/// stretch, or turn into an L that keeps the direction at the fixed end when a
/// plain stretch would make them diagonal. Wires are not merged here
/// (`doc::normalize_wires` does that after the drag).
pub fn move_group(sch: &mut Schematic, pins: &[(usize, i32, i32)], comps: &[usize], wires: &[usize], paints: &[usize], dx: i32, dy: i32) {
    if (dx, dy) == (0, 0) {
        return;
    }
    let comp_set: HashSet<usize> = comps.iter().copied().collect();
    let wire_set: HashSet<usize> = wires.iter().copied().collect();
    let moved_pins: HashSet<(i32, i32)> = pins.iter().filter(|p| comp_set.contains(&p.0)).map(|p| (p.1, p.2)).collect();
    let fixed_pins: HashSet<(i32, i32)> = pins.iter().filter(|p| !comp_set.contains(&p.0)).map(|p| (p.1, p.2)).collect();
    let orig = sch.wires.clone();
    let on_moved_wire = |p: (i32, i32)| wires.iter().filter_map(|&i| orig.get(i)).any(|w| on_segment(w, p) || w.p1() == p || w.p2() == p);
    // a point follows the block unless a part that stays put holds it
    let moves = |p: (i32, i32)| (moved_pins.contains(&p) || on_moved_wire(p)) && !fixed_pins.contains(&p);
    let mv = |p: (i32, i32)| (p.0 + dx, p.1 + dy);
    for &i in comps {
        if let Some(c) = sch.components.get_mut(i) {
            c.x += dx;
            c.y += dy;
        }
    }
    for &i in paints {
        if let Some(p) = sch.paintings.get_mut(i) {
            *p = translate_painting(p, dx, dy);
        }
    }
    let mut extra = Vec::new();
    for (i, w) in orig.iter().enumerate() {
        if wire_set.contains(&i) {
            sch.wires[i] = rebuilt(w, mv(w.p1()), mv(w.p2()), (dx, dy));
            continue;
        }
        let (ma, mb) = (moves(w.p1()), moves(w.p2()));
        if w.is_point() || (ma && mb) {
            if ma || mb {
                sch.wires[i] = rebuilt(w, mv(w.p1()), mv(w.p2()), (dx, dy));
            }
            continue;
        }
        if !ma && !mb {
            continue;
        }
        let (f, e) = if ma { (w.p2(), w.p1()) } else { (w.p1(), w.p2()) };
        let ne = mv(e);
        let corner = if w.is_horizontal() && ne.1 != f.1 {
            Some((ne.0, f.1))
        } else if w.is_vertical() && ne.0 != f.0 {
            Some((f.0, ne.1))
        } else {
            None
        };
        match corner {
            None => sch.wires[i] = rebuilt(w, f, ne, (0, 0)),
            Some(c) => {
                sch.wires[i] = rebuilt(w, f, c, (0, 0));
                extra.push(Wire::new(c.0, c.1, ne.0, ne.1));
            }
        }
    }
    sch.wires.extend(extra);
}

/// `w` with new end points; its label moves by `shift` and keeps its anchor.
fn rebuilt(w: &Wire, a: (i32, i32), b: (i32, i32), shift: (i32, i32)) -> Wire {
    let mut n = Wire::new(a.0, a.1, b.0, b.1);
    n.extra = w.extra.clone();
    if !w.label.is_empty() {
        let (ax, ay) = w.label_anchor();
        n.set_label(&w.label, w.lx + shift.0, w.ly + shift.1, (ax + shift.0, ay + shift.1));
    }
    n
}

/// Point strictly inside or at the ends of an axis-parallel wire.
fn on_segment(w: &Wire, p: (i32, i32)) -> bool {
    (w.is_horizontal() && p.1 == w.y1 && p.0 >= w.x1.min(w.x2) && p.0 <= w.x1.max(w.x2))
        || (w.is_vertical() && p.0 == w.x1 && p.1 >= w.y1.min(w.y2) && p.1 <= w.y1.max(w.y2))
}

fn collinear(w: &Wire, a: (i32, i32), b: (i32, i32)) -> bool {
    (w.is_horizontal() && a.1 == b.1 && a.1 == w.y1) || (w.is_vertical() && a.0 == b.0 && a.0 == w.x1)
}

/// Which component text block is under the point.
pub fn comp_label_at(doc: &Doc, x: f64, y: f64) -> Option<usize> {
    let conn = doc.conn();
    conn.placed.iter().enumerate().find_map(|(i, p)| {
        let b = p.label_bbox?;
        (x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]).then_some(i)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(ws: &[Wire]) -> Vec<(i32, i32, i32, i32)> {
        let mut v: Vec<_> = ws.iter().filter(|w| !w.is_point()).map(|w| (w.x1, w.y1, w.x2, w.y2)).collect();
        v.sort();
        v
    }

    #[test]
    fn drag_wire_stretches_perpendicular_neighbours() {
        // U shape: two verticals joined by a horizontal at the bottom
        let ws = vec![Wire::new(0, 0, 0, 100), Wire::new(0, 100, 100, 100), Wire::new(100, 0, 100, 100)];
        let out = drag_wire(&ws, &HashSet::new(), 1, 7, 40);
        assert_eq!(segs(&out), vec![(0, 0, 0, 140), (0, 140, 100, 140), (100, 0, 100, 140)]);
    }

    #[test]
    fn drag_wire_adds_stub_at_pin() {
        // horizontal wire between two pins
        let ws = vec![Wire::new(0, 0, 100, 0)];
        let pins: HashSet<_> = [(0, 0), (100, 0)].into_iter().collect();
        let out = drag_wire(&ws, &pins, 0, 0, 30);
        assert_eq!(segs(&out), vec![(0, 0, 0, 30), (0, 30, 100, 30), (100, 0, 100, 30)]);
    }

    #[test]
    fn drag_wire_merges_stub_into_wire_along_the_move() {
        // horizontal ends on a pin that also has a wire going down: the stub
        // would overlap it, so that wire is split at the new corner instead
        let ws = vec![Wire::new(0, 0, 100, 0), Wire::new(0, 0, 0, 100)];
        let pins: HashSet<_> = [(0, 0)].into_iter().collect();
        let out = drag_wire(&ws, &pins, 0, 0, 30);
        assert_eq!(segs(&out), vec![(0, 0, 0, 30), (0, 30, 0, 100), (0, 30, 100, 30)]);
    }

    #[test]
    fn drag_wire_keeps_collinear_neighbour_connected() {
        let ws = vec![Wire::new(0, 0, 100, 0), Wire::new(100, 0, 200, 0)];
        let out = drag_wire(&ws, &HashSet::new(), 1, 0, -20);
        assert_eq!(segs(&out), vec![(0, 0, 100, 0), (100, -20, 100, 0), (100, -20, 200, -20)]);
    }

    #[test]
    fn drag_wire_moves_label_with_it() {
        let mut w = Wire::new(0, 0, 100, 0);
        w.set_label("n1", 40, -20, (50, 0));
        let out = drag_wire(&[w], &HashSet::new(), 0, 0, 30);
        assert_eq!((out[0].lx, out[0].ly, out[0].label_anchor()), (40, 10, (50, 30)));
    }

    fn sch_with(comps: &[(i32, i32)], wires: Vec<Wire>) -> Schematic {
        let mut s = Schematic::default();
        for (k, &(x, y)) in comps.iter().enumerate() {
            s.components.push(Component::parse(&format!("R R{k} 1 {x} {y} 0 0 0 1 \"1k\" 1")).unwrap());
        }
        s.wires = wires;
        s
    }

    #[test]
    fn move_group_moves_block_and_stretches_attached_wires() {
        // part 0 (pins at 0,0 and 0,60) with a wire to part 1 (pin at 200,0);
        // part 0 and a wire hanging below it are selected and moved right by 50
        let mut s = sch_with(&[(0, 30), (200, 30)], vec![Wire::new(0, 0, 200, 0), Wire::new(0, 60, 0, 100)]);
        let pins = [(0, 0, 0), (0, 0, 60), (1, 200, 0)];
        move_group(&mut s, &pins, &[0], &[1], &[], 50, 0);
        assert_eq!((s.components[0].x, s.components[1].x), (50, 200));
        assert_eq!(segs(&s.wires), vec![(50, 0, 200, 0), (50, 60, 50, 100)]);
    }

    #[test]
    fn move_group_bends_wires_that_would_go_diagonal() {
        let mut s = sch_with(&[(0, 30), (200, 30)], vec![Wire::new(0, 0, 200, 0)]);
        let pins = [(0, 0, 0), (1, 200, 0)];
        move_group(&mut s, &pins, &[0], &[], &[], 0, 40);
        // the wire stays where it is and a jog reaches the moved pin
        assert_eq!(segs(&s.wires), vec![(0, 0, 0, 40), (0, 0, 200, 0)]);
    }

    #[test]
    fn move_group_keeps_pins_of_parts_left_behind() {
        // a wire ends on a pin of a part that is not selected: that end stays
        let mut s = sch_with(&[(0, 30), (200, 30)], vec![Wire::new(0, 0, 200, 0)]);
        let pins = [(0, 0, 0), (1, 200, 0)];
        move_group(&mut s, &pins, &[], &[0], &[], 0, -20);
        assert_eq!(segs(&s.wires), vec![(0, -20, 200, -20)]);
    }
}
