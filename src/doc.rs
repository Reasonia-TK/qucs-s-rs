//! Editing operations on a schematic, shared by the GUI and the MCP server.

use crate::library::{self, lib, text_width, FONT_PX, LINE_H};
use crate::netlist::Connectivity;
use crate::router::{path_stats, path_wires, Arrive, Costs, Grid, Target, G};
use crate::schematic::{Component, Prop, Schematic, Wire};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Doc {
    pub sch: Schematic,
    pub path: Option<PathBuf>,
    undo: Vec<Schematic>,
    redo: Vec<Schematic>,
    pub dirty: bool,
}

#[derive(Clone, Debug)]
pub struct Terminal {
    pub x: i32,
    pub y: i32,
    pub dir: Option<(i32, i32)>,
    pub island: Option<usize>,
    pub desc: String,
}

impl Doc {
    pub fn new(sch: Schematic, path: Option<PathBuf>) -> Doc {
        let mut d = Doc { sch, path, undo: Vec::new(), redo: Vec::new(), dirty: false };
        d.fix_names();
        d
    }

    pub fn open(path: &Path) -> Result<Doc, String> {
        let sch = Schematic::load(path)?;
        Ok(Doc::new(sch, Some(path.to_path_buf())))
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    pub fn conn(&self) -> Connectivity {
        Connectivity::build(&self.sch, self.dir().as_deref())
    }

    pub fn checkpoint(&mut self) {
        self.undo.push(self.sch.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }
    pub fn undo(&mut self) -> bool {
        if let Some(s) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.sch, s));
            self.dirty = true;
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self) -> bool {
        if let Some(s) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.sch, s));
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn save(&mut self) -> Result<PathBuf, String> {
        let p = self.path.clone().ok_or("schematic has no file name yet")?;
        self.sch.save(&p)?;
        self.dirty = false;
        Ok(p)
    }

    /// Qucs stores every ground as "*"; give each component a unique name so it can be addressed.
    fn fix_names(&mut self) {
        let mut used: HashSet<String> = self.sch.components.iter().map(|c| c.name.clone()).collect();
        for i in 0..self.sch.components.len() {
            let c = &self.sch.components[i];
            if c.name == "*" || c.name.is_empty() {
                let prefix = if c.typ == "GND" { "GND".to_string() } else { library::name_prefix(&c.typ) };
                let name = unique_name(&prefix, &used);
                used.insert(name.clone());
                let c = &mut self.sch.components[i];
                c.name = name;
                c.active |= 4; // keep the name hidden like Qucs does for "*"
            }
        }
    }

    pub fn unique(&self, prefix: &str) -> String {
        let used: HashSet<String> = self.sch.components.iter().map(|c| c.name.clone()).collect();
        unique_name(prefix, &used)
    }

    // ------------------------------------------------------------------ components

    pub fn add_component(
        &mut self,
        typ: &str,
        name: Option<&str>,
        x: i32,
        y: i32,
        rotate: Option<i32>,
        mirror: Option<bool>,
        props: &serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let name = match name {
            Some(n) if !n.is_empty() => {
                if self.sch.component(n).is_some() {
                    return Err(format!("a component named '{n}' already exists"));
                }
                n.to_string()
            }
            _ => {
                let prefix = if typ == "GND" {
                    "GND".into()
                } else if typ == "Lib" {
                    let lc = props.get("Lib").and_then(|l| props.get("Comp").map(|c| (l, c)));
                    lc.and_then(|(l, c)| crate::qlib::find(l.as_str().unwrap_or(""), c.as_str().unwrap_or("")))
                        .map(|(_, c)| if c.symbol.id_prefix.is_empty() { "X".to_string() } else { c.symbol.id_prefix.clone() })
                        .unwrap_or_else(|| "X".into())
                } else {
                    library::name_prefix(typ)
                };
                self.unique(&prefix)
            }
        };
        let mut c = if typ == "Lib" {
            let get = |k: &str| props.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
            let (Some(l), Some(cn)) = (get("Lib"), get("Comp")) else {
                return Err("a Lib component needs \"lib\" and \"comp\" (see search_library)".into());
            };
            crate::qlib::new_lib_component(&l, &cn, &name, snap(x), snap(y))?
        } else {
            library::new_component(typ, &name, snap(x), snap(y))?
        };
        if typ == "GND" {
            c.active |= 4;
        }
        if let Some(r) = rotate {
            c.rotate = r.rem_euclid(4);
        }
        if let Some(m) = mirror {
            c.mirror = m as i32;
        }
        apply_props(&mut c, props)?;
        self.sch.components.push(c);
        let i = self.sch.components.len() - 1;
        self.default_label_pos(i);
        Ok(name)
    }

    /// Put the component text where Qucs would, avoiding the symbol body.
    pub fn default_label_pos(&mut self, i: usize) {
        self.place_label(i, None);
    }

    /// Choose the component text position: the Qucs-like default unless it collides
    /// with symbols, wires or other text; then the free-est of right/left/above/below.
    pub fn place_label(&mut self, i: usize, conn: Option<&Connectivity>) -> bool {
        let c = &self.sch.components[i];
        if c.typ.starts_with('.') || library::is_text_block(&c.typ) {
            return false;
        }
        let p = library::place(c, self.dir().as_deref());
        let lines = library::label_lines(c);
        if lines.is_empty() {
            return false;
        }
        let w = lines.iter().map(|l| text_width(l, FONT_PX)).fold(0.0, f64::max);
        let h = lines.len() as f64 * LINE_H;
        let bb = p.bbox;
        let (cx, cy) = (c.x as f64, c.y as f64);
        let midx = (bb[0] + bb[2]) / 2.0;
        let midy = (bb[1] + bb[3]) / 2.0;
        // candidate top-left corners (absolute), first = preferred default
        let horizontal = p.pins.iter().any(|q| q.dir.1 == 0) && !p.pins.iter().any(|q| q.dir.0 == 0);
        let right_mid = (bb[2] + 4.0, midy - h / 2.0);
        let right_top = (bb[2] + 4.0, bb[1]);
        let above = (midx - w / 2.0, bb[1] - h - 2.0);
        let below = (midx - w / 2.0, bb[3] + 2.0);
        let left_mid = (bb[0] - w - 4.0, midy - h / 2.0);
        let left_top = (bb[0] - w - 4.0, bb[1]);
        let right_below = (bb[2] + 4.0, bb[3] - h.min(bb[3] - bb[1]));
        let cands: Vec<(f64, f64)> = if p.pins.len() > 2 {
            vec![right_top, right_mid, right_below, left_top, left_mid, above, below]
        } else if horizontal {
            vec![above, below, right_mid, left_mid]
        } else {
            vec![right_mid, left_mid, right_top, left_top, above, below]
        };
        let pick = match conn {
            None => cands[0],
            Some(conn) => {
                let mut obstacles = Vec::new();
                for (k, q) in conn.placed.iter().enumerate() {
                    obstacles.push(q.bbox);
                    if k != i {
                        if let Some(l) = q.label_bbox {
                            obstacles.push(l);
                        }
                    }
                }
                for wl in &self.sch.wires {
                    let (a, b) = (wl.x1.min(wl.x2) as f64, wl.x1.max(wl.x2) as f64);
                    let (c0, d) = (wl.y1.min(wl.y2) as f64, wl.y1.max(wl.y2) as f64);
                    obstacles.push([a - 3.0, c0 - 3.0, b + 3.0, d + 3.0]);
                    if !wl.label.is_empty() {
                        obstacles.push([wl.lx as f64, wl.ly as f64, wl.lx as f64 + text_width(&wl.label, FONT_PX), wl.ly as f64 + LINE_H]);
                    }
                }
                let mut best = (f64::MAX, cands[0]);
                for (k, &(x, y)) in cands.iter().enumerate() {
                    let r = [x, y, x + w, y + h];
                    let score: f64 = obstacles.iter().map(|o| overlap(&r, o)).sum::<f64>() + k as f64 * 15.0;
                    if score < best.0 {
                        best = (score, (x, y));
                    }
                }
                best.1
            }
        };
        let (tx, ty) = ((pick.0 - cx).round() as i32, (pick.1 - cy).round() as i32);
        let c = &mut self.sch.components[i];
        let changed = (c.tx, c.ty) != (tx, ty);
        c.tx = tx;
        c.ty = ty;
        changed
    }

    /// Move component texts that collide with something to a free spot.
    pub fn fix_label_collisions(&mut self) -> Vec<String> {
        let mut moved = Vec::new();
        let conn = self.conn();
        let rep = crate::check::check(&self.sch, &conn);
        let names: Vec<String> = rep
            .issues
            .iter()
            .filter(|i| i.kind == "text_overlap")
            .filter_map(|i| i.message.strip_prefix("label of ").and_then(|m| m.split(' ').next()).map(|s| s.to_string()))
            .collect();
        for n in names {
            if let Some(ci) = self.sch.component_index(&n) {
                let conn = self.conn();
                if self.place_label(ci, Some(&conn)) {
                    moved.push(n);
                }
            }
        }
        moved
    }

    pub fn delete_components(&mut self, names: &[String]) -> Result<Vec<String>, String> {
        let mut removed = Vec::new();
        for n in names {
            let i = self.sch.component_index(n).ok_or_else(|| format!("no component '{n}'"))?;
            self.sch.components.remove(i);
            removed.push(n.clone());
        }
        Ok(removed)
    }

    /// Move components; `reroute` re-draws the nets attached to them.
    pub fn move_components(&mut self, moves: &[(String, i32, i32)], reroute: bool) -> Result<Value, String> {
        let before = self.conn();
        let mut nets: Vec<usize> = Vec::new();
        for (n, x, y) in moves {
            let ci = self.sch.component_index(n).ok_or_else(|| format!("no component '{n}'"))?;
            for p in before.pins.iter().filter(|p| p.comp == ci) {
                if !nets.contains(&p.net) {
                    nets.push(p.net);
                }
            }
            let c = &mut self.sch.components[ci];
            c.x = snap(*x);
            c.y = snap(*y);
        }
        if !reroute {
            return Ok(json!({"moved": moves.len()}));
        }
        // islands touching the moved components, identified by their pins before the move
        let moved: HashSet<usize> = moves.iter().filter_map(|(n, _, _)| self.sch.component_index(n)).collect();
        let mut groups: Vec<Vec<(usize, usize)>> = Vec::new(); // (comp, pin) per island
        let mut labels: Vec<Vec<String>> = Vec::new();
        let mut drop_wires: HashSet<usize> = HashSet::new();
        for isl in &before.islands {
            if !isl.pins.iter().any(|&p| moved.contains(&before.pins[p].comp)) {
                continue;
            }
            groups.push(isl.pins.iter().map(|&p| (before.pins[p].comp, before.pins[p].pin)).collect());
            labels.push(isl.labels.clone());
            drop_wires.extend(isl.wires.iter().copied());
        }
        let mut k = 0;
        self.sch.wires.retain(|_| {
            let keep = !drop_wires.contains(&k);
            k += 1;
            keep
        });
        let mut report = Vec::new();
        for (g, l) in groups.iter().zip(labels.iter()) {
            let conn = self.conn();
            let terms: Vec<String> = g
                .iter()
                .filter_map(|&(ci, pi)| conn.pins.iter().find(|p| p.comp == ci && p.pin == pi).map(|p| p.label.clone()))
                .collect();
            if terms.len() >= 2 {
                report.push(self.connect(&terms, None)?);
            }
            for lab in l {
                if let Some(t) = terms.first() {
                    let _ = self.add_net_label(t, lab);
                }
            }
        }
        Ok(json!({"moved": moves.len(), "rerouted": report}))
    }

    // ------------------------------------------------------------------ wiring

    pub fn resolve_terminal(&self, conn: &Connectivity, spec: &str) -> Result<Terminal, String> {
        let s = spec.trim();
        if let Some(rest) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            let v: Vec<i32> = rest.split(',').filter_map(|t| t.trim().parse::<f64>().ok().map(|f| f.round() as i32)).collect();
            if v.len() == 2 {
                let (x, y) = (snap(v[0]), snap(v[1]));
                let island = self.island_at(conn, x, y);
                return Ok(Terminal { x, y, dir: None, island, desc: format!("[{x},{y}]") });
            }
        }
        if let Some(net) = s.strip_prefix("net:") {
            let n = conn.net_by_name(net).ok_or_else(|| format!("no net named '{net}'"))?;
            let isl = conn.nets[n].islands.first().copied();
            // use the first wire end point or pin of the net
            if let Some(&wi) = conn.nets[n].wires.first() {
                let w = &self.sch.wires[wi];
                return Ok(Terminal { x: w.x1, y: w.y1, dir: None, island: isl, desc: s.into() });
            }
            if let Some(&pi) = conn.nets[n].pins.first() {
                let p = &conn.pins[pi];
                return Ok(Terminal { x: p.x, y: p.y, dir: Some(p.dir), island: Some(p.island), desc: s.into() });
            }
            return Err(format!("net '{net}' has no wires or pins"));
        }
        if let Some(p) = conn.find_pin(s) {
            return Ok(Terminal { x: p.x, y: p.y, dir: Some(p.dir), island: Some(p.island), desc: p.label.clone() });
        }
        let comp = s.split('.').next().unwrap_or("");
        if let Some(ci) = self.sch.component_index(comp) {
            let names: Vec<String> = conn.pins.iter().filter(|p| p.comp == ci).map(|p| p.label.clone()).collect();
            return Err(format!("unknown pin '{s}'. Pins of {comp}: {}", names.join(", ")));
        }
        Err(format!("unknown terminal '{s}' (use \"R1.1\", \"[x,y]\" or \"net:name\")"))
    }

    fn island_at(&self, conn: &Connectivity, x: i32, y: i32) -> Option<usize> {
        for (i, w) in self.sch.wires.iter().enumerate() {
            if w.contains(x, y, true) {
                return Some(conn.wire_island[i]);
            }
        }
        conn.pins.iter().find(|p| p.x == x && p.y == y).map(|p| p.island)
    }

    /// Route wires so that all terminals end up on one net.
    pub fn connect(&mut self, terms: &[String], label: Option<&str>) -> Result<Value, String> {
        if terms.len() < 2 && label.is_none() {
            return Err("connect needs at least two terminals".into());
        }
        let conn = self.conn();
        let mut tl: Vec<Terminal> = Vec::new();
        for t in terms {
            tl.push(self.resolve_terminal(&conn, t)?);
        }
        if tl.is_empty() {
            return Err("no terminals".into());
        }
        // tree: islands already connected
        let mut tree_islands: HashSet<usize> = HashSet::new();
        if let Some(i) = tl[0].island {
            tree_islands.insert(i);
        }
        const OWN: i32 = -10;
        let mut tree_pts: Vec<(i32, i32, Arrive)> = vec![(tl[0].x, tl[0].y, arrive_for(tl[0].dir))];
        let mut pending: Vec<usize> = (1..tl.len()).collect();
        // terminals already in the tree (same island) need no wire
        pending.retain(|&k| !(tl[k].island.is_some() && tl[k].island == tl[0].island) && (tl[k].x, tl[k].y) != (tl[0].x, tl[0].y));
        let extra: Vec<(i32, i32)> = tl.iter().map(|t| (t.x, t.y)).collect();
        let mut new_wires: Vec<Wire> = Vec::new();
        let mut routes = Vec::new();
        let mut failed = Vec::new();
        while !pending.is_empty() {
            // targets: tree islands' pins / wires + the tree built so far
            let targets = self.tree_targets(&conn, &tree_islands, &tree_pts, &new_wires);
            // nearest pending terminal
            pending.sort_by_key(|&k| targets.iter().map(|t| (t.x - tl[k].x).abs() + (t.y - tl[k].y).abs()).min().unwrap_or(0));
            let k = pending.remove(0);
            let t = tl[k].clone();
            let ti = t.island;
            let id = |i: usize| if tree_islands.contains(&i) || Some(i) == ti { OWN } else { i as i32 };
            let build = |costs: Costs| {
                let mut g = Grid::new(&self.sch, &conn, &extra, costs, &id);
                for w in &new_wires {
                    g.add_wire(w, OWN);
                }
                g
            };
            let grid = build(Costs::default());
            // start from the terminal, or from anywhere on the wiring it already has
            let mut starts: Vec<((i32, i32), Option<(i32, i32)>)> = vec![((t.x, t.y), t.dir)];
            if let Some(i) = ti {
                let isl = &conn.islands[i];
                for &wi in &isl.wires {
                    let w = &self.sch.wires[wi];
                    let (dx, dy) = ((w.x2 - w.x1).signum() * G, (w.y2 - w.y1).signum() * G);
                    let (mut x, mut y) = (w.x1, w.y1);
                    loop {
                        if !starts.iter().any(|s| s.0 == (x, y)) {
                            starts.push(((x, y), None));
                        }
                        if (x, y) == (w.x2, w.y2) || (dx == 0 && dy == 0) || (w.x1 != w.x2 && w.y1 != w.y2) {
                            break;
                        }
                        x += dx;
                        y += dy;
                    }
                }
                for &pi in &isl.pins {
                    let p = &conn.pins[pi];
                    if !starts.iter().any(|s| s.0 == (p.x, p.y)) {
                        starts.push(((p.x, p.y), Some(p.dir)));
                    }
                }
            }
            let mut path = grid.route_multi(&starts, &targets, OWN);
            if path.is_none() {
                let loose: Vec<((i32, i32), Option<(i32, i32)>)> = starts.iter().map(|s| (s.0, None)).collect();
                path = grid.route_multi(&loose, &targets, OWN);
            }
            if path.is_none() {
                // relaxed: allow running next to symbols and through labels
                let relaxed = build(Costs { near_body: 1, label: 1, near_wire: 1, ..Costs::default() });
                let loose: Vec<((i32, i32), Option<(i32, i32)>)> = starts.iter().map(|s| (s.0, None)).collect();
                path = relaxed.route_multi(&loose, &targets, OWN);
            }
            match path {
                Some(pts) => {
                    let (len, bends) = path_stats(&pts);
                    let end = *pts.last().unwrap();
                    routes.push(json!({"from": t.desc, "to": [end.0, end.1], "length": len, "bends": bends}));
                    for w in path_wires(&pts) {
                        new_wires.push(w);
                    }
                    tree_pts.push((t.x, t.y, arrive_for(t.dir)));
                    if let Some(i) = t.island {
                        tree_islands.insert(i);
                    }
                }
                None => failed.push(t.desc.clone()),
            }
        }
        // T-junctions: split existing wires where a new wire ends on their interior
        let old = std::mem::take(&mut self.sch.wires);
        let old_ends: Vec<(i32, i32)> = old.iter().flat_map(|w| [w.p1(), w.p2()]).collect();
        let mut all = old;
        all.extend(new_wires.iter().cloned());
        for w in &new_wires {
            for p in [w.p1(), w.p2()] {
                all = split_wires_at(all, p);
            }
        }
        // old end points that a new wire passes through (only own-net points can be reached)
        let n_old = all.len() - new_wires.len();
        let _ = n_old;
        for p in old_ends {
            if new_wires.iter().any(|w| w.contains(p.0, p.1, false)) {
                all = split_wires_at(all, p);
            }
        }
        self.sch.wires = all;
        if let (Some(l), Some(t0)) = (label, terms.first()) {
            self.add_net_label(t0, l)?;
        }
        let mut out = json!({"routes": routes});
        if !failed.is_empty() {
            out["failed"] = json!(failed);
            out["hint"] = json!("No clean path was found for some terminals. Move components apart (move_components), or connect via a net label (add_net_label with the same name on both sides).");
        }
        Ok(out)
    }

    fn tree_targets(&self, conn: &Connectivity, islands: &HashSet<usize>, pts: &[(i32, i32, Arrive)], new_wires: &[Wire]) -> Vec<Target> {
        let mut t = Vec::new();
        for &(x, y, a) in pts {
            t.push(Target { x, y, arrive: a });
        }
        let mut wires: Vec<&Wire> = new_wires.iter().collect();
        for &i in islands {
            let isl = &conn.islands[i];
            for &p in &isl.pins {
                let p = &conn.pins[p];
                t.push(Target { x: p.x, y: p.y, arrive: arrive_for(Some(p.dir)) });
            }
            for &wi in &isl.wires {
                wires.push(&self.sch.wires[wi]);
            }
        }
        for w in wires {
            if w.is_point() {
                t.push(Target { x: w.x1, y: w.y1, arrive: Arrive::Any });
                continue;
            }
            t.push(Target { x: w.x1, y: w.y1, arrive: Arrive::Any });
            t.push(Target { x: w.x2, y: w.y2, arrive: Arrive::Any });
            let (dx, dy) = ((w.x2 - w.x1).signum() * G, (w.y2 - w.y1).signum() * G);
            if w.is_horizontal() || w.is_vertical() {
                let (mut x, mut y) = (w.x1 + dx, w.y1 + dy);
                while (x, y) != (w.x2, w.y2) {
                    t.push(Target { x, y, arrive: Arrive::Perp(w.is_horizontal()) });
                    x += dx;
                    y += dy;
                }
            }
        }
        t
    }

    pub fn add_wire_path(&mut self, pts: &[(i32, i32)]) -> usize {
        let pts: Vec<(i32, i32)> = pts.iter().map(|&(x, y)| (snap(x), snap(y))).collect();
        let mut n = 0;
        let mut add = Vec::new();
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a == b {
                continue;
            }
            if a.0 != b.0 && a.1 != b.1 {
                // make it orthogonal (horizontal first)
                add.push(Wire::new(a.0, a.1, b.0, a.1));
                add.push(Wire::new(b.0, a.1, b.0, b.1));
                n += 2;
            } else {
                add.push(Wire::new(a.0, a.1, b.0, b.1));
                n += 1;
            }
        }
        let mut all = std::mem::take(&mut self.sch.wires);
        for w in &add {
            all = split_wires_at(all, w.p1());
            all = split_wires_at(all, w.p2());
        }
        all.extend(add);
        self.sch.wires = all;
        n
    }

    /// Attach a net label to the net of `target` (pin, point or net:name).
    pub fn add_net_label(&mut self, target: &str, label: &str) -> Result<Value, String> {
        let conn = self.conn();
        let t = self.resolve_terminal(&conn, target)?;
        // choose the wire of that island with the best free label spot
        let island = t.island.or_else(|| self.island_at(&conn, t.x, t.y));
        let mut cands: Vec<usize> = match island {
            Some(i) => conn.islands[i].wires.clone(),
            None => vec![],
        };
        // existing labels on that island are replaced
        for &wi in &cands {
            self.sch.wires[wi].label.clear();
        }
        cands.retain(|&wi| !self.sch.wires[wi].is_point());
        let tw = text_width(label, FONT_PX);
        let obstacles = self.obstacles(&conn);
        let mut best: Option<(f64, usize, i32, i32, i32, i32)> = None;
        for &wi in &cands {
            let w = &self.sch.wires[wi];
            let len = w.len();
            if len == 0 {
                continue;
            }
            let steps = (len / 10).max(1);
            for s in 0..=steps {
                let off = s * 10;
                let (ax, ay) = if w.is_horizontal() {
                    (w.x1 + off * (w.x2 - w.x1).signum(), w.y1)
                } else {
                    (w.x1, w.y1 + off * (w.y2 - w.y1).signum())
                };
                let cands_pos: Vec<(f64, f64)> = if w.is_horizontal() {
                    vec![(ax as f64 + 4.0, ay as f64 - LINE_H - 5.0), (ax as f64 - tw - 4.0, ay as f64 - LINE_H - 5.0), (ax as f64 + 4.0, ay as f64 + 5.0)]
                } else {
                    vec![(ax as f64 + 6.0, ay as f64 - LINE_H / 2.0), (ax as f64 - tw - 6.0, ay as f64 - LINE_H / 2.0)]
                };
                for (lx, ly) in cands_pos {
                    let r = [lx, ly, lx + tw, ly + LINE_H];
                    let mut score = obstacles.iter().map(|o| overlap(&r, o)).sum::<f64>();
                    // prefer the middle of long horizontal wires
                    score += if w.is_horizontal() { 0.0 } else { 20.0 };
                    score += ((off as f64) - len as f64 / 2.0).abs() * 0.05;
                    if best.map(|b| score < b.0).unwrap_or(true) {
                        best = Some((score, wi, lx.round() as i32, ly.round() as i32, ax, ay));
                    }
                }
            }
        }
        match best {
            Some((_, wi, lx, ly, ax, ay)) => {
                let w = &mut self.sch.wires[wi];
                w.set_label(label, lx, ly, (ax, ay));
                Ok(json!({"label": label, "wire": [w.x1, w.y1, w.x2, w.y2]}))
            }
            None => {
                // no wire: node label = zero-length wire at the point
                let mut w = Wire::new(t.x, t.y, t.x, t.y);
                w.label = label.to_string();
                w.lx = t.x + 6;
                w.ly = t.y - 22;
                self.sch.wires.push(w);
                Ok(json!({"label": label, "node": [t.x, t.y]}))
            }
        }
    }

    /// Rectangles that labels should avoid.
    pub fn obstacles(&self, conn: &Connectivity) -> Vec<[f64; 4]> {
        let mut o = Vec::new();
        for p in &conn.placed {
            o.push(p.bbox);
            if let Some(l) = p.label_bbox {
                o.push(l);
            }
        }
        for w in &self.sch.wires {
            let (a, b) = (w.x1.min(w.x2) as f64, w.x1.max(w.x2) as f64);
            let (c, d) = (w.y1.min(w.y2) as f64, w.y1.max(w.y2) as f64);
            o.push([a - 2.0, c - 2.0, b + 2.0, d + 2.0]);
            if !w.label.is_empty() {
                o.push([w.lx as f64, w.ly as f64, w.lx as f64 + text_width(&w.label, FONT_PX), w.ly as f64 + LINE_H]);
            }
        }
        o
    }

    /// Rip up and re-route nets (all when `names` is empty).
    pub fn reroute(&mut self, names: &[String]) -> Result<Value, String> {
        let conn = self.conn();
        let sel: Vec<usize> = if names.is_empty() {
            (0..conn.nets.len()).collect()
        } else {
            let mut v = Vec::new();
            for n in names {
                v.push(conn.net_by_name(n).ok_or_else(|| format!("no net '{n}'"))?);
            }
            v
        };
        let mut jobs: Vec<(Vec<String>, Vec<String>, i64)> = Vec::new();
        let mut drop: HashSet<usize> = HashSet::new();
        for &n in &sel {
            for &is in &conn.nets[n].islands {
                let isl = &conn.islands[is];
                if isl.pins.is_empty() {
                    continue; // keep pin-less wire groups as they are
                }
                drop.extend(isl.wires.iter().copied());
                let pins: Vec<String> = isl.pins.iter().map(|&p| conn.pins[p].label.clone()).collect();
                let xs: Vec<i32> = isl.pins.iter().map(|&p| conn.pins[p].x).collect();
                let ys: Vec<i32> = isl.pins.iter().map(|&p| conn.pins[p].y).collect();
                let hp = (xs.iter().max().unwrap() - xs.iter().min().unwrap() + ys.iter().max().unwrap() - ys.iter().min().unwrap()) as i64;
                jobs.push((pins, isl.labels.clone(), hp));
            }
        }
        let mut k = 0;
        self.sch.wires.retain(|_| {
            let keep = !drop.contains(&k);
            k += 1;
            keep
        });
        jobs.sort_by_key(|j| j.2);
        let all_nets: Vec<Vec<String>> = jobs.iter().map(|j| j.0.clone()).collect();
        let flipped = self.optimize_flips(&all_nets);
        let mut out = Vec::new();
        let mut failed = Vec::new();
        for (pins, labels, _) in jobs {
            if pins.len() >= 2 {
                let r = self.connect(&pins, None)?;
                if let Some(f) = r.get("failed") {
                    failed.push(json!({"pins": pins, "failed": f}));
                }
                out.push(r);
            }
            for l in labels {
                self.add_net_label(&pins[0], &l)?;
            }
        }
        let gain = self.improve_routes(2);
        let relabeled = self.fix_label_collisions();
        Ok(json!({"nets_rerouted": out.len(), "failed": failed, "flipped": flipped, "improvement": gain, "labels_moved": relabeled}))
    }

    /// Before routing: turn symmetric two-pin parts (R, C, L) by 180° when that brings
    /// each pin closer to the terminals it will be wired to ("pin swapping").
    pub fn optimize_flips(&mut self, nets: &[Vec<String>]) -> Vec<String> {
        const SYM: [&str; 6] = ["R", "C", "L", "R_SPICE", "C_SPICE", "L_SPICE"];
        let mut flipped = Vec::new();
        for _pass in 0..self.sch.components.len().min(60) {
            let conn = self.conn();
            let deg = conn.degree_map(&self.sch);
            // (comp, pin) -> intended net index, and terminal positions per net
            let mut pin_net: HashMap<(usize, usize), usize> = HashMap::new();
            let mut pos: Vec<Vec<(usize, i32, i32)>> = vec![Vec::new(); nets.len()];
            for (ni, n) in nets.iter().enumerate() {
                for t in n {
                    if let Some(p) = conn.find_pin(t) {
                        pin_net.insert((p.comp, p.pin), ni);
                        pos[ni].push((p.comp, p.x, p.y));
                    } else if let Ok(tt) = self.resolve_terminal(&conn, t) {
                        pos[ni].push((usize::MAX, tt.x, tt.y));
                    }
                }
            }
            let mut best: Option<(i32, usize)> = None;
            for ci in 0..self.sch.components.len() {
                let c = &self.sch.components[ci];
                if !SYM.contains(&c.typ.as_str()) {
                    continue;
                }
                if c.typ == "C" && c.props.get(2).map(|p| p.value.contains("polar")).unwrap_or(false) {
                    continue;
                }
                let pins: Vec<&crate::netlist::PinRef> = conn.pins.iter().filter(|p| p.comp == ci).collect();
                if pins.len() != 2 || pins.iter().any(|p| deg.get(&(p.x, p.y)).copied().unwrap_or(0) > 1) {
                    continue;
                }
                // distance to the centroid of the other terminals of the pin's net
                let dist = |pin_idx: usize, x: i32, y: i32| -> f64 {
                    let Some(&ni) = pin_net.get(&(ci, pin_idx)) else { return 0.0 };
                    let o: Vec<&(usize, i32, i32)> = pos[ni].iter().filter(|q| q.0 != ci).collect();
                    if o.is_empty() {
                        return 0.0;
                    }
                    let cx = o.iter().map(|q| q.1 as f64).sum::<f64>() / o.len() as f64;
                    let cy = o.iter().map(|q| q.2 as f64).sum::<f64>() / o.len() as f64;
                    (cx - x as f64).abs() + (cy - y as f64).abs()
                };
                let centroid = |pin_idx: usize| -> Option<(f64, f64)> {
                    let ni = *pin_net.get(&(ci, pin_idx))?;
                    let o: Vec<&(usize, i32, i32)> = pos[ni].iter().filter(|q| q.0 != ci).collect();
                    if o.is_empty() {
                        return None;
                    }
                    Some((o.iter().map(|q| q.1 as f64).sum::<f64>() / o.len() as f64, o.iter().map(|q| q.2 as f64).sum::<f64>() / o.len() as f64))
                };
                let (a, b) = (pins[0], pins[1]);
                let now = dist(a.pin, a.x, a.y) + dist(b.pin, b.x, b.y);
                let swapped = dist(a.pin, b.x, b.y) + dist(b.pin, a.x, a.y);
                let mut gain = (now - swapped).round() as i32;
                if gain.abs() <= 10 {
                    // tie: the pin whose net lies further along the part's axis should point that way
                    if let (Some(ca), Some(cb)) = (centroid(a.pin), centroid(b.pin)) {
                        let (ux, uy) = ((a.x - b.x) as f64, (a.y - b.y) as f64);
                        let l = (ux * ux + uy * uy).sqrt().max(1.0);
                        let d = ((ca.0 - cb.0) * ux + (ca.1 - cb.1) * uy) / l;
                        if d < -10.0 {
                            gain = 11;
                        }
                    }
                }
                if gain > 10 && best.map(|b| gain > b.0).unwrap_or(true) {
                    best = Some((gain, ci));
                }
            }
            let Some((_, ci)) = best else { break };
            let name = self.sch.components[ci].name.clone();
            if flipped.contains(&name) {
                break; // oscillation guard
            }
            let c = &mut self.sch.components[ci];
            c.rotate = (c.rotate + 2) % 4;
            self.default_label_pos(ci);
            flipped.push(name);
        }
        flipped
    }

    /// Wiring quality score used by the improvement pass (lower is better).
    pub fn wiring_cost(&self) -> i64 {
        let conn = self.conn();
        let r = crate::check::check(&self.sch, &conn);
        let m = &r.metrics;
        let g = |k: &str| m.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        g("total_wire_length") + 30 * g("bends") + 80 * g("crossings") + 400 * g("errors") + 40 * g("warnings")
    }

    /// Rip-up-and-retry: re-route each island alone, keep the result when the score improves.
    pub fn improve_routes(&mut self, passes: usize) -> i64 {
        let start = self.wiring_cost();
        let mut best = start;
        for _ in 0..passes {
            let conn = self.conn();
            let groups: Vec<Vec<String>> = conn
                .islands
                .iter()
                .filter(|i| i.pins.len() >= 2 && !i.wires.is_empty())
                .map(|i| i.pins.iter().map(|&p| conn.pins[p].label.clone()).collect())
                .collect();
            let mut improved = false;
            for g in groups {
                let saved = self.sch.wires.clone();
                let conn = self.conn();
                let Some(p0) = conn.find_pin(&g[0]) else { continue };
                let isl = &conn.islands[p0.island];
                if isl.wires.is_empty() {
                    continue;
                }
                let labels = isl.labels.clone();
                let drop: HashSet<usize> = isl.wires.iter().copied().collect();
                let mut k = 0;
                self.sch.wires.retain(|_| {
                    let keep = !drop.contains(&k);
                    k += 1;
                    keep
                });
                let ok = self.connect(&g, None).map(|r| r.get("failed").is_none()).unwrap_or(false);
                if ok {
                    for l in &labels {
                        let _ = self.add_net_label(&g[0], l);
                    }
                }
                let cost = self.wiring_cost();
                if ok && cost < best {
                    best = cost;
                    improved = true;
                } else {
                    self.sch.wires = saved;
                }
            }
            if !improved {
                break;
            }
        }
        start - best
    }

    /// Clean-up without changing connectivity.
    pub fn tidy(&mut self, relabel: bool, reroute: bool) -> Result<Value, String> {
        let mut actions = Vec::new();
        // snap components to grid
        for c in self.sch.components.iter_mut() {
            let (x, y) = (snap(c.x), snap(c.y));
            if (x, y) != (c.x, c.y) {
                actions.push(format!("snapped {} to grid", c.name));
                c.x = x;
                c.y = y;
            }
        }
        let n0 = self.sch.wires.len();
        self.sch.wires = normalize_wires(std::mem::take(&mut self.sch.wires), &self.conn_pins_points());
        if self.sch.wires.len() != n0 {
            actions.push(format!("merged/removed wires: {} -> {}", n0, self.sch.wires.len()));
        }
        if reroute {
            let r = self.reroute(&[])?;
            actions.push(format!("rerouted all nets: {r}"));
        }
        if relabel {
            // component labels next to their symbol, wire labels re-placed
            for i in 0..self.sch.components.len() {
                let conn = self.conn();
                self.place_label(i, Some(&conn));
            }
            let conn = self.conn();
            let mut jobs = Vec::new();
            for isl in &conn.islands {
                for l in &isl.labels {
                    if let Some(&wi) = isl.wires.first() {
                        let w = &self.sch.wires[wi];
                        jobs.push((format!("[{},{}]", w.x1, w.y1), l.clone()));
                    }
                }
            }
            for (t, l) in jobs {
                let _ = self.add_net_label(&t, &l);
            }
            actions.push("re-placed component and net labels".into());
        }
        Ok(json!({"actions": actions}))
    }

    fn conn_pins_points(&self) -> HashSet<(i32, i32)> {
        self.conn().pins.iter().map(|p| (p.x, p.y)).collect()
    }

    // ------------------------------------------------------------------ simulation blocks

    pub fn add_simulation(&mut self, kind: &str, params: &serde_json::Map<String, Value>, x: Option<i32>, y: Option<i32>) -> Result<String, String> {
        let typ = match kind.to_ascii_lowercase().as_str() {
            "tr" | "tran" | "transient" => ".TR",
            "ac" => ".AC",
            "dc" | "op" => ".DC",
            "sweep" | "sw" | "param" => ".SW",
            "noise" => ".NOISE",
            "fourier" | "four" => ".FOURIER",
            "sp" | "s-param" => ".SP",
            "hb" => ".HB",
            "pz" => ".PZ",
            "disto" => ".DISTO",
            "sens" => ".SENS",
            "fft" => ".FFT",
            k if k.starts_with('.') => return self.add_simulation(&k[1..], params, x, y),
            other => return Err(format!("unknown simulation kind '{other}' (tr, ac, dc, sweep, noise, fourier, sp, hb, pz, disto, sens, fft)")),
        };
        let conn = self.conn();
        let (bx, by) = match (x, y) {
            (Some(x), Some(y)) => (x, y),
            _ => {
                // below the circuit, next to existing simulation blocks
                let mut maxy = 0.0f64;
                let mut minx = f64::MAX;
                for p in &conn.placed {
                    maxy = maxy.max(p.bbox[3]);
                    minx = minx.min(p.bbox[0]);
                    if let Some(l) = p.label_bbox {
                        maxy = maxy.max(l[3]);
                    }
                }
                let sims: Vec<&Component> = self.sch.components.iter().filter(|c| c.typ.starts_with('.')).collect();
                if let Some(last) = sims.iter().max_by_key(|c| c.x) {
                    (last.x + 200, last.y)
                } else {
                    let minx = if minx == f64::MAX { 60.0 } else { minx };
                    (snap(minx as i32), snap(maxy as i32 + 60))
                }
            }
        };
        let prefix = library::name_prefix(typ);
        let name = self.unique(&prefix.to_ascii_uppercase());
        let mut c = library::new_component(typ, &name, snap(bx), snap(by))?;
        apply_props(&mut c, params)?;
        // show the properties that were set explicitly
        if let Some(info) = lib().info(typ) {
            for k in params.keys() {
                if let Some(i) = info.props.iter().position(|p| p.name.eq_ignore_ascii_case(k)) {
                    if let Some(p) = c.props.get_mut(i) {
                        p.display = "1".into();
                    }
                }
            }
        }
        self.sch.components.push(c);
        Ok(name)
    }
}

