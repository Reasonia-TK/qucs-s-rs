//! Hierarchy: turn a selection into a subcircuit file (Qucs `Sub` + `Port`s) and
//! find the file behind a `Sub` component.

use crate::doc::{snap, Doc};
use crate::schematic::Schematic;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Absolute path of the schematic a `Sub` component refers to.
pub fn sub_file(doc: &Doc, comp: &str) -> Result<PathBuf, String> {
    let c = doc.sch.component(comp).ok_or_else(|| format!("no component '{comp}'"))?;
    if c.typ != "Sub" {
        return Err(format!("{comp} is not a subcircuit (type {})", c.typ));
    }
    let f = c.props.first().map(|p| p.value.trim().to_string()).unwrap_or_default();
    if f.is_empty() {
        return Err(format!("{comp} has no File property"));
    }
    let p = Path::new(&f);
    let abs = if p.is_absolute() { p.to_path_buf() } else { doc.dir().ok_or("save the schematic first")?.join(p) };
    if !abs.exists() {
        return Err(format!("subcircuit file not found: {}", abs.display()));
    }
    Ok(abs)
}

enum NetKind {
    Internal,
    Ground,
    /// connects to the rest of the circuit: becomes port `k`
    Port(usize),
}

/// Move the components `names` into a new subcircuit schematic `file` (relative to
/// the document) and put a `Sub` component in their place, wired to the same nets.
/// The caller takes the undo checkpoint.
pub fn create_subcircuit(doc: &mut Doc, names: &[String], file: &str, overwrite: bool) -> Result<Value, String> {
    let dir = doc.dir().ok_or("save the schematic first (the subcircuit file is stored next to it)")?;
    let mut rel = file.trim().replace('\\', "/");
    if rel.is_empty() {
        return Err("file name required".into());
    }
    if !rel.to_ascii_lowercase().ends_with(".sch") {
        rel.push_str(".sch");
    }
    let path = if Path::new(&rel).is_absolute() { PathBuf::from(&rel) } else { dir.join(&rel) };
    if doc.path.as_ref().map(|p| p == &path).unwrap_or(false) {
        return Err("the subcircuit file must differ from the schematic itself".into());
    }
    if path.exists() && !overwrite {
        return Err(format!("{} exists (pass overwrite)", path.display()));
    }
    let conn = doc.conn();
    let mut sel: HashSet<usize> = HashSet::new();
    for n in names {
        let ci = doc.sch.component_index(n).ok_or_else(|| format!("no component '{n}'"))?;
        let c = &doc.sch.components[ci];
        if c.typ.starts_with('.') || conn.placed[ci].pins.is_empty() && c.typ != "GND" {
            return Err(format!("{n} ({}) cannot go into a subcircuit", c.typ));
        }
        sel.insert(ci);
    }
    if sel.iter().all(|&ci| doc.sch.components[ci].typ == "GND") {
        return Err("select at least one component".into());
    }

    // ---- classify the nets touched by the selection
    let mut touched: Vec<usize> = Vec::new();
    for p in conn.pins.iter().filter(|p| sel.contains(&p.comp)) {
        if !touched.contains(&p.net) {
            touched.push(p.net);
        }
    }
    let is_ext = |pi: usize| !sel.contains(&conn.pins[pi].comp);
    let mut port_nets: Vec<usize> = touched
        .iter()
        .copied()
        .filter(|&n| !conn.nets[n].name.eq_ignore_ascii_case("gnd") && conn.nets[n].pins.iter().any(|&p| is_ext(p)))
        .collect();
    // ports top to bottom, then left to right (by the selected pins of the net)
    let key = |n: usize| {
        let ps: Vec<&crate::netlist::PinRef> = conn.nets[n].pins.iter().map(|&p| &conn.pins[p]).filter(|p| sel.contains(&p.comp)).collect();
        let k = ps.len().max(1) as i64;
        (ps.iter().map(|p| p.y as i64).sum::<i64>() / k, ps.iter().map(|p| p.x as i64).sum::<i64>() / k)
    };
    port_nets.sort_by_key(|&n| key(n));
    let kind = |n: usize| -> NetKind {
        if let Some(k) = port_nets.iter().position(|&m| m == n) {
            NetKind::Port(k)
        } else if conn.nets[n].name.eq_ignore_ascii_case("gnd") {
            NetKind::Ground
        } else {
            NetKind::Internal
        }
    };

    // ---- the subcircuit schematic
    let mut bb = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for &ci in &sel {
        let b = conn.placed[ci].bbox;
        bb = [bb[0].min(b[0] as i32), bb[1].min(b[1] as i32), bb[2].max(b[2] as i32), bb[3].max(b[3] as i32)];
    }
    let (dx, dy) = (snap(200 - bb[0]), snap(120 - bb[1]));
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "sub".into());
    let mut sub = Doc::new(Schematic::new_empty(&stem), Some(path.clone()));
    let mut order: Vec<usize> = sel.iter().copied().collect();
    order.sort();
    for &ci in &order {
        let mut c = doc.sch.components[ci].clone();
        c.x += dx;
        c.y += dy;
        sub.sch.components.push(c);
    }
    // wires of islands that only reach selected pins come along unchanged
    let mut moved_wires: HashSet<usize> = HashSet::new();
    let mut mixed_islands: Vec<usize> = Vec::new();
    for (ii, isl) in conn.islands.iter().enumerate() {
        let has_sel = isl.pins.iter().any(|&p| !is_ext(p));
        if !has_sel {
            continue;
        }
        if isl.pins.iter().all(|&p| !is_ext(p)) {
            for &wi in &isl.wires {
                let w = &doc.sch.wires[wi];
                let mut nw = w.clone();
                nw.x1 += dx;
                nw.x2 += dx;
                nw.y1 += dy;
                nw.y2 += dy;
                if !nw.label.is_empty() {
                    nw.lx += dx;
                    nw.ly += dy;
                }
                sub.sch.wires.push(nw);
                moved_wires.insert(wi);
            }
        } else {
            mixed_islands.push(ii);
        }
    }
    let sel_pins_of_net = |n: usize| -> Vec<String> { conn.nets[n].pins.iter().filter(|&&p| !is_ext(p)).map(|&p| conn.pins[p].label.clone()).collect() };
    let mut sub_report = Vec::new();
    let mid_x = (bb[0] + bb[2]) / 2;
    let (mut yl, mut yr) = (bb[1] + dy, bb[1] + dy);
    let mut ports = Vec::new();
    for (k, &n) in port_nets.iter().enumerate() {
        let (ky, kx) = key(n);
        let left = (kx as i32) <= mid_x;
        let (x, y) = if left {
            let p = (snap(bb[0] + dx - 70), snap((ky as i32 + dy).max(yl)));
            yl = p.1 + 40;
            p
        } else {
            let p = (snap(bb[2] + dx + 70), snap((ky as i32 + dy).max(yr)));
            yr = p.1 + 40;
            p
        };
        let mut props = Map::new();
        props.insert("Num".into(), json!((k + 1).to_string()));
        let pname = format!("P{}", k + 1);
        let pname = if sub.sch.component(&pname).is_some() { sub.unique("P") } else { pname };
        sub.add_component("Port", Some(&pname), x, y, Some(if left { 0 } else { 2 }), None, &props)?;
        let mut terms = vec![format!("{pname}.1")];
        terms.extend(sel_pins_of_net(n));
        sub_report.push(sub.connect(&terms, None)?);
        ports.push(json!({"num": k + 1, "net": conn.nets[n].name, "port": pname}));
    }
    // ground and internal nets whose wiring was shared with the outside
    for &ii in &mixed_islands {
        let isl = &conn.islands[ii];
        let n = isl.net;
        let mut terms: Vec<String> = isl.pins.iter().filter(|&&p| !is_ext(p)).map(|&p| conn.pins[p].label.clone()).collect();
        match kind(n) {
            NetKind::Port(_) => continue,
            NetKind::Ground => {
                let first = isl.pins.iter().find(|&&p| !is_ext(p)).map(|&p| &conn.pins[p]);
                if let Some(p) = first {
                    let (gx, gy) = (snap(p.x + dx + p.dir.0 * 30), snap(p.y + dy + p.dir.1.max(0) * 30 + 30));
                    let g = sub.add_component("GND", None, gx, gy, None, None, &Map::new())?;
                    terms.push(format!("{g}.1"));
                }
            }
            NetKind::Internal => {}
        }
        if terms.len() >= 2 {
            sub_report.push(sub.connect(&terms, None)?);
        }
    }
    sub.save()?;

    // ---- the parent schematic
    // external terminals of every mixed island (to be re-wired), with their labels
    struct Rewire {
        terms: Vec<String>,
        labels: Vec<String>,
        port: Option<usize>,
    }
    let mut rewires: Vec<Rewire> = Vec::new();
    let mut drop: HashSet<usize> = moved_wires.clone();
    for &ii in &mixed_islands {
        let isl = &conn.islands[ii];
        drop.extend(isl.wires.iter().copied());
        let terms: Vec<String> = isl.pins.iter().filter(|&&p| is_ext(p)).map(|&p| conn.pins[p].label.clone()).collect();
        let port = match kind(isl.net) {
            NetKind::Port(k) => Some(k),
            _ => None,
        };
        rewires.push(Rewire { terms, labels: isl.labels.clone(), port });
    }
    // ports whose net reaches the selection only through other islands (labels)
    for (k, &n) in port_nets.iter().enumerate() {
        if !rewires.iter().any(|r| r.port == Some(k)) {
            rewires.push(Rewire { terms: vec![], labels: vec![conn.nets[n].name.clone()], port: Some(k) });
        }
    }
    let mut k = 0;
    doc.sch.wires.retain(|_| {
        let keep = !drop.contains(&k);
        k += 1;
        keep
    });
    let sel_names: Vec<String> = order.iter().map(|&ci| doc.sch.components[ci].name.clone()).collect();
    doc.delete_components(&sel_names)?;
    let mut props = Map::new();
    props.insert("File".into(), json!(rel));
    let cx = snap((bb[0] + bb[2]) / 2);
    let cy = snap((bb[1] + bb[3]) / 2);
    let sub_name = doc.add_component("Sub", None, cx, cy, Some(0), None, &props)?;
    let mut report = Vec::new();
    // one wire tree per port (all its islands), then the other islands
    let mut by_port: BTreeMap<usize, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for r in &rewires {
        if let Some(k) = r.port {
            let e = by_port.entry(k).or_default();
            e.0.extend(r.terms.iter().cloned());
            e.1.extend(r.labels.iter().cloned());
        }
    }
    for (k, (terms, labels)) in by_port {
        let mut t = vec![format!("{sub_name}.{}", k + 1)];
        t.extend(terms);
        let mut labels: Vec<String> = labels.into_iter().filter(|l| !l.is_empty()).collect();
        labels.dedup();
        if t.len() >= 2 {
            report.push(doc.connect(&t, None)?);
        }
        for l in labels {
            let _ = doc.add_net_label(&t[0], &l);
        }
    }
    for r in rewires.iter().filter(|r| r.port.is_none()) {
        if r.terms.len() >= 2 {
            report.push(doc.connect(&r.terms, None)?);
        }
        if let Some(t) = r.terms.first() {
            for l in r.labels.iter().filter(|l| !l.is_empty()) {
                let _ = doc.add_net_label(t, l);
            }
        }
    }
    Ok(json!({
        "subcircuit_file": path.display().to_string(),
        "component": sub_name,
        "ports": ports,
        "moved": sel_names,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn divider(dir: &Path) -> Doc {
        let p = dir.join("top.sch");
        let mut d = Doc::new(Schematic::new_empty("top"), Some(p));
        let none = Map::new();
        d.add_component("Vdc", Some("V1"), 0, 100, None, None, &none).unwrap();
        d.add_component("R", Some("R1"), 150, 40, None, None, &none).unwrap();
        d.add_component("R", Some("R2"), 260, 120, Some(1), None, &none).unwrap();
        d.add_component("R", Some("RL"), 400, 120, Some(1), None, &none).unwrap();
        d.add_component("GND", None, 0, 240, None, None, &none).unwrap();
        d.connect(&["V1.+".into(), "R1.1".into()], Some("in")).unwrap();
        d.connect(&["R1.2".into(), "R2.2".into(), "RL.2".into()], Some("out")).unwrap();
        d.connect(&["V1.-".into(), "R2.1".into(), "RL.1".into(), "GND1.1".into()], None).unwrap();
        d.save().unwrap();
        d
    }

    #[test]
    fn selection_becomes_subcircuit() {
        let dir = std::env::temp_dir().join(format!("qucs-rs-hier-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = divider(&dir);
        let before = d.conn();
        let net_of = |c: &crate::netlist::Connectivity, pin: &str| c.nets[c.find_pin(pin).unwrap().net].name.clone();
        assert_eq!(net_of(&before, "RL.2"), "out");
        let r = create_subcircuit(&mut d, &["R1".into(), "R2".into()], "div", false).unwrap();
        assert_eq!(r["ports"].as_array().unwrap().len(), 2, "{r}");
        // the parent: R1/R2 gone, a Sub with two pins on 'in' and 'out'
        assert!(d.sch.component("R1").is_none());
        let s = r["component"].as_str().unwrap().to_string();
        let conn = d.conn();
        let nets: Vec<String> = (1..=2).map(|k| net_of(&conn, &format!("{s}.{k}"))).collect();
        assert!(nets.contains(&net_of(&conn, "V1.+")), "{nets:?}");
        assert!(nets.contains(&net_of(&conn, "RL.2")), "{nets:?}");
        assert_eq!(net_of(&conn, "RL.2"), "out");
        // the subcircuit: R1, R2, two ports, ground
        let sub = Schematic::load(&dir.join("div.sch")).unwrap();
        assert!(sub.component("R1").is_some() && sub.component("R2").is_some());
        assert_eq!(sub.components.iter().filter(|c| c.typ == "Port").count(), 2);
        let sconn = crate::netlist::Connectivity::build(&sub, Some(&dir));
        assert_eq!(net_of(&sconn, "R2.1"), "gnd");
        assert_eq!(net_of(&sconn, "R1.2"), net_of(&sconn, "R2.2"));
        // and it netlists hierarchically
        let n = crate::spice::build(&d.sch, &conn, "t", Some(&dir), crate::spice::Mode::OperatingPoint).unwrap();
        assert!(n.text.contains(".SUBCKT Sub_div"), "{}", n.text);
        assert!(n.text.lines().any(|l| l.starts_with(&format!("X{s} ")) && l.ends_with("Sub_div")), "{}", n.text);
        assert_eq!(sub_file(&d, &s).unwrap(), dir.join("div.sch"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
