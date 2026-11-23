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
    pub summary_ja: String,
}

impl Issue {
    pub fn json(&self, n: usize) -> Value {
        json!({"id": n, "kind": self.kind, "severity": self.severity, "message": self.message, "at": [self.at.0.round(), self.at.1.round()]})
    }

    pub fn guidance_ja(&self) -> &'static str {
        match self.kind {
            "pin_on_wire_middle" | "wire_end_on_wire_middle" => "接続する場合は交点に接続点を作ります。別のネットなら、配線を引き直して間隔を空けます。",
            "unconnected_pin" => "接続先がある場合は「配線」または「自動配線」で接続します。",
            "diagonal_wire" => "配線を水平・垂直の区間に引き直します。",
            "off_grid" => "部品または配線端をグリッド上に移動します。",
            "dangling_wire" => "配線端をピンまたは接続点につなぎます。不要な配線は削除します。",
            "overlapping_wires" if self.severity == "error" => "別のネットです。どちらかを引き直して重なりを解消します。",
            "overlapping_wires" => "同じネットです。接続を確認し、重複する区間を整理します。",
            "component_overlap" => "どちらかの部品をドラッグして間隔を空けます。",
            "wire_through_symbol" => "部品の外側を通るように配線を引き直します。",
            "text_overlap" => "ラベルをドラッグして、部品や配線から離します。",
            _ => "",
        }
    }

    pub fn junction_point(&self, sch: &Schematic) -> Option<(i32, i32)> {
        if !matches!(self.kind, "pin_on_wire_middle" | "wire_end_on_wire_middle") {
            return None;
        }
        let point = (self.at.0 as i32, self.at.1 as i32);
        // Several through wires can be unrelated nets at a crossing. Leave that choice to the user.
        let through = sch.wires.iter().filter(|w| !w.is_point() && w.contains(point.0, point.1, false));
        (through.take(2).count() == 1).then_some(point)
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
                summary_ja: format!("ピン {} が配線の途中に重なっていますが、未接続です", p.label),
            });
        } else if d <= 1 {
            issues.push(Issue { kind: "unconnected_pin", severity: "warning", message: format!("{} is not connected", p.label), at: (p.x as f64, p.y as f64), summary_ja: format!("ピン {} が未接続です", p.label) });
        }
    }

    // wire end points
    let pin_pts: std::collections::HashSet<(i32, i32)> = conn.pins.iter().map(|p| (p.x, p.y)).collect();
    for (i, w) in sch.wires.iter().enumerate() {
        if w.x1 != w.x2 && w.y1 != w.y2 {
            issues.push(Issue { kind: "diagonal_wire", severity: "warning", message: format!("wire {i} is diagonal"), at: (w.x1 as f64, w.y1 as f64), summary_ja: format!("配線（{}, {}）が斜めに引かれています", w.x1, w.y1) });
        }
        for c in [w.x1, w.y1, w.x2, w.y2] {
            if c % 10 != 0 {
                let p = if w.x1 % 10 != 0 || w.y1 % 10 != 0 { w.p1() } else { w.p2() };
                issues.push(Issue { kind: "off_grid", severity: "warning", message: format!("wire {i} is off the 10-unit grid"), at: (p.0 as f64, p.1 as f64), summary_ja: format!("配線端（{}, {}）がグリッドから外れています", p.0, p.1) });
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
                    summary_ja: format!("配線端（{}, {}）が別の配線に重なっていますが、未接続です", p.0, p.1),
                });
            } else if d == 1 && !pin_pts.contains(&p) && w.label.is_empty() && !w.is_point() {
                issues.push(Issue { kind: "dangling_wire", severity: "warning", message: format!("open wire end at ({},{})", p.0, p.1), at: (p.0 as f64, p.1 as f64), summary_ja: format!("配線端（{}, {}）が未接続です", p.0, p.1) });
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
                let midpoint = |a1: i32, a2: i32, b1: i32, b2: i32| {
                    let start = a1.min(a2).max(b1.min(b2));
                    let end = a1.max(a2).min(b1.max(b2));
                    (f64::from(start) + f64::from(end)) / 2.0
                };
                let at = if a.is_horizontal() {
                    (midpoint(a.x1, a.x2, b.x1, b.x2), f64::from(a.y1))
                } else {
                    (f64::from(a.x1), midpoint(a.y1, a.y2, b.y1, b.y2))
                };
                issues.push(Issue {
                    kind: "overlapping_wires",
                    severity: if same { "warning" } else { "error" },
                    message: format!("wires {i} and {j} overlap{}", if same { " (same net, redundant)" } else { " (different nets – looks shorted)" }),
                    at,
                    summary_ja: format!("（{}, {}）付近で配線が重なっています", at.0, at.1),
                });
            }
        }
    }

    // components
    for (i, pi) in conn.placed.iter().enumerate() {
        let c = &sch.components[i];
        if c.x % 10 != 0 || c.y % 10 != 0 {
            issues.push(Issue { kind: "off_grid", severity: "warning", message: format!("{} is off the grid", c.name), at: (c.x as f64, c.y as f64), summary_ja: format!("部品 {} がグリッドから外れています", c.name) });
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
                    summary_ja: format!("部品 {} と {} が重なっています", names[i], names[j]),
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
                    summary_ja: format!("配線が部品 {} のシンボル内を通っています", names[i]),
                });
            }
        }
    }

    // labels overlapping things
    let mut label_boxes: Vec<(String, String, [f64; 4])> = Vec::new();
    for (i, p) in conn.placed.iter().enumerate() {
        if let Some(b) = p.label_bbox {
            label_boxes.push((format!("label of {}", names[i]), format!("部品 {} のラベル", names[i]), b));
        }
    }
    for w in &sch.wires {
        if !w.label.is_empty() {
            let b = [w.lx as f64, w.ly as f64, w.lx as f64 + text_width(&w.label, FONT_PX), w.ly as f64 + LINE_H];
            label_boxes.push((format!("net label '{}'", w.label), format!("ネットラベル「{}」", w.label), b));
        }
    }
    for (k, (desc, label_ja, b)) in label_boxes.iter().enumerate() {
        let mut hits = Vec::new();
        for (i, p) in conn.placed.iter().enumerate() {
            let own = desc == &format!("label of {}", names[i]);
            if !own && inter(b, &p.bbox) > 4.0 {
                hits.push(format!("symbol {}", names[i]));
            }
        }
        for (j, (d2, _, b2)) in label_boxes.iter().enumerate() {
            if j > k && inter(b, b2) > 4.0 {
                hits.push(d2.clone());
            }
        }
        let wires_hit = sch.wires.iter().filter(|w| seg_hits_rect(w.x1 as f64, w.y1 as f64, w.x2 as f64, w.y2 as f64, &[b[0] - 1.0, b[1] - 1.0, b[2] + 1.0, b[3] + 1.0])).count();
        if wires_hit > 0 {
            hits.push(format!("{wires_hit} wire(s)"));
        }
        if !hits.is_empty() {
            issues.push(Issue { kind: "text_overlap", severity: "warning", message: format!("{desc} overlaps {}", hits.join(", ")), at: ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0), summary_ja: format!("{label_ja}が部品、配線、または別のラベルに重なっています") });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schematic::Wire;

    #[test]
    fn junction_action_only_targets_one_through_wire() {
        let mut sch = Schematic::new_empty("junction");
        sch.wires = vec![Wire::new(-40, 0, 40, 0), Wire::new(0, -20, 0, 0)];
        let find_junction = |sch: &Schematic| {
            check(sch, &Connectivity::build(sch, None)).issues.into_iter()
                .find(|issue| issue.kind == "wire_end_on_wire_middle").unwrap()
        };
        let issue = find_junction(&sch);
        assert_eq!(issue.junction_point(&sch), Some((0, 0)));

        sch.wires.push(Wire::new(0, -40, 0, 40));
        assert_eq!(issue.junction_point(&sch), None);
        sch.wires.pop();

        sch.wires = crate::doc::split_wires_at(std::mem::take(&mut sch.wires), (0, 0));
        let report = check(&sch, &Connectivity::build(&sch, None));
        assert!(!report.issues.iter().any(|issue| issue.kind == "wire_end_on_wire_middle"));
    }
}