impl Doc {
    /// Parameter sweep (.SW) over `param` for the analysis `sim` (e.g. "TR1", "AC1", "DC1").
    /// `param` may be a component name (its main value is swept, e.g. "R1", "V1") or a
    /// variable; a missing variable is created in a .PARAM block with `default`.
    pub fn add_parameter_sweep(&mut self, sim: &str, param: &str, start: &str, stop: &str, points: &str, typ: &str, default: Option<&str>) -> Result<String, String> {
        if !self.sch.components.iter().any(|c| c.name == sim && c.typ.starts_with('.')) {
            let sims: Vec<String> = self.sch.components.iter().filter(|c| c.typ.starts_with('.') && c.typ != ".SW").map(|c| c.name.clone()).collect();
            return Err(format!("no simulation '{sim}'. Available: {}", sims.join(", ")));
        }
        let is_comp = self.sch.component(param).is_some() || param.eq_ignore_ascii_case("temp");
        if !is_comp {
            let defined = self.sch.components.iter().any(|c| (c.typ == "SpicePar" || c.typ == "Eqn") && c.props.iter().any(|p| p.value.split('=').next().map(|n| n.trim() == param).unwrap_or(false)));
            if !defined {
                let mut m = serde_json::Map::new();
                m.insert(param.to_string(), json!(default.unwrap_or(start)));
                match self.sch.components.iter().position(|c| c.typ == "SpicePar") {
                    Some(i) => apply_props(&mut self.sch.components[i], &m)?,
                    None => {
                        let (x, y) = self.free_spot();
                        let n = self.add_component("SpicePar", None, x, y, None, None, &serde_json::Map::new())?;
                        let i = self.sch.component_index(&n).unwrap();
                        self.sch.components[i].props.clear();
                        apply_props(&mut self.sch.components[i], &m)?;
                    }
                }
            }
        }
        let mut p = serde_json::Map::new();
        for (k, v) in [("Sim", sim), ("Type", typ), ("Param", param), ("Start", start), ("Stop", stop), ("Points", points)] {
            p.insert(k.into(), json!(v));
        }
        self.add_simulation("sweep", &p, None, None)
    }

