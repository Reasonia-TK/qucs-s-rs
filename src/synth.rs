//! Turn calculator results (filters, attenuators) into a clean schematic:
//! series parts on one row, shunt branches hanging down to ground, then wired
//! by the router. Optionally adds a test bench (source, load, AC sweep).

use crate::doc::{snap, Doc};
use crate::rftools::{self, AttenuatorTopology, Design, FilterClass, FilterResponse, FilterSpec};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};

const STEP: i32 = 120;

/// Place `design` with its input node at (x0, y0). Returns the component names per part.
pub fn place_design(doc: &mut Doc, d: &Design, x0: i32, y0: i32, bench: Option<(f64, f64, f64, f64)>) -> Result<Value, String> {
    let (x0, y0) = (snap(x0), snap(y0));
    let is_gnd = |n: &str| n.eq_ignore_ascii_case("gnd");
    // main path in -> out over non-ground parts (BFS)
    let mut adj: HashMap<&str, Vec<(usize, &str)>> = HashMap::new();
    for (k, p) in d.parts.iter().enumerate() {
        if p.nets.len() == 2 && !is_gnd(&p.nets[0]) && !is_gnd(&p.nets[1]) {
            adj.entry(&p.nets[0]).or_default().push((k, &p.nets[1]));
            adj.entry(&p.nets[1]).or_default().push((k, &p.nets[0]));
        }
    }
    let mut prev: HashMap<&str, (&str, usize)> = HashMap::new();
    let mut q = VecDeque::from(["in"]);
    let mut seen: HashSet<&str> = HashSet::from(["in"]);
    while let Some(n) = q.pop_front() {
        for &(k, m) in adj.get(n).map(|v| v.as_slice()).unwrap_or(&[]) {
            if seen.insert(m) {
                prev.insert(m, (n, k));
                q.push_back(m);
            }
        }
    }
    let mut path_nodes = vec!["out"];
    let mut series: HashSet<usize> = HashSet::new();
    if d.parts.iter().any(|p| p.nets.iter().any(|n| n == "out")) && prev.contains_key("out") {
        let mut cur = "out";
        while let Some(&(p, k)) = prev.get(cur) {
            series.insert(k);
            path_nodes.push(p);
            cur = p;
        }
    } else {
        path_nodes = vec!["in"];
    }
    path_nodes.reverse();
    // x position of every node on the main path
    let mut node_x: HashMap<String, i32> = HashMap::new();
    let mut x = x0;
    for (i, n) in path_nodes.iter().enumerate() {
        if i > 0 {
            x += STEP;
        }
        node_x.insert(n.to_string(), x);
    }
    let mut names: Vec<String> = Vec::new();
    let mut part_name: HashMap<usize, String> = HashMap::new();
    let mut shunt_count: HashMap<String, i32> = HashMap::new();
    let mut hang_y: HashMap<String, (i32, i32)> = HashMap::new(); // hanging node -> (x, y)
    let mut gnd_pins: Vec<String> = Vec::new();
    let none = Map::new();
    let mut order: Vec<usize> = (0..d.parts.len()).collect();
    // series first, then shunts attached to the path, then the rest
    order.sort_by_key(|&k| if series.contains(&k) { 0 } else if d.parts[k].nets.iter().any(|n| node_x.contains_key(n)) { 1 } else { 2 });
    let mut placed_nodes: HashMap<String, (i32, i32)> = node_x.iter().map(|(n, &x)| (n.clone(), (x, y0))).collect();
    let mut pending: Vec<usize> = order.clone();
    let mut guard = 0;
    while !pending.is_empty() && guard < 1000 {
        guard += 1;
        let k = pending.remove(0);
        let p = &d.parts[k];
        let mut props = Map::new();
        for (a, b) in &p.props {
            props.insert(a.clone(), json!(b));
        }
        let name;
        if series.contains(&k) {
            let (xa, xb) = (node_x[&p.nets[0]], node_x[&p.nets[1]]);
            name = doc.add_component(&p.typ, Some(&unique(doc, &p.name)), (xa + xb) / 2, y0, Some(0), None, &props)?;
            // pin 1 must face net 0
            let flip = xa > xb;
            if flip {
                let i = doc.sch.component_index(&name).unwrap();
                doc.sch.components[i].rotate = 2;
                doc.default_label_pos(i);
            }
        } else {
            // a vertical branch: the end that is already placed is the top
            let (top, bottom, pin_top_first) = if let Some(&pos) = placed_nodes.get(&p.nets[0]) {
                (pos, p.nets[1].clone(), true)
            } else if let Some(&pos) = placed_nodes.get(&p.nets[1]) {
                (pos, p.nets[0].clone(), false)
            } else {
                pending.push(k);
                continue;
            };
            let key = format!("{},{}", top.0, top.1);
            let c = shunt_count.entry(key).or_insert(0);
            let x = top.0 + *c * 60;
            *c += 1;
            let y = top.1 + 90;
            // R/C/L rotate=1: pin 1 at the bottom; rotate=3: pin 1 at the top
            let rot = if pin_top_first { 3 } else { 1 };
            name = doc.add_component(&p.typ, Some(&unique(doc, &p.name)), x, y, Some(rot), None, &props)?;
            if is_gnd(&bottom) {
                let g = doc.add_component("GND", None, x, y + 70, None, None, &none)?;
                gnd_pins.push(format!("{g}.1"));
            } else {
                placed_nodes.entry(bottom.clone()).or_insert((x, y + 60));
                hang_y.insert(bottom, (x, y + 60));
            }
        }
        part_name.insert(k, name.clone());
        names.push(name);
    }
    let mut nets: HashMap<String, Vec<String>> = HashMap::new();
    for (k, p) in d.parts.iter().enumerate() {
        let Some(name) = part_name.get(&k) else { continue };
        for (pi, net) in p.nets.iter().enumerate() {
            if is_gnd(net) {
                continue; // grounded through the GND symbol placed under the part
            }
            nets.entry(net.clone()).or_default().push(format!("{name}.{}", pi + 1));
        }
    }
    // ground symbols: connect each to the part pin right above it
    let conn = doc.conn();
    let mut gnd_nets: Vec<Vec<String>> = Vec::new();
    for g in &gnd_pins {
        if let Some(gp) = conn.find_pin(g) {
            if let Some(above) = conn.pins.iter().filter(|q| q.x == gp.x && q.y < gp.y && !q.label.starts_with("GND")).max_by_key(|q| q.y) {
                gnd_nets.push(vec![above.label.clone(), g.clone()]);
            }
        }
    }
    let mut report = Vec::new();
    for terms in gnd_nets {
        report.push(doc.connect(&terms, None)?);
    }
    let mut keys: Vec<&String> = nets.keys().collect();
    keys.sort();
    for n in keys {
        let t = &nets[n];
        if t.len() >= 2 {
            let label = if n == "in" || n == "out" { Some(n.as_str()) } else { None };
            report.push(doc.connect(t, label)?);
        }
    }
    let mut out = json!({"components": names, "nets": nets});
    if let Some((z_src, z_load, f_start, f_stop)) = bench {
        out["test_bench"] = add_bench(doc, &nets, &node_x, x0, y0, z_src, z_load, f_start, f_stop)?;
    }
    Ok(out)
}

