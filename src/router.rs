//! Orthogonal, net-aware A* wire router on the 10-unit Qucs grid.
//!
//! Rules that keep schematics readable (and electrically honest):
//! * wires never run on top of a wire of another net (no collinear overlap)
//! * wires never touch a foreign pin, wire end point or junction
//! * wires never bend/end on the middle of a foreign wire (would look connected)
//! * crossings are allowed but expensive; bends are expensive
//! * symbol bodies are obstacles; staying close to bodies / labels costs extra
//! * a wire leaves (and enters) a pin along the pin's lead direction

use crate::netlist::Connectivity;
use crate::schematic::{Schematic, Wire};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const G: i32 = 10;
const DIRS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

#[derive(Clone, Copy, Debug)]
pub struct Costs {
    pub step: u32,
    pub bend: u32,
    pub cross: u32,
    pub near_body: u32,
    pub near_wire: u32,
    pub label: u32,
}

impl Default for Costs {
    fn default() -> Self {
        Costs { step: 10, bend: 45, cross: 35, near_body: 9, near_wire: 4, label: 14 }
    }
}

fn dir_index(d: (i32, i32)) -> Option<usize> {
    DIRS.iter().position(|&x| x == d)
}

pub struct Grid {
    pub gx0: i32,
    pub gy0: i32,
    pub w: i32,
    pub h: i32,
    blocked: Vec<bool>,
    soft: Vec<u32>,
    /// net occupying the grid edge (gx,gy)-(gx+1,gy) / (gx,gy)-(gx,gy+1); -1 free
    hedge: Vec<i32>,
    vedge: Vec<i32>,
    /// net of a "node" at the point: wire end/bend, pin; -1 none; -2 several
    node: Vec<i32>,
    /// net of a wire running through the point horizontally / vertically (interior)
    hthru: Vec<i32>,
    vthru: Vec<i32>,
    pub costs: Costs,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arrive {
    Any,
    Dir(usize), // last move must be this direction
    Perp(bool), // true: target wire is horizontal -> arrive vertically
}

#[derive(Clone, Debug)]
pub struct Target {
    pub x: i32,
    pub y: i32,
    pub arrive: Arrive,
}

impl Grid {
    /// `island_id` maps a physical island to the id used for obstacle marking
    /// (islands that belong to the tree being routed map to the routing id).
    pub fn new(sch: &Schematic, conn: &Connectivity, extra_pts: &[(i32, i32)], costs: Costs, island_id: &dyn Fn(usize) -> i32) -> Grid {
        let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        let mut grow = |x: f64, y: f64| {
            b[0] = b[0].min(x.floor() as i32);
            b[1] = b[1].min(y.floor() as i32);
            b[2] = b[2].max(x.ceil() as i32);
            b[3] = b[3].max(y.ceil() as i32);
        };
        for p in &conn.placed {
            grow(p.bbox[0], p.bbox[1]);
            grow(p.bbox[2], p.bbox[3]);
        }
        for w in &sch.wires {
            grow(w.x1 as f64, w.y1 as f64);
            grow(w.x2 as f64, w.y2 as f64);
        }
        for &(x, y) in extra_pts {
            grow(x as f64, y as f64);
        }
        if b[0] > b[2] {
            b = [0, 0, 100, 100];
        }
        let margin = 120;
        let gx0 = (b[0] - margin).div_euclid(G);
        let gy0 = (b[1] - margin).div_euclid(G);
        let gx1 = (b[2] + margin).div_euclid(G) + 1;
        let gy1 = (b[3] + margin).div_euclid(G) + 1;
        let (w, h) = (gx1 - gx0 + 1, gy1 - gy0 + 1);
        let n = (w * h) as usize;
        let mut g = Grid {
            gx0,
            gy0,
            w,
            h,
            blocked: vec![false; n],
            soft: vec![0; n],
            hedge: vec![-1; n],
            vedge: vec![-1; n],
            node: vec![-1; n],
            hthru: vec![-1; n],
            vthru: vec![-1; n],
            costs,
        };
        // symbol bodies
        for (ci, p) in conn.placed.iter().enumerate() {
            let c = &sch.components[ci];
            let bb = p.bbox;
            let textual = c.typ.starts_with('.') || p.pins.is_empty();
            for gy in g.gy0..g.gy0 + g.h {
                for gx in g.gx0..g.gx0 + g.w {
                    let (x, y) = ((gx * G) as f64, (gy * G) as f64);
                    let i = g.idx(gx, gy);
                    if x > bb[0] && x < bb[2] && y > bb[1] && y < bb[3] {
                        if textual {
                            g.soft[i] += costs.label * 3;
                        } else {
                            g.blocked[i] = true;
                        }
                    } else if x >= bb[0] - 10.0 && x <= bb[2] + 10.0 && y >= bb[1] - 10.0 && y <= bb[3] + 10.0 {
                        g.soft[i] += costs.near_body;
                    }
                    if let Some(lb) = p.label_bbox {
                        if x >= lb[0] - 2.0 && x <= lb[2] + 2.0 && y >= lb[1] - 2.0 && y <= lb[3] + 2.0 {
                            g.soft[i] += costs.label;
                        }
                    }
                }
            }
        }
        // wire labels
        for wl in sch.wires.iter().filter(|w| !w.label.is_empty()) {
            let tw = crate::library::text_width(&wl.label, crate::library::FONT_PX);
            let (lx, ly) = (wl.lx as f64, wl.ly as f64);
            g.soft_rect([lx, ly, lx + tw, ly + crate::library::LINE_H], costs.label);
        }
        // pins
        for p in &conn.pins {
            g.mark_node(p.x, p.y, island_id(p.island));
        }
        // wires
        for (wi, w) in sch.wires.iter().enumerate() {
            g.add_wire(w, island_id(conn.wire_island[wi]));
        }
        g
    }