    /// A free spot below the circuit for text blocks.
    fn free_spot(&self) -> (i32, i32) {
        let conn = self.conn();
        let mut maxy = 0.0f64;
        let mut maxx = 0.0f64;
        for p in &conn.placed {
            maxy = maxy.max(p.bbox[3]);
            maxx = maxx.max(p.bbox[2]);
        }
        let sims = self.sch.components.iter().filter(|c| c.typ.starts_with('.')).map(|c| c.x).max();
        match sims {
            Some(x) => (snap(x + 200), snap(self.sch.components.iter().filter(|c| c.typ.starts_with('.')).map(|c| c.y).max().unwrap_or(0))),
            None => (snap(maxx as i32 + 80), snap(maxy as i32 + 60)),
        }
    }
}

fn arrive_for(dir: Option<(i32, i32)>) -> Arrive {
    match dir {
        Some((dx, dy)) => [(1, 0), (0, 1), (-1, 0), (0, -1)]
            .iter()
            .position(|&d| d == (-dx, -dy))
            .map(Arrive::Dir)
            .unwrap_or(Arrive::Any),
        None => Arrive::Any,
    }
}

pub fn snap(v: i32) -> i32 {
    ((v as f64) / G as f64).round() as i32 * G
}

pub fn unique_name(prefix: &str, used: &HashSet<String>) -> String {
    let p = if prefix.is_empty() { "X" } else { prefix };
    for i in 1.. {
        let n = format!("{p}{i}");
        if !used.contains(&n) {
            return n;
        }
    }
    unreachable!()
}

