//! Layout / readability checks and wiring metrics.

use crate::library::{text_width, FONT_PX, LINE_H};
use crate::netlist::Connectivity;
use crate::schematic::Schematic;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Issue {
    pub kind: &'static str,
    pub severity: &'static str, // error | warning | info
    pub message: String,
    pub at: (f64, f64),
}

impl Issue {
    pub fn json(&self, n: usize) -> Value {
        json!({"id": n, "kind": self.kind, "severity": self.severity, "message": self.message, "at": [self.at.0.round(), self.at.1.round()]})
    }
}

fn inter(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    w * h
}

fn seg_hits_rect(x1: f64, y1: f64, x2: f64, y2: f64, r: &[f64; 4]) -> bool {
    // strictly inside the rectangle interior (shrunk by 1)
    let r = [r[0] + 1.0, r[1] + 1.0, r[2] - 1.0, r[3] - 1.0];
    if r[0] >= r[2] || r[1] >= r[3] {
        return false;
    }
    if y1 == y2 {
        y1 > r[1] && y1 < r[3] && x1.max(x2) > r[0] && x1.min(x2) < r[2]
    } else if x1 == x2 {
        x1 > r[0] && x1 < r[2] && y1.max(y2) > r[1] && y1.min(y2) < r[3]
    } else {
        false
    }
}

pub struct Report {
    pub issues: Vec<Issue>,
    pub metrics: Value,
}

