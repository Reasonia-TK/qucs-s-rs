//! Unit tests of the core: .sch parsing / writing, connectivity rules, editing
//! operations with undo, and the router's layout guarantees.

use crate::check;
use crate::doc::{normalize_wires, snap, split_wires_at, Doc};
use crate::netlist::Connectivity;
use crate::schematic::{tokenize, unquote, Schematic, Wire};
use serde_json::Map;
use std::collections::HashSet;

const MINI: &str = "<Qucs Schematic 24.4.0>
<Properties>
  <View=0,0,800,600,1,0,0>
</Properties>
<Symbol>
</Symbol>
<Components>
  <R R1 1 100 100 -26 15 0 0 \"10 k\" 1 \"26.85\" 0 \"0.0\" 0 \"0.0\" 0 \"26.85\" 0 \"european\" 0>
  <GND * 1 200 200 0 0 0 0>
</Components>
<Wires>
  <130 100 200 100 \"out\" 160 70 0 \"\">
  <200 100 200 200 \"\" 0 0 0 \"\">
</Wires>
<Diagrams>
</Diagrams>
<Paintings>
  <Text 40 300 12 #000000 0 \"hello world\">
</Paintings>
<FutureSection>
  keep me
</FutureSection>
";

fn none() -> Map<String, serde_json::Value> {
    Map::new()
}

fn net_of(c: &Connectivity, pin: &str) -> String {
    c.nets[c.find_pin(pin).unwrap_or_else(|| panic!("no pin {pin}")).net].name.clone()
}

// ------------------------------------------------------------------ schematic

#[test]
fn parse_and_write_keep_everything() {
    let s = Schematic::parse(MINI).unwrap();
    assert_eq!(s.version, "24.4.0");
    assert_eq!(s.components.len(), 2);
    assert_eq!(s.components[0].props[0].value, "10 k");
    assert_eq!(s.wires[0].label, "out");
    assert_eq!(s.paintings.len(), 1);
    assert_eq!(s.extra_sections, vec![("FutureSection".to_string(), vec!["  keep me".to_string()])]);
    let again = Schematic::parse(&s.serialize()).unwrap();
    assert_eq!(again.components, s.components);
    assert_eq!(again.wires, s.wires);
    assert_eq!(again.paintings, s.paintings);
    assert_eq!(again.extra_sections, s.extra_sections);
}

#[test]
fn rejects_non_schematics() {
    assert!(Schematic::parse("").is_err());
    assert!(Schematic::parse("hello\n<Components>\n</Components>").is_err());
}