fn unique(doc: &Doc, want: &str) -> String {
    if doc.sch.component(want).is_none() {
        return want.to_string();
    }
    doc.unique(want.trim_end_matches(|c: char| c.is_ascii_digit()))
}

/// Source (Vac + series R) at the input, load R at the output, AC sweep.
#[allow(clippy::too_many_arguments)]
fn add_bench(doc: &mut Doc, nets: &HashMap<String, Vec<String>>, node_x: &HashMap<String, i32>, x0: i32, y0: i32, z_src: f64, z_load: f64, f_start: f64, f_stop: f64) -> Result<Value, String> {
    let none = Map::new();
    // pins of the in/out nets that sit on the signal row (avoids detours to shunt parts)
    let conn = doc.conn();
    let row_pin = |net: &str| -> Option<String> {
        let v = nets.get(net)?;
        v.iter().min_by_key(|t| conn.find_pin(t).map(|p| (p.y - y0).abs()).unwrap_or(i32::MAX)).cloned()
    };
    let in_pin = row_pin("in").ok_or("design has no input net")?;
    let out_pin = row_pin("out");
    let xin = x0 - 2 * STEP - 30;
    let mut p = Map::new();
    p.insert("U".into(), json!("1 V"));
    let vs = doc.add_component("Vac", Some(&unique(doc, "VS")), xin, y0 + 90, None, None, &p)?;
    let g1 = doc.add_component("GND", None, xin, y0 + 160, None, None, &none)?;
    let mut r = Map::new();
    r.insert("R".into(), json!(rftools::format_value(z_src, "Ohm")));
    let rs = doc.add_component("R", Some(&unique(doc, "RS")), x0 - STEP, y0, Some(0), None, &r)?;
    doc.connect(&[format!("{vs}.-"), format!("{g1}.1")], None)?;
    doc.connect(&[format!("{vs}.+"), format!("{rs}.1")], Some("src"))?;
    doc.connect(&[format!("{rs}.2"), in_pin], Some("in"))?;
    let mut load = Value::Null;
    if let Some(op) = out_pin {
        let xo = node_x.get("out").copied().unwrap_or(x0) + STEP + 60;
        let mut r = Map::new();
        r.insert("R".into(), json!(rftools::format_value(z_load, "Ohm")));
        let rl = doc.add_component("R", Some(&unique(doc, "RL")), xo, y0 + 90, Some(3), None, &r)?;
        let g2 = doc.add_component("GND", None, xo, y0 + 160, None, None, &none)?;
        doc.connect(&[format!("{rl}.2"), format!("{g2}.1")], None)?;
        doc.connect(&[op, format!("{rl}.1")], Some("out"))?;
        load = json!(rl);
    }
    let mut ac = Map::new();
    ac.insert("Type".into(), json!("log"));
    ac.insert("Start".into(), json!(rftools::format_value(f_start, "Hz")));
    ac.insert("Stop".into(), json!(rftools::format_value(f_stop, "Hz")));
    ac.insert("Points".into(), json!("401"));
    let sim = doc.add_simulation("ac", &ac, None, None)?;
    Ok(json!({"source": vs, "source_resistor": rs, "load": load, "simulation": sim,
              "hint": "v(out)/v(src) is the transfer function; with equal terminations the passband level is -6 dB"}))
}