    fn soft_rect(&mut self, r: [f64; 4], cost: u32) {
        let gx0 = (r[0] / G as f64).floor() as i32;
        let gx1 = (r[2] / G as f64).ceil() as i32;
        let gy0 = (r[1] / G as f64).floor() as i32;
        let gy1 = (r[3] / G as f64).ceil() as i32;
        for gy in gy0..=gy1 {
            for gx in gx0..=gx1 {
                if self.inside(gx, gy) {
                    let i = self.idx(gx, gy);
                    self.soft[i] += cost;
                }
            }
        }
    }

    #[inline]
    pub fn inside(&self, gx: i32, gy: i32) -> bool {
        gx >= self.gx0 && gy >= self.gy0 && gx < self.gx0 + self.w && gy < self.gy0 + self.h
    }
    #[inline]
    pub fn idx(&self, gx: i32, gy: i32) -> usize {
        ((gy - self.gy0) * self.w + (gx - self.gx0)) as usize
    }

    fn mark_node(&mut self, x: i32, y: i32, net: i32) {
        if x % G != 0 || y % G != 0 {
            return;
        }
        let (gx, gy) = (x / G, y / G);
        if !self.inside(gx, gy) {
            return;
        }
        let i = self.idx(gx, gy);
        self.node[i] = if self.node[i] == -1 || self.node[i] == net { net } else { -2 };
    }

    /// Register a wire (of `net`) as an obstacle for other nets.
    pub fn add_wire(&mut self, w: &Wire, net: i32) {
        self.mark_node(w.x1, w.y1, net);
        self.mark_node(w.x2, w.y2, net);
        if w.is_point() {
            return;
        }
        let on_grid = w.x1 % G == 0 && w.y1 % G == 0 && w.x2 % G == 0 && w.y2 % G == 0;
        if !on_grid {
            return;
        }
        let (gx1, gy1, gx2, gy2) = (w.x1 / G, w.y1 / G, w.x2 / G, w.y2 / G);
        if w.is_horizontal() {
            let (a, b) = (gx1.min(gx2), gx1.max(gx2));
            for gx in a..b {
                if self.inside(gx, gy1) {
                    let i = self.idx(gx, gy1);
                    self.hedge[i] = net;
                }
            }
            for gx in a + 1..b {
                if self.inside(gx, gy1) {
                    let i = self.idx(gx, gy1);
                    self.hthru[i] = net;
                }
            }
            // keep other nets from hugging this wire
            for gx in a..=b {
                for dy in [-1, 1] {
                    if self.inside(gx, gy1 + dy) {
                        let i = self.idx(gx, gy1 + dy);
                        self.soft[i] += self.costs.near_wire;
                    }
                }
            }
        } else if w.is_vertical() {
            let (a, b) = (gy1.min(gy2), gy1.max(gy2));
            for gy in a..b {
                if self.inside(gx1, gy) {
                    let i = self.idx(gx1, gy);
                    self.vedge[i] = net;
                }
            }
            for gy in a + 1..b {
                if self.inside(gx1, gy) {
                    let i = self.idx(gx1, gy);
                    self.vthru[i] = net;
                }
            }
            for gy in a..=b {
                for dx in [-1, 1] {
                    if self.inside(gx1 + dx, gy) {
                        let i = self.idx(gx1 + dx, gy);
                        self.soft[i] += self.costs.near_wire;
                    }
                }
            }
        }
    }