#[test]
fn tokenizer_keeps_quoted_spaces() {
    assert_eq!(tokenize(r#"R R1 1 "10 k" 1 "" 0"#), vec!["R", "R1", "1", "\"10 k\"", "1", "\"\"", "0"]);
    assert_eq!(unquote("\"a b\""), "a b");
    assert_eq!(unquote("x"), "x");
}

#[test]
fn grounds_get_unique_hidden_names() {
    let d = Doc::new(Schematic::parse(MINI).unwrap(), None);
    let g = d.sch.components.iter().find(|c| c.typ == "GND").unwrap();
    assert_ne!(g.name, "*");
    assert!(g.active & 4 != 0, "name stays hidden like Qucs' '*'");
}

// ------------------------------------------------------------------ connectivity

#[test]
fn labels_and_ground_define_nets() {
    let d = Doc::new(Schematic::parse(MINI).unwrap(), None);
    let c = d.conn();
    // R1.2 touches the labelled wire, which reaches the ground symbol
    assert_eq!(net_of(&c, "R1.2"), "gnd");
    // R1.1 is not connected to anything: an auto-named net of its own
    assert!(net_of(&c, "R1.1").starts_with("_net"));
}

#[test]
fn pin_in_the_middle_of_a_wire_is_not_connected() {
    let mut s = Schematic::new_empty("t");
    s.components.push(crate::library::new_component("R", "R1", 100, 100).unwrap()); // pins at 70 and 130
    s.wires.push(Wire::new(0, 100, 200, 100)); // passes through both pins
    let c = Connectivity::build(&s, None);
    let wnet = c.wire_net[0];
    assert_ne!(c.find_pin("R1.1").unwrap().net, wnet);
    assert_ne!(c.find_pin("R1.2").unwrap().net, wnet);
    // after splitting the wire at the pins they are connected (what the router does)
    let mut s2 = s.clone();
    s2.wires = split_wires_at(split_wires_at(s2.wires, (70, 100)), (130, 100));
    let c2 = Connectivity::build(&s2, None);
    assert_eq!(c2.find_pin("R1.1").unwrap().net, c2.find_pin("R1.2").unwrap().net);
}

#[test]
fn same_label_joins_separate_islands() {
    let mut s = Schematic::new_empty("t");
    s.components.push(crate::library::new_component("R", "R1", 100, 100).unwrap());
    s.components.push(crate::library::new_component("R", "R2", 400, 100).unwrap());
    let mut a = Wire::new(130, 100, 180, 100);
    a.label = "mid".into();
    let mut b = Wire::new(320, 100, 370, 100);
    b.label = "mid".into();
    s.wires = vec![a, b];
    let c = Connectivity::build(&s, None);
    assert_eq!(net_of(&c, "R1.2"), "mid");
    assert_eq!(net_of(&c, "R2.1"), "mid");
    assert_eq!(c.find_pin("R1.2").unwrap().net, c.find_pin("R2.1").unwrap().net);
    assert_ne!(c.find_pin("R1.2").unwrap().island, c.find_pin("R2.1").unwrap().island);
}

#[test]
fn normalize_merges_collinear_wires_but_keeps_pins() {
    let pins: HashSet<(i32, i32)> = [(50, 0)].into_iter().collect();
    let w = vec![Wire::new(0, 0, 50, 0), Wire::new(50, 0, 100, 0), Wire::new(100, 0, 150, 0)];
    let out = normalize_wires(w, &pins);
    // 0..50 stays separate (a pin sits at 50), 50..150 merges
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out.iter().any(|w| (w.x1.min(w.x2), w.x1.max(w.x2)) == (50, 150)));
}

#[test]
fn snapping() {
    assert_eq!(snap(14), 10);
    assert_eq!(snap(15), 20);
    assert_eq!(snap(-14), -10);
}

// ------------------------------------------------------------------ editing

fn rc_doc() -> Doc {
    let mut d = Doc::new(Schematic::new_empty("t"), None);
    d.add_component("Vdc", Some("V1"), 0, 100, None, None, &none()).unwrap();
    d.add_component("R", Some("R1"), 150, 40, None, None, &none()).unwrap();
    d.add_component("C", Some("C1"), 260, 120, Some(1), None, &none()).unwrap();
    d.add_component("GND", None, 0, 220, None, None, &none()).unwrap();
    d.connect(&["V1.+".into(), "R1.1".into()], Some("in")).unwrap();
    d.connect(&["R1.2".into(), "C1.2".into()], Some("out")).unwrap();
    d.connect(&["V1.-".into(), "C1.1".into(), "GND1.1".into()], None).unwrap();
    d
}

#[test]
fn duplicate_names_are_refused() {
    let mut d = rc_doc();
    assert!(d.add_component("R", Some("R1"), 500, 500, None, None, &none()).is_err());
    let n = d.add_component("R", None, 500, 500, None, None, &none()).unwrap();
    assert_eq!(n, "R2");
}

#[test]
fn undo_redo_restores_exact_states() {
    let mut d = rc_doc();
    let before = d.sch.clone();
    d.checkpoint();
    d.delete_components(&["R1".into()]).unwrap();
    assert!(d.sch.component("R1").is_none());
    assert!(d.dirty);
    assert!(d.undo());
    assert_eq!(d.sch.components, before.components);
    assert_eq!(d.sch.wires, before.wires);
    assert!(d.redo());
    assert!(d.sch.component("R1").is_none());
    assert!(!d.redo(), "nothing left to redo");
}

#[test]
fn moving_parts_reroutes_and_keeps_nets() {
    let mut d = rc_doc();
    let nets_before: Vec<String> = ["R1.1", "R1.2", "C1.1", "C1.2"].iter().map(|p| net_of(&d.conn(), p)).collect();
    d.move_components(&[("C1".into(), 400, 260)], true).unwrap();
    let c = d.conn();
    let nets_after: Vec<String> = ["R1.1", "R1.2", "C1.1", "C1.2"].iter().map(|p| net_of(&c, p)).collect();
    assert_eq!(nets_before, nets_after);
    let rep = check::check(&d.sch, &c);
    assert_eq!(rep.metrics["errors"], 0, "{:?}", rep.issues);
}

#[test]
fn tidy_is_idempotent() {
    let mut d = rc_doc();
    d.tidy(true, false).unwrap();
    let once = d.sch.clone();
    d.tidy(true, false).unwrap();
    assert_eq!(d.sch.wires, once.wires);
    assert_eq!(d.sch.components, once.components);
}

#[test]
fn reroute_everything_is_clean() {
    let mut d = rc_doc();
    d.reroute(&[]).unwrap();
    let c = d.conn();
    assert_eq!(net_of(&c, "R1.2"), "out");
    assert_eq!(net_of(&c, "C1.1"), "gnd");
    let rep = check::check(&d.sch, &c);
    assert_eq!(rep.metrics["errors"], 0, "{:?}", rep.issues);
}

// ------------------------------------------------------------------ router guarantees

/// A grid of parts wired across each other: the router must not cross symbols,
/// must not touch foreign pins and must keep every wire orthogonal.
#[test]
fn router_avoids_symbols_and_foreign_nets() {
    let mut d = Doc::new(Schematic::new_empty("t"), None);
    let mut names = Vec::new();
    for row in 0..3 {
        for col in 0..3 {
            let n = format!("R{}", row * 3 + col + 1);
            d.add_component("R", Some(&n), 100 + col * 160, 100 + row * 140, Some((row + col) % 2), None, &none()).unwrap();
            names.push(n);
        }
    }
    // corner to corner and across the middle
    let nets = [["R1.1", "R9.2"], ["R3.2", "R7.1"], ["R2.1", "R8.2"], ["R4.1", "R6.2"]];
    for n in nets {
        d.connect(&[n[0].into(), n[1].into()], None).unwrap();
    }
    let c = d.conn();
    for n in nets {
        assert_eq!(c.find_pin(n[0]).unwrap().net, c.find_pin(n[1]).unwrap().net, "{n:?} not connected");
    }
    // the four nets stay distinct
    let ids: HashSet<usize> = nets.iter().map(|n| c.find_pin(n[0]).unwrap().net).collect();
    assert_eq!(ids.len(), 4);
    assert!(d.sch.wires.iter().all(|w| w.is_horizontal() || w.is_vertical()));
    let rep = check::check(&d.sch, &c);
    assert_eq!(rep.metrics["errors"], 0, "{:?}", rep.issues);
    // no wire runs through a symbol body
    for w in &d.sch.wires {
        for (ci, p) in c.placed.iter().enumerate() {
            let b = p.bbox;
            let inside = |x: f64, y: f64| x > b[0] + 2.0 && x < b[2] - 2.0 && y > b[1] + 2.0 && y < b[3] - 2.0;
            let mid = ((w.x1 + w.x2) as f64 / 2.0, (w.y1 + w.y2) as f64 / 2.0);
            assert!(!inside(mid.0, mid.1), "wire {w:?} crosses {}", d.sch.components[ci].name);
        }
    }
}

#[test]
fn unsaved_copy_shares_the_work_dir_of_its_original() {
    let orig = std::path::Path::new("C:/x/amp.sch");
    let tmp = crate::sim::unsaved_copy_path(orig, "sim").unwrap();
    assert_eq!(tmp.parent(), orig.parent());
    assert_ne!(tmp, orig);
    assert_eq!(crate::sim::work_dir(&tmp, "sim").unwrap(), crate::sim::work_dir(orig, "sim").unwrap());
}