fn overlap(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    w * h
}

/// Set properties from a JSON object (`{"R": "10k", "Temp": "27"}`); `"name": {"value": .., "show": bool}` also works.
pub fn apply_props(c: &mut Component, props: &serde_json::Map<String, Value>) -> Result<(), String> {
    let info = lib().info(&c.typ);
    for (k, v) in props {
        if c.typ == "Lib" || c.typ == "Sub" {
            if c.typ == "Lib" && (k == "Lib" || k == "Comp") {
                continue; // fixed when the component is created
            }
            let val = match v {
                Value::String(s) => s.clone(),
                Value::Object(o) => o.get("value").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                other => other.to_string(),
            };
            let n = c.props.len();
            match (0..n).find(|&i| crate::qlib::param_name(c, i, None).map(|p| p.eq_ignore_ascii_case(k)).unwrap_or(false)) {
                Some(i) => c.props[i].value = val,
                None => {
                    let names: Vec<String> = (0..n).filter_map(|i| crate::qlib::param_name(c, i, None)).collect();
                    return Err(format!("{} has no parameter '{k}'. Parameters: {}", c.name, names.join(", ")));
                }
            }
            continue;
        }
        let (val, show) = match v {
            Value::String(s) => (s.clone(), None),
            Value::Number(n) => (n.to_string(), None),
            Value::Bool(b) => ((if *b { "yes" } else { "no" }).to_string(), None),
            Value::Object(o) => (
                o.get("value").map(|x| x.as_str().map(|s| s.to_string()).unwrap_or_else(|| x.to_string())).unwrap_or_default(),
                o.get("show").and_then(|s| s.as_bool()),
            ),
            other => (other.to_string(), None),
        };
        let text_block = library::is_text_block(&c.typ);
        let idx = info.and_then(|i| i.props.iter().position(|p| p.name.eq_ignore_ascii_case(k)));
        match idx {
            Some(i) => {
                while c.props.len() <= i {
                    let d = &info.unwrap().props[c.props.len()];
                    c.props.push(Prop { value: d.default.clone(), display: if d.display { "1" } else { "0" }.into() });
                }
                c.props[i].value = val;
                if let Some(s) = show {
                    c.props[i].display = if s { "1" } else { "0" }.into();
                }
            }
            None if text_block => {
                // equation-like blocks: "name=value" entries
                let entry = format!("{k}={val}");
                if let Some(p) = c.props.iter_mut().find(|p| p.value.split('=').next() == Some(k.as_str())) {
                    p.value = entry;
                } else {
                    // insert before trailing non-equation options (e.g. Eqn "Export")
                    let pos = c.props.iter().rposition(|p| p.value.contains('=')).map(|i| i + 1).unwrap_or(c.props.len());
                    c.props.insert(pos, Prop { value: entry, display: "1".into() });
                }
            }
            None => {
                let names: Vec<String> = info.map(|i| i.props.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
                return Err(format!("{} has no property '{k}'. Properties: {}", c.typ, names.join(", ")));
            }
        }
    }
    Ok(())
}

/// Split wires whose interior contains `p` (creates the T-junction Qucs needs).
pub fn split_wires_at(wires: Vec<Wire>, p: (i32, i32)) -> Vec<Wire> {
    let mut out = Vec::with_capacity(wires.len() + 1);
    for w in wires {
        if !w.is_point() && w.contains(p.0, p.1, false) {
            let mut a = w.clone();
            a.x2 = p.0;
            a.y2 = p.1;
            let mut b = Wire::new(p.0, p.1, w.x2, w.y2);
            // keep the label on the part that holds its anchor
            if !w.label.is_empty() {
                let (ax, ay) = w.label_anchor();
                if !a.contains(ax, ay, true) {
                    b.set_label(&w.label, w.lx, w.ly, (ax, ay));
                    a.label.clear();
                    a.loff = 0;
                    a.lx = 0;
                    a.ly = 0;
                }
            }
            out.push(a);
            out.push(b);
        } else {
            out.push(w);
        }
    }
    out
}

/// Remove zero-length / duplicate wires and merge collinear wires that meet at
/// a point with nothing else attached.
pub fn normalize_wires(mut wires: Vec<Wire>, pins: &HashSet<(i32, i32)>) -> Vec<Wire> {
    // duplicates and zero-length (keep labelled points)
    let mut seen = HashSet::new();
    wires.retain(|w| {
        if w.is_point() && w.label.is_empty() {
            return false;
        }
        let key = if w.p1() <= w.p2() { (w.p1(), w.p2()) } else { (w.p2(), w.p1()) };
        seen.insert(key) || !w.label.is_empty()
    });
    loop {
        let mut deg: HashMap<(i32, i32), usize> = HashMap::new();
        for w in &wires {
            if !w.is_point() {
                *deg.entry(w.p1()).or_default() += 1;
                *deg.entry(w.p2()).or_default() += 1;
            }
        }
        let mut merged = false;
        'outer: for i in 0..wires.len() {
            for j in i + 1..wires.len() {
                let (a, b) = (&wires[i], &wires[j]);
                if a.is_point() || b.is_point() || (!a.label.is_empty() && !b.label.is_empty()) {
                    continue;
                }
                let same_dir = (a.is_horizontal() && b.is_horizontal() && a.y1 == b.y1) || (a.is_vertical() && b.is_vertical() && a.x1 == b.x1);
                if !same_dir {
                    continue;
                }
                let shared = [a.p1(), a.p2()].into_iter().find(|p| *p == b.p1() || *p == b.p2());
                if let Some(s) = shared {
                    if deg.get(&s).copied().unwrap_or(0) != 2 || pins.contains(&s) {
                        continue;
                    }
                    let ao = if a.p1() == s { a.p2() } else { a.p1() };
                    let bo = if b.p1() == s { b.p2() } else { b.p1() };
                    let mut m = Wire::new(ao.0, ao.1, bo.0, bo.1);
                    let src = if !a.label.is_empty() { a } else { b };
                    if !src.label.is_empty() {
                        let (ax, ay) = src.label_anchor();
                        m.set_label(&src.label, src.lx, src.ly, (ax, ay));
                    }
                    wires[i] = m;
                    wires.remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
        if !merged {
            break;
        }
    }
    wires
}
