//! Electrical connectivity with Qucs semantics:
//! * wires connect only at their end points; a pin connects to a wire only at a
//!   wire end point (a pin lying on the middle of a wire is *not* connected)
//! * wire/node labels with the same name connect their nets; all grounds are one net
//!
//! A physically connected group of wires and pins is an *island*; islands joined
//! by labels / ground form a *net*.

use crate::library::{place, Placed};
use crate::schematic::Schematic;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct PinRef {
    pub comp: usize,
    pub pin: usize,
    pub label: String, // "R1.1"
    pub x: i32,
    pub y: i32,
    pub dir: (i32, i32),
    pub net: usize,
    pub island: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Net {
    pub name: String,
    pub pins: Vec<usize>,  // indices into Connectivity::pins
    pub wires: Vec<usize>, // indices into schematic wires
    pub islands: Vec<usize>,
    pub named: bool, // name from a label / ground
}

#[derive(Clone, Debug, Default)]
pub struct Island {
    pub net: usize,
    pub pins: Vec<usize>,
    pub wires: Vec<usize>,
    pub labels: Vec<String>,
}

#[allow(dead_code)]
pub struct Connectivity {
    pub placed: Vec<Placed>,
    pub pins: Vec<PinRef>,
    pub nets: Vec<Net>,
    pub islands: Vec<Island>,
    pub wire_net: Vec<usize>,
    pub wire_island: Vec<usize>,
    pub point_net: HashMap<(i32, i32), usize>,
}

pub struct Dsu {
    p: Vec<usize>,
}
impl Dsu {
    pub fn new(n: usize) -> Dsu {
        Dsu { p: (0..n).collect() }
    }
    pub fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.p[r] != r {
            r = self.p[r];
        }
        let mut c = x;
        while self.p[c] != r {
            let n = self.p[c];
            self.p[c] = r;
            c = n;
        }
        r
    }
    pub fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.p[a] = b;
        }
    }
}

impl Connectivity {
    pub fn build(sch: &Schematic, dir: Option<&Path>) -> Connectivity {
        let placed: Vec<Placed> = sch.components.iter().map(|c| place(c, dir)).collect();
        let mut ids: HashMap<(i32, i32), usize> = HashMap::new();
        let id = |p: (i32, i32), ids: &mut HashMap<(i32, i32), usize>| -> usize {
            let n = ids.len();
            *ids.entry(p).or_insert(n)
        };
        let wire_pts: Vec<(usize, usize)> = sch.wires.iter().map(|w| (id(w.p1(), &mut ids), id(w.p2(), &mut ids))).collect();
        let mut pin_pts = Vec::new();
        for (ci, p) in placed.iter().enumerate() {
            for pin in &p.pins {
                pin_pts.push((ci, pin.index, id((pin.x, pin.y), &mut ids)));
            }
        }
        let mut dsu = Dsu::new(ids.len());
        for &(a, b) in &wire_pts {
            dsu.union(a, b);
        }
        // islands
        let mut root_island: BTreeMap<usize, usize> = BTreeMap::new();
        let mut islands: Vec<Island> = Vec::new();
        let mut island_of = |pt: usize, dsu: &mut Dsu, islands: &mut Vec<Island>| -> usize {
            let r = dsu.find(pt);
            *root_island.entry(r).or_insert_with(|| {
                islands.push(Island::default());
                islands.len() - 1
            })
        };
        let mut pins = Vec::new();
        let mut gnd_islands = Vec::new();
        for &(ci, pi, a) in &pin_pts {
            let is = island_of(a, &mut dsu, &mut islands);
            let c = &sch.components[ci];
            let pp = &placed[ci].pins[pi];
            islands[is].pins.push(pins.len());
            if c.typ == "GND" {
                gnd_islands.push(is);
            }
            pins.push(PinRef { comp: ci, pin: pi, label: format!("{}.{}", c.name, pp.name), x: pp.x, y: pp.y, dir: pp.dir, net: 0, island: is });
        }
        let mut wire_island = Vec::new();
        for (wi, &(a, _)) in wire_pts.iter().enumerate() {
            let is = island_of(a, &mut dsu, &mut islands);
            islands[is].wires.push(wi);
            let w = &sch.wires[wi];
            if !w.label.is_empty() && !islands[is].labels.contains(&w.label) {
                islands[is].labels.push(w.label.clone());
            }
            wire_island.push(is);
        }
        // islands -> nets (labels with the same name and all grounds are connected)
        let mut nd = Dsu::new(islands.len());
        let mut by_label: HashMap<String, usize> = HashMap::new();
        for (i, isl) in islands.iter().enumerate() {
            for l in &isl.labels {
                let key = if l.eq_ignore_ascii_case("gnd") { "gnd".to_string() } else { l.clone() };
                match by_label.get(&key) {
                    Some(&j) => nd.union(i, j),
                    None => {
                        by_label.insert(key, i);
                    }
                }
            }
        }
        if let Some(&g0) = gnd_islands.first() {
            for &g in &gnd_islands {
                nd.union(g0, g);
            }
            if let Some(&j) = by_label.get("gnd") {
                nd.union(g0, j);
            }
        }
        let mut root_net: BTreeMap<usize, usize> = BTreeMap::new();
        let mut nets: Vec<Net> = Vec::new();
        for i in 0..islands.len() {
            let r = nd.find(i);
            let n = *root_net.entry(r).or_insert_with(|| {
                nets.push(Net::default());
                nets.len() - 1
            });
            islands[i].net = n;
            nets[n].islands.push(i);
        }
        for (pi, p) in pins.iter_mut().enumerate() {
            p.net = islands[p.island].net;
            nets[p.net].pins.push(pi);
        }
        let mut wire_net = Vec::new();
        for (wi, &is) in wire_island.iter().enumerate() {
            let n = islands[is].net;
            nets[n].wires.push(wi);
            wire_net.push(n);
        }
        for &g in &gnd_islands {
            let n = islands[g].net;
            nets[n].name = "gnd".into();
            nets[n].named = true;
        }
        for isl in &islands {
            if let Some(l) = isl.labels.first() {
                let n = &mut nets[isl.net];
                if !n.named {
                    n.name = l.clone();
                    n.named = true;
                }
            }
        }
        let mut k = 1;
        for n in nets.iter_mut() {
            if n.name.is_empty() {
                n.name = format!("_net{k}");
                k += 1;
            }
        }
        let mut point_net = HashMap::new();
        for (p, &i) in &ids {
            let r = dsu.find(i);
            if let Some(&is) = root_island.get(&r) {
                point_net.insert(*p, islands[is].net);
            }
        }
        Connectivity { placed, pins, nets, islands, wire_net, wire_island, point_net }
    }