    fn edge_net(&self, gx: i32, gy: i32, d: usize) -> i32 {
        let (dx, dy) = DIRS[d];
        match (dx, dy) {
            (1, 0) => self.hedge[self.idx(gx, gy)],
            (-1, 0) => self.hedge[self.idx(gx - 1, gy)],
            (0, 1) => self.vedge[self.idx(gx, gy)],
            _ => self.vedge[self.idx(gx, gy - 1)],
        }
    }

    /// Route from `start` (leaving in `start_dir` if given) to any of `targets`.
    /// Returns the poly-line (schematic units) including both end points.
    /// Route from any of several start points (e.g. every point of an already wired
    /// group); each start may require a first-move direction.
    pub fn route_multi(&self, starts: &[((i32, i32), Option<(i32, i32)>)], targets: &[Target], net: i32) -> Option<Vec<(i32, i32)>> {
        let starts: Vec<((i32, i32), Option<(i32, i32)>)> = starts
            .iter()
            .filter(|(p, _)| p.0 % G == 0 && p.1 % G == 0 && self.inside(p.0 / G, p.1 / G))
            .map(|&((x, y), d)| ((x / G, y / G), d))
            .collect();
        if starts.is_empty() {
            return None;
        }
        let start_cells: Vec<(i32, i32)> = starts.iter().map(|s| s.0).collect();
        let n = (self.w * self.h) as usize;
        // target lookup
        let mut tmap: Vec<Option<Arrive>> = vec![None; n];
        let mut tb = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for t in targets {
            if t.x % G != 0 || t.y % G != 0 {
                continue;
            }
            let (gx, gy) = (t.x / G, t.y / G);
            if !self.inside(gx, gy) || start_cells.contains(&(gx, gy)) {
                continue;
            }
            let i = self.idx(gx, gy);
            tmap[i] = Some(match tmap[i] {
                None => t.arrive,
                Some(_) => Arrive::Any,
            });
            tb = [tb[0].min(gx), tb[1].min(gy), tb[2].max(gx), tb[3].max(gy)];
        }
        if tb[0] > tb[2] {
            return None;
        }
        let hfn = |gx: i32, gy: i32| -> u32 {
            let dx = if gx < tb[0] { tb[0] - gx } else if gx > tb[2] { gx - tb[2] } else { 0 };
            let dy = if gy < tb[1] { tb[1] - gy } else if gy > tb[3] { gy - tb[3] } else { 0 };
            (dx + dy) as u32 * self.costs.step
        };
        let ns = n * 4;
        let mut best = vec![u32::MAX; ns];
        let mut parent = vec![u32::MAX; ns];
        let mut heap = BinaryHeap::new();
        let start_state = u32::MAX - 1;
        for &((sx, sy), sd) in &starts {
            let start_dirs: Vec<usize> = match sd.and_then(dir_index) {
                Some(d) => vec![d],
                None => (0..4).collect(),
            };
            for d in start_dirs {
                if let Some((ni, cost)) = self.step_cost(sx, sy, d, None, net, &tmap) {
                    let st = ni * 4 + d;
                    if cost < best[st] {
                        best[st] = cost;
                        parent[st] = start_state;
                        let (gx, gy) = (sx + DIRS[d].0, sy + DIRS[d].1);
                        heap.push(Reverse((cost + hfn(gx, gy), cost, st as u32)));
                    }
                }
            }
        }
        let mut found: Option<usize> = None;
        let mut expanded = 0usize;
        while let Some(Reverse((_, g, st))) = heap.pop() {
            let st = st as usize;
            if g > best[st] {
                continue;
            }
            expanded += 1;
            if expanded > 400_000 {
                break;
            }
            let cell = st / 4;
            let d = st % 4;
            let gx = (cell as i32 % self.w) + self.gx0;
            let gy = (cell as i32 / self.w) + self.gy0;
            if let Some(a) = tmap[cell] {
                let ok = match a {
                    Arrive::Any => true,
                    Arrive::Dir(req) => req == d,
                    Arrive::Perp(horizontal) => (DIRS[d].1 != 0) == horizontal,
                };
                if ok {
                    found = Some(st);
                    break;
                }
            }
            // may not continue through a target point of our own net (would overlap the tree)
            if tmap[cell].is_some() {
                continue;
            }
            for nd in 0..4 {
                if nd == (d + 2) % 4 {
                    continue;
                }
                // no bending on a foreign wire's interior (would look like a junction)
                if nd != d && (self.foreign(self.hthru[cell], net) || self.foreign(self.vthru[cell], net)) {
                    continue;
                }
                if let Some((ni, mut cost)) = self.step_cost(gx, gy, nd, Some(d), net, &tmap) {
                    if nd != d {
                        cost += self.costs.bend;
                    }
                    let ng = g + cost;
                    let ns_ = ni * 4 + nd;
                    if ng < best[ns_] {
                        best[ns_] = ng;
                        parent[ns_] = st as u32;
                        let (nx, ny) = (gx + DIRS[nd].0, gy + DIRS[nd].1);
                        heap.push(Reverse((ng + hfn(nx, ny), ng, ns_ as u32)));
                    }
                }
            }
        }
        let end = found?;
        // reconstruct
        let mut cells = Vec::new();
        let mut st = end;
        loop {
            let cell = st / 4;
            cells.push(cell);
            let p = parent[st];
            if p == start_state {
                // the start cell is one step back along the first move
                let (gx, gy) = ((cell as i32 % self.w) + self.gx0, (cell as i32 / self.w) + self.gy0);
                let (dx, dy) = DIRS[st % 4];
                cells.push(self.idx(gx - dx, gy - dy));
                break;
            }
            st = p as usize;
        }
        cells.reverse();
        let pts: Vec<(i32, i32)> = cells
            .iter()
            .map(|&c| (((c as i32 % self.w) + self.gx0) * G, ((c as i32 / self.w) + self.gy0) * G))
            .collect();
        Some(simplify(&pts))
    }