// ---------------------------------------------------------------------------
// argument parsing shared by MCP and GUI
// ---------------------------------------------------------------------------

pub fn parse_freq(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => rftools::parse_value(s),
        _ => None,
    }
}

pub fn filter_spec(a: &Value) -> Result<FilterSpec, String> {
    let s = |k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let response = match s("response").to_ascii_lowercase().as_str() {
        "" | "butterworth" => FilterResponse::Butterworth,
        "chebyshev" | "cheby" => FilterResponse::Chebyshev { ripple_db: a.get("ripple_db").and_then(|v| v.as_f64()).unwrap_or(0.5) },
        "bessel" => FilterResponse::Bessel,
        o => return Err(format!("unknown response '{o}' (butterworth, chebyshev, bessel)")),
    };
    let class = match s("class").to_ascii_lowercase().as_str() {
        "" | "lowpass" | "lp" => FilterClass::LowPass,
        "highpass" | "hp" => FilterClass::HighPass,
        "bandpass" | "bp" => FilterClass::BandPass,
        "bandstop" | "bs" | "notch" => FilterClass::BandStop,
        o => return Err(format!("unknown class '{o}' (lowpass, highpass, bandpass, bandstop)")),
    };
    Ok(FilterSpec {
        response,
        class,
        order: a.get("order").and_then(|v| v.as_u64()).unwrap_or(3) as usize,
        f1: a.get("f1").and_then(parse_freq).ok_or("f1 (corner / lower band edge) required, e.g. \"1 MHz\"")?,
        f2: a.get("f2").and_then(parse_freq),
        z0: a.get("z0").and_then(parse_freq).unwrap_or(50.0),
        first_series: a.get("first_series").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

pub fn attenuator_args(a: &Value) -> Result<(AttenuatorTopology, f64, f64, f64), String> {
    let topo = match a.get("topology").and_then(|v| v.as_str()).unwrap_or("pi").to_ascii_lowercase().as_str() {
        "pi" => AttenuatorTopology::Pi,
        "tee" | "t" => AttenuatorTopology::Tee,
        "bridgedtee" | "bridged_tee" | "bridged-t" => AttenuatorTopology::BridgedTee,
        o => return Err(format!("unknown topology '{o}' (pi, tee, bridged_tee)")),
    };
    let db = a.get("atten_db").and_then(|v| v.as_f64()).ok_or("atten_db required")?;
    let zi = a.get("z_in").and_then(parse_freq).unwrap_or(50.0);
    let zo = a.get("z_out").and_then(parse_freq).unwrap_or(zi);
    Ok((topo, db, zi, zo))
}

/// Sweep range around a filter: a decade below and above the band.
pub fn bench_range(spec: &FilterSpec) -> (f64, f64) {
    let lo = spec.f1.min(spec.f2.unwrap_or(spec.f1));
    let hi = spec.f1.max(spec.f2.unwrap_or(spec.f1));
    (lo / 10.0, hi * 10.0)
}

/// Transmission-line calculator (all lengths in mm on the interface).
pub fn tline(a: &Value) -> Result<Value, String> {
    let kind = a.get("kind").and_then(|v| v.as_str()).unwrap_or("microstrip");
    let f = a.get("freq").and_then(parse_freq).unwrap_or(0.0);
    match kind {
        "microstrip" => {
            let er = a.get("er").and_then(|v| v.as_f64()).unwrap_or(4.4);
            let h = a.get("h_mm").and_then(|v| v.as_f64()).unwrap_or(1.6) * 1e-3;
            let t = a.get("t_um").and_then(|v| v.as_f64()).unwrap_or(35.0) * 1e-6;
            let m = rftools::Microstrip { er, h, t, freq: f };
            let w = match (a.get("w_mm").and_then(|v| v.as_f64()), a.get("z0").and_then(|v| v.as_f64())) {
                (Some(w), _) => w * 1e-3,
                (None, Some(z)) => rftools::microstrip_synthesize(&m, z),
                _ => return Err("give w_mm (analyse) or z0 (synthesise)".into()),
            };
            let (z0, eeff) = rftools::microstrip_analyze(&m, w);
            let mut o = json!({"kind": "microstrip", "er": er, "h_mm": h * 1e3, "t_um": t * 1e6, "w_mm": (w * 1e6).round() / 1e3, "z0_ohm": (z0 * 100.0).round() / 100.0, "eps_eff": (eeff * 1000.0).round() / 1000.0});
            if f > 0.0 {
                if let Some(l) = a.get("length_mm").and_then(|v| v.as_f64()) {
                    o["electrical_length_deg"] = json!(rftools::electrical_length_deg(l * 1e-3, f, eeff));
                }
                if let Some(deg) = a.get("electrical_deg").and_then(|v| v.as_f64()) {
                    o["length_mm"] = json!(rftools::physical_length(deg, f, eeff) * 1e3);
                }
                o["quarter_wave_mm"] = json!(rftools::physical_length(90.0, f, eeff) * 1e3);
            }
            Ok(o)
        }
        "coax" => {
            let er = a.get("er").and_then(|v| v.as_f64()).unwrap_or(2.25);
            let d = a.get("d_inner_mm").and_then(|v| v.as_f64()).ok_or("d_inner_mm required")?;
            let dd = a.get("d_outer_mm").and_then(|v| v.as_f64()).ok_or("d_outer_mm required")?;
            let z0 = rftools::coax_z0(er, d, dd);
            let mut o = json!({"kind": "coax", "er": er, "z0_ohm": (z0 * 100.0).round() / 100.0, "velocity_factor": 1.0 / er.sqrt()});
            if f > 0.0 {
                o["quarter_wave_mm"] = json!(rftools::physical_length(90.0, f, er) * 1e3);
            }
            Ok(o)
        }
        o => Err(format!("unknown line kind '{o}' (microstrip, coax)")),
    }
}