    pub fn find_pin(&self, label: &str) -> Option<&PinRef> {
        let l = label.trim();
        if let Some(p) = self.pins.iter().find(|p| p.label == l) {
            return Some(p);
        }
        let (comp, pin) = l.rsplit_once('.')?;
        let cand: Vec<&PinRef> = self.pins.iter().filter(|p| p.label.rsplit_once('.').map(|x| x.0) == Some(comp)).collect();
        if let Some(p) = cand.iter().find(|p| p.label.rsplit_once('.').unwrap().1.eq_ignore_ascii_case(pin)) {
            return Some(p);
        }
        if let Ok(n) = pin.parse::<usize>() {
            if n >= 1 {
                return cand.into_iter().find(|p| p.pin == n - 1);
            }
        }
        None
    }

    pub fn net_by_name(&self, name: &str) -> Option<usize> {
        self.nets.iter().position(|n| n.name == name)
    }

    /// Net at a point (wire end point or pin); also wire interiors when `interior`.
    #[allow(dead_code)]
    pub fn net_at(&self, sch: &Schematic, x: i32, y: i32, interior: bool) -> Option<usize> {
        if let Some(&n) = self.point_net.get(&(x, y)) {
            return Some(n);
        }
        if interior {
            for (i, w) in sch.wires.iter().enumerate() {
                if w.contains(x, y, true) {
                    return Some(self.wire_net[i]);
                }
            }
        }
        None
    }

    /// Number of connections at a point (wire ends + pins), used for junction dots.
    pub fn degree_map(&self, sch: &Schematic) -> HashMap<(i32, i32), usize> {
        let mut m: HashMap<(i32, i32), usize> = HashMap::new();
        for w in &sch.wires {
            if w.is_point() {
                continue;
            }
            *m.entry(w.p1()).or_default() += 1;
            *m.entry(w.p2()).or_default() += 1;
        }
        for p in &self.pins {
            *m.entry((p.x, p.y)).or_default() += 1;
        }
        m
    }
}