pub fn check(sch: &Schematic, conn: &Connectivity) -> Report {
    let mut issues = Vec::new();
    let deg = conn.degree_map(sch);
    let names: Vec<&str> = sch.components.iter().map(|c| c.name.as_str()).collect();

    // pins
    for p in &conn.pins {
        let d = deg.get(&(p.x, p.y)).copied().unwrap_or(0);
        let on_mid = sch.wires.iter().any(|w| w.contains(p.x, p.y, false));
        if on_mid {
            issues.push(Issue {
                kind: "pin_on_wire_middle",
                severity: "error",
                message: format!("{} touches the middle of a wire but is NOT connected (Qucs connects only at wire ends). Use connect or split the wire.", p.label),
                at: (p.x as f64, p.y as f64),
            });
        } else if d <= 1 {
            issues.push(Issue { kind: "unconnected_pin", severity: "warning", message: format!("{} is not connected", p.label), at: (p.x as f64, p.y as f64) });
        }
    }

    // wire end points
    let pin_pts: std::collections::HashSet<(i32, i32)> = conn.pins.iter().map(|p| (p.x, p.y)).collect();
    for (i, w) in sch.wires.iter().enumerate() {
        if w.x1 != w.x2 && w.y1 != w.y2 {
            issues.push(Issue { kind: "diagonal_wire", severity: "warning", message: format!("wire {i} is diagonal"), at: (w.x1 as f64, w.y1 as f64) });
        }
        for c in [w.x1, w.y1, w.x2, w.y2] {
            if c % 10 != 0 {
                issues.push(Issue { kind: "off_grid", severity: "warning", message: format!("wire {i} is off the 10-unit grid"), at: (w.x1 as f64, w.y1 as f64) });
                break;
            }
        }
        for p in [w.p1(), w.p2()] {
            let mid_other = sch.wires.iter().enumerate().any(|(j, o)| j != i && o.contains(p.0, p.1, false));
            let d = deg.get(&p).copied().unwrap_or(0);
            if mid_other && d <= 1 {
                issues.push(Issue {
                    kind: "wire_end_on_wire_middle",
                    severity: "error",
                    message: format!("wire end ({},{}) lies on the middle of another wire but is NOT connected", p.0, p.1),
                    at: (p.0 as f64, p.1 as f64),
                });
            } else if d == 1 && !pin_pts.contains(&p) && w.label.is_empty() && !w.is_point() {
                issues.push(Issue { kind: "dangling_wire", severity: "warning", message: format!("open wire end at ({},{})", p.0, p.1), at: (p.0 as f64, p.1 as f64) });
            }
        }
    }

    // overlapping wires
    for i in 0..sch.wires.len() {
        for j in i + 1..sch.wires.len() {
            let (a, b) = (&sch.wires[i], &sch.wires[j]);
            if a.is_point() || b.is_point() {
                continue;
            }
            let ov = if a.is_horizontal() && b.is_horizontal() && a.y1 == b.y1 {
                (a.x1.max(a.x2).min(b.x1.max(b.x2)) - a.x1.min(a.x2).max(b.x1.min(b.x2))) > 0
            } else if a.is_vertical() && b.is_vertical() && a.x1 == b.x1 {
                (a.y1.max(a.y2).min(b.y1.max(b.y2)) - a.y1.min(a.y2).max(b.y1.min(b.y2))) > 0
            } else {
                false
            };
            if ov {
                let same = conn.wire_net[i] == conn.wire_net[j];
                issues.push(Issue {
                    kind: "overlapping_wires",
                    severity: if same { "warning" } else { "error" },
                    message: format!("wires {i} and {j} overlap{}", if same { " (same net, redundant)" } else { " (different nets – looks shorted)" }),
                    at: (a.x1 as f64, a.y1 as f64),
                });
            }
        }
    }

    // components
    for (i, pi) in conn.placed.iter().enumerate() {
        let c = &sch.components[i];
        if c.x % 10 != 0 || c.y % 10 != 0 {
            issues.push(Issue { kind: "off_grid", severity: "warning", message: format!("{} is off the grid", c.name), at: (c.x as f64, c.y as f64) });
        }
        let shrink = |b: [f64; 4]| [b[0] + 3.0, b[1] + 3.0, b[2] - 3.0, b[3] - 3.0];
        for j in i + 1..conn.placed.len() {
            let pj = &conn.placed[j];
            if inter(&shrink(pi.bbox), &shrink(pj.bbox)) > 0.0 {
                issues.push(Issue {
                    kind: "component_overlap",
                    severity: "error",
                    message: format!("{} and {} overlap", names[i], names[j]),
                    at: ((pi.bbox[0] + pi.bbox[2]) / 2.0, (pi.bbox[1] + pi.bbox[3]) / 2.0),
                });
            }
        }
        if pi.pins.is_empty() {
            continue;
        }
        for (wi, w) in sch.wires.iter().enumerate() {
            if seg_hits_rect(w.x1 as f64, w.y1 as f64, w.x2 as f64, w.y2 as f64, &pi.bbox) {
                // allow a wire that ends at one of this component's pins and lies along its lead
                issues.push(Issue {
                    kind: "wire_through_symbol",
                    severity: "error",
                    message: format!("wire {wi} runs through the symbol of {}", names[i]),
                    at: ((pi.bbox[0] + pi.bbox[2]) / 2.0, (pi.bbox[1] + pi.bbox[3]) / 2.0),
                });
            }
        }
    }

    // labels overlapping things
    let mut label_boxes: Vec<(String, [f64; 4])> = Vec::new();
    for (i, p) in conn.placed.iter().enumerate() {
        if let Some(b) = p.label_bbox {
            label_boxes.push((format!("label of {}", names[i]), b));
        }
    }
    for w in &sch.wires {
        if !w.label.is_empty() {
            let b = [w.lx as f64, w.ly as f64, w.lx as f64 + text_width(&w.label, FONT_PX), w.ly as f64 + LINE_H];
            label_boxes.push((format!("net label '{}'", w.label), b));
        }
    }
    for (k, (desc, b)) in label_boxes.iter().enumerate() {
        let mut hits = Vec::new();
        for (i, p) in conn.placed.iter().enumerate() {
            let own = desc == &format!("label of {}", names[i]);
            if !own && inter(b, &p.bbox) > 4.0 {
                hits.push(format!("symbol {}", names[i]));
            }
        }
        for (j, (d2, b2)) in label_boxes.iter().enumerate() {
            if j > k && inter(b, b2) > 4.0 {
                hits.push(d2.clone());
            }
        }
        let wires_hit = sch.wires.iter().filter(|w| seg_hits_rect(w.x1 as f64, w.y1 as f64, w.x2 as f64, w.y2 as f64, &[b[0] - 1.0, b[1] - 1.0, b[2] + 1.0, b[3] + 1.0])).count();
        if wires_hit > 0 {
            hits.push(format!("{wires_hit} wire(s)"));
        }
        if !hits.is_empty() {
            issues.push(Issue { kind: "text_overlap", severity: "warning", message: format!("{desc} overlaps {}", hits.join(", ")), at: ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0) });
        }
    }

    // metrics
    let total_len: i32 = sch.wires.iter().map(|w| w.len()).sum();
    let mut crossings = 0;
    for i in 0..sch.wires.len() {
        for j in i + 1..sch.wires.len() {
            let (a, b) = (&sch.wires[i], &sch.wires[j]);
            let (h, v) = if a.is_horizontal() && b.is_vertical() {
                (a, b)
            } else if a.is_vertical() && b.is_horizontal() {
                (b, a)
            } else {
                continue;
            };
            if h.is_point() || v.is_point() {
                continue;
            }
            let x = v.x1;
            let y = h.y1;
            if x > h.x1.min(h.x2) && x < h.x1.max(h.x2) && y > v.y1.min(v.y2) && y < v.y1.max(v.y2) {
                crossings += 1;
            }
        }
    }
    let mut ends: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, w) in sch.wires.iter().enumerate() {
        if !w.is_point() {
            ends.entry(w.p1()).or_default().push(i);
            ends.entry(w.p2()).or_default().push(i);
        }
    }
    let bends = ends
        .iter()
        .filter(|(p, v)| v.len() == 2 && !pin_pts.contains(p) && sch.wires[v[0]].is_horizontal() != sch.wires[v[1]].is_horizontal())
        .count();
    let junctions = deg.values().filter(|&&d| d >= 3).count();
    let errors = issues.iter().filter(|i| i.severity == "error").count();
    let warnings = issues.iter().filter(|i| i.severity == "warning").count();
    let metrics = json!({
        "components": sch.components.len(),
        "wires": sch.wires.len(),
        "nets": conn.nets.len(),
        "total_wire_length": total_len,
        "bends": bends,
        "crossings": crossings,
        "junctions": junctions,
        "errors": errors,
        "warnings": warnings,
    });
    Report { issues, metrics }
}