    #[inline]
    fn foreign(&self, v: i32, net: i32) -> bool {
        v != -1 && v != net
    }

    /// Cost of moving from (gx,gy) in direction d, or None if forbidden.
    fn step_cost(&self, gx: i32, gy: i32, d: usize, _prev: Option<usize>, net: i32, tmap: &[Option<Arrive>]) -> Option<(usize, u32)> {
        let (nx, ny) = (gx + DIRS[d].0, gy + DIRS[d].1);
        if !self.inside(nx, ny) {
            return None;
        }
        let e = self.edge_net(gx, gy, d);
        if e != -1 {
            return None; // never run along an existing wire (own net: it's already a target)
        }
        let ni = self.idx(nx, ny);
        let is_target = tmap[ni].is_some();
        if self.blocked[ni] && !is_target {
            return None;
        }
        let node = self.node[ni];
        if node != -1 && node != net {
            return None; // foreign pin / wire end / junction
        }
        let mut cost = self.costs.step + self.soft[ni];
        let horizontal = DIRS[d].1 == 0;
        let thru_other = if horizontal { self.vthru[ni] } else { self.hthru[ni] };
        if self.foreign(thru_other, net) {
            if is_target {
                return None;
            }
            cost += self.costs.cross;
        }
        let thru_same = if horizontal { self.hthru[ni] } else { self.vthru[ni] };
        if self.foreign(thru_same, net) {
            return None;
        }
        Some((ni, cost))
    }
}

/// Drop collinear intermediate points.
pub fn simplify(pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = Vec::new();
    for &p in pts {
        if out.last() == Some(&p) {
            continue;
        }
        if out.len() >= 2 {
            let a = out[out.len() - 2];
            let b = out[out.len() - 1];
            if (a.0 == b.0 && b.0 == p.0) || (a.1 == b.1 && b.1 == p.1) {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

pub fn path_wires(pts: &[(i32, i32)]) -> Vec<Wire> {
    pts.windows(2).filter(|w| w[0] != w[1]).map(|w| Wire::new(w[0].0, w[0].1, w[1].0, w[1].1)).collect()
}

/// Statistics of a route (for reporting).
pub fn path_stats(pts: &[(i32, i32)]) -> (i32, usize) {
    let len = pts.windows(2).map(|w| (w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()).sum();
    (len, pts.len().saturating_sub(2))
}
