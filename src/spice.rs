//! Built-in ngspice netlister. It follows Qucs-S' own translation rules
//! (`spice_netlist()` of each component, `Ngspice::createNetlist`) so the result
//! matches `qucs-s -n --ngspice` closely, but needs no Qucs-S installation.
//!
//! Components it does not know are reported, and `sim::netlist` then falls back
//! to qucs-s when it is installed.

use crate::library::lib;
use crate::netlist::Connectivity;
use crate::schematic::{Component, Schematic};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// What the netlist should run.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// The simulations placed on the schematic.
    Simulations,
    /// Only a DC operating point, printing every node voltage and branch current.
    OperatingPoint,
}

pub struct Netlist {
    pub text: String,
    /// SPICE node name of every net of the top-level schematic (index = net index).
    pub node_names: Vec<String>,
}

pub const OP_OUTPUT: &str = "spice4qucs.op.ngspice.dc.print";

/// Analysis types the built-in netlister translates.
pub const SUPPORTED_SIMS: [&str; 3] = [".AC", ".TR", ".DC"];
/// Component types the built-in netlister translates.
pub const SUPPORTED_COMPONENTS: [&str; 29] = [
    "R", "C", "L", "GND", "Vdc", "Idc", "Vac", "Iac", "Vpulse", "Ipulse", "Vrect", "Irect", "Diode", "_BJT", "_MOSFET", "JFET", "OpAmp", "VCVS", "VCCS", "CCVS", "CCCS", "IProbe", "VProbe",
    "S4Q_Ieqndef", "src_eqndef", "S4Q_V", "S4Q_I", "Sub", "Port",
];
/// Whether the built-in netlister translates this component or simulation type.
pub fn is_supported(typ: &str) -> bool {
    SUPPORTED_COMPONENTS.contains(&typ) || SUPPORTED_SIMS.contains(&typ) || PASSIVE_BLOCKS.contains(&typ)
}

/// Non-electrical blocks that are understood (or safely ignored).
const PASSIVE_BLOCKS: [&str; 3] = ["NutmegEq", "SpicePar", "GND"];

fn is_active(c: &Component) -> bool {
    c.active & 3 == 1
}

/// `spicecompat::normalize_value`: drop units, M -> Meg, wrap variables in braces.
pub fn normalize_value(v: &str) -> String {
    let s: String = v.chars().filter(|c| *c != ' ').collect();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s;
    }
    let digit = s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
    // same order as the regular expressions in Qucs-S
    let unit = ["Ohm", "F", "H", "V", "A", "Hz", "S", "s"].into_iter().find(|u| digit && s.len() > u.len() && s.ends_with(u));
    let signed_digit = s.trim_start_matches(['+', '-']).chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
    let out = if let Some(u) = unit {
        s[..s.len() - u.len()].replace('M', "Meg")
    } else if signed_digit && s.ends_with("dBm") {
        s[..s.len() - 3].to_string()
    } else if s.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
        format!("{{{s}}}")
    } else {
        s.clone()
    };
    out.to_uppercase()
}

/// Number formatting like Qt's `QString::arg(double)` (`%g`, 6 significant digits).
pub fn gnum(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if !x.is_finite() {
        return x.to_string();
    }
    let exp = x.abs().log10().floor() as i32;
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        let s = format!("{x:.decimals$}");
        let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
        s
    } else {
        let s = format!("{x:.5e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
        let e: i32 = e.parse().unwrap_or(0);
        format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    }
}

fn num(v: &str) -> f64 {
    crate::rftools::parse_value(v).unwrap_or(0.0)
}

/// Property names of a component (by position).
fn prop_names(c: &Component) -> Vec<String> {
    (0..c.props.len()).map(|i| crate::library::prop_name(c, i)).collect()
}

/// Property value by name; the library default when the file does not have it.
fn prop(c: &Component, name: &str) -> String {
    let info = lib().info(&c.typ);
    if let Some(info) = info {
        if let Some(k) = info.props.iter().position(|p| p.name == name) {
            if let Some(p) = c.props.get(k) {
                return p.value.clone();
            }
            return info.props[k].default.clone();
        }
    }
    String::new()
}

fn prop_at(c: &Component, i: usize) -> String {
    c.props.get(i).map(|p| p.value.clone()).unwrap_or_else(|| lib().info(&c.typ).and_then(|i2| i2.props.get(i)).map(|p| p.default.clone()).unwrap_or_default())
}

/// `check_refdes`: prefix the SPICE letter unless the name already starts with it.
fn refdes(name: &str, letter: &str) -> String {
    if name.len() >= letter.len() && name[..letter.len()].eq_ignore_ascii_case(letter) {
        name.to_string()
    } else {
        format!("{letter}{name}")
    }
}

/// `key=value ` for every property not in `skip`, renaming per `rename`.
fn param_list(c: &Component, skip: &[&str], rename: &[(&str, &str)], drop_zero_ikf: bool) -> String {
    let mut s = String::new();
    for (k, n) in prop_names(c).iter().enumerate() {
        if skip.contains(&n.as_str()) {
            continue;
        }
        let v = prop_at(c, k);
        if drop_zero_ikf && n == "Ikf" && v.trim() == "0" {
            continue;
        }
        let name = rename.iter().find(|r| r.0 == n).map(|r| r.1).unwrap_or(n);
        let _ = write!(s, "{name}={} ", normalize_value(&v));
    }
    s
}

struct Ctx<'a> {
    dir: Option<&'a Path>,
    depth: usize,
    /// subcircuit definitions: file -> (subckt name, text)
    subckts: Vec<(PathBuf, String, String)>,
    unsupported: BTreeSet<String>,
}

/// Netlist the components of one schematic level. `nodes[pin]` gives a pin's node.
fn components(sch: &Schematic, conn: &Connectivity, node: &dyn Fn(usize, usize) -> String, cx: &mut Ctx, out: &mut String) {
    for (ci, c) in sch.components.iter().enumerate() {
        if !is_active(c) {
            if c.active & 3 == 2 {
                cx.unsupported.insert(format!("{} (shorted component)", c.name));
            }
            continue;
        }
        let t = c.typ.as_str();
        if t.starts_with('.') || PASSIVE_BLOCKS.contains(&t) || t == "Port" || crate::library::is_text_block(t) && t != "Eqn" {
            continue;
        }
        let n = |k: usize| node(ci, k);
        let name = c.name.as_str();
        let v0 = || normalize_value(&prop_at(c, 0));
        match t {
            "R" => {
                let mut s = format!("{} {} {}  {}", refdes(name, "R"), n(0), n(1), v0());
                let tc1 = prop(c, "Tc1");
                let tc2 = prop(c, "Tc2");
                if !tc1.is_empty() {
                    let _ = write!(s, " tc1={tc1}");
                }
                if !tc2.is_empty() {
                    let _ = write!(s, " tc2={tc2}");
                }
                let _ = writeln!(out, "{s} ");
            }
            "C" | "L" => {
                let mut s = format!("{} {} {}  {} ", refdes(name, t), n(0), n(1), v0());
                let ic: String = prop_at(c, 1).chars().filter(|c| *c != ' ').collect::<String>().to_uppercase();
                if !ic.is_empty() {
                    let _ = write!(s, " IC={ic}");
                }
                let _ = writeln!(out, "{s}");
            }
            "Vdc" => {
                let _ = writeln!(out, "{} {} {} DC {}", refdes(name, "V"), n(0), n(1), v0());
            }
            "Idc" => {
                let _ = writeln!(out, "{} {} {} DC {}", refdes(name, "I"), n(1), n(0), v0());
            }
            "Vac" | "Iac" => {
                let (letter, a, b, off) = if t == "Vac" { ("V", n(0), n(1), "VO") } else { ("I", n(1), n(0), "IO") };
                let amp = v0();
                let freq = normalize_value(&prop_at(c, 1));
                let nz = |s: String| if s.is_empty() { "0".to_string() } else { s };
                let phase = nz(prop_at(c, 2).replace(' ', ""));
                let theta = nz(prop_at(c, 3).replace(' ', ""));
                let o = normalize_value(&prop(c, off));
                let td = normalize_value(&prop(c, "TD"));
                let _ = writeln!(out, "{} {} {} DC {o} SIN({o} {amp} {freq} {td} {theta} {phase}) AC {amp} ACPHASE {phase}", refdes(name, letter), a, b);
            }
            "Vpulse" | "Ipulse" => {
                let (letter, a, b) = if t == "Vpulse" { ("V", n(0), n(1)) } else { ("I", n(1), n(0)) };
                let lo = v0();
                let hi = normalize_value(&prop_at(c, 1));
                let tr = normalize_value(&prop_at(c, 4));
                let tf = normalize_value(&prop_at(c, 5));
                let t1 = normalize_value(&prop(c, "T1"));
                let t2 = normalize_value(&prop(c, "T2"));
                let _ = writeln!(out, "{} {a} {b} DC 0 PULSE({lo} {hi} {t1} {tr} {tf} {{({t2})-({t1})-({tr})-({tf})}}) AC 0", refdes(name, letter));
            }
            "Vrect" => {
                let u = v0();
                let td = normalize_value(&prop_at(c, 5));
                let tr = normalize_value(&prop_at(c, 3));
                let tf = normalize_value(&prop_at(c, 4));
                let u0 = normalize_value(&prop(c, "U0"));
                let th = normalize_value(&prop(c, "TH"));
                let tl = normalize_value(&prop(c, "TL"));
                let _ = writeln!(out, "{} {} {} DC 0 PULSE( {u0} {u} {td} {tr} {tf} {th} {{({th})+({tl})+({tr})+({tf})}} )  AC 0", refdes(name, "V"), n(0), n(1));
            }
            "Irect" => {
                let u = v0();
                let td = normalize_value(&prop_at(c, 5));
                let tr = normalize_value(&prop_at(c, 3));
                let tf = normalize_value(&prop_at(c, 4));
                let th = num(&prop_at(c, 1));
                let per = num(&prop_at(c, 2)) + th + num(&prop_at(c, 3)) + num(&prop_at(c, 4));
                let _ = writeln!(out, "{} {} {} DC 0 PULSE( 0  -{u} {td} {tr} {tf} {} {})  AC 0", refdes(name, "I"), n(0), n(1), gnum(th), gnum(per));
            }
            "Diode" => {
                let skip = ["Cp", "Isr", "Nr", "Ffe", "Temp", "Area", "Symbol", "UseGlobTemp", "LibName", "CompName"];
                let par = param_list(c, &skip, &[("Tbv", "Tcv")], true);
                let mut s = format!("{} {} {}", refdes(name, "D"), n(1), n(0));
                if prop(c, "UseGlobTemp") == "yes" {
                    let _ = writeln!(s, " DMOD_{name} AREA={}", prop(c, "Area"));
                } else {
                    let _ = writeln!(s, " DMOD_{name} AREA={} Temp={}", prop(c, "Area"), prop(c, "Temp"));
                }
                let _ = writeln!(s, ".MODEL DMOD_{name} D ({par})");
                out.push_str(&s);
            }
            "_BJT" => {
                let skip = ["Type", "Area", "Temp", "Ffe", "Kb", "Ab", "Fb", "UseGlobTemp", "LibName", "CompName"];
                let par = param_list(c, &skip, &[], false);
                let mut s = format!("{} {} {} {}", refdes(name, "Q"), n(1), n(0), n(2));
                if prop(c, "UseGlobTemp") == "yes" {
                    let _ = writeln!(s, " QMOD_{name} AREA={}", prop(c, "Area"));
                } else {
                    let _ = writeln!(s, " QMOD_{name} AREA={} TEMP={}", prop(c, "Area"), prop(c, "Temp"));
                }
                let _ = writeln!(s, ".MODEL QMOD_{name} {} ({par})", prop(c, "Type"));
                out.push_str(&s);
            }
            "_MOSFET" => {
                let mut skip = vec!["Type", "Temp", "L", "W", "Ad", "As", "Pd", "Ps", "Rg", "N", "Tt", "Nrd", "Nrs", "Ffe", "UseGlobTemp", "LibName", "CompName"];
                for p in ["Nsub", "Nss"] {
                    if num(&prop(c, p)) == 0.0 {
                        skip.push(p);
                    }
                }
                let par = param_list(c, &skip, &[], false);
                let typ = prop(c, "Type").chars().next().map(|ch| ch.to_ascii_uppercase()).unwrap_or('N');
                let g = |p: &str| normalize_value(&prop(c, p));
                let mut s = format!("{} {} {} {} {}", refdes(name, "M"), n(1), n(0), n(2), n(2));
                let _ = write!(s, " MMOD_{name} L={} W={} Ad={} As={} Pd={} Ps={}", g("L"), g("W"), g("Ad"), g("As"), g("Pd"), g("Ps"));
                if prop(c, "UseGlobTemp") == "yes" {
                    s.push('\n');
                } else {
                    let _ = writeln!(s, " Temp={}", prop(c, "Temp"));
                }
                let _ = writeln!(s, ".MODEL MMOD_{name} {typ}MOS ({par})");
                out.push_str(&s);
            }
            "JFET" => {
                let skip = ["Type", "Area", "Temp", "Ffe", "N", "Isr", "Nr", "M", "Xti", "Betatce", "UseGlobTemp", "LibName", "CompName"];
                let par = param_list(c, &skip, &[("Vt0tc", "Tcv")], false);
                let typ = prop(c, "Type").chars().next().map(|ch| ch.to_ascii_uppercase()).unwrap_or('N');
                let mut s = format!("{} {} {} {}", refdes(name, "J"), n(1), n(0), n(2));
                if prop(c, "UseGlobTemp") == "yes" {
                    let _ = writeln!(s, " JMOD_{name} {}", prop(c, "Area"));
                } else {
                    let _ = writeln!(s, " JMOD_{name} {} TEMP={}", prop(c, "Area"), prop(c, "Temp"));
                }
                let _ = writeln!(s, ".MODEL JMOD_{name} {typ}JF ({par})");
                out.push_str(&s);
            }
            "OpAmp" => {
                let (im, ip, o) = (n(0), n(1), n(2));
                let g = v0();
                let vm = normalize_value(&prop_at(c, 1));
                let x = format!("{g}*V({ip},{im})");
                let _ = writeln!(out, "B_{name} {o} 0 V = {x}*u({vm}-{x})*u({x}-(-{vm}))+{vm}*u({x}-{vm})+(-{vm})*u((-{vm})-{x})");
            }
            "VCVS" | "VCCS" => {
                let letter = if t == "VCVS" { "E" } else { "G" };
                let val = if t == "VCVS" { prop_at(c, 0) } else { v0() };
                let _ = writeln!(out, "{} {} {} {} {} {val}", refdes(name, letter), n(1), n(2), n(0), n(3));
            }
            "CCVS" | "CCCS" => {
                let letter = if t == "CCVS" { "H" } else { "F" };
                let _ = writeln!(out, "{} {} {}  V{name} {}", refdes(name, letter), n(1), n(2), v0());
                let _ = writeln!(out, "V{name} {} {} DC 0", n(0), n(3));
            }
            "IProbe" => {
                let _ = writeln!(out, "V{name} {} {} DC 0", n(0), n(1));
            }
            "VProbe" => {
                let _ = writeln!(out, "E{name} {name} 0 {} {} 1.0\nR{name}{name} {name} 0 1E8\nR{name}{} {} {} 1E8", n(0), n(1), n(0), n(0), n(1));
            }
            "S4Q_Ieqndef" | "src_eqndef" => {
                let var = if t == "S4Q_Ieqndef" { "I" } else { "V" };
                let mut s = format!("{} {}  {} ", refdes(name, "B"), n(0), n(1));
                let _ = write!(s, " {var} = {} ", prop_at(c, 0));
                for k in 1..5 {
                    let l = prop_at(c, k);
                    if !l.is_empty() {
                        let _ = write!(s, "\n{l}");
                    }
                }
                let _ = writeln!(out, "{s}");
            }
            "S4Q_V" | "S4Q_I" => {
                let letter = if t == "S4Q_V" { "V" } else { "I" };
                let mut s = format!("{} {}  {} ", refdes(name, letter), n(0), n(1));
                for k in 0..5 {
                    let l = prop_at(c, k);
                    if !l.is_empty() {
                        if k > 0 {
                            s.push('\n');
                        }
                        s.push_str(&l);
                    }
                }
                let _ = writeln!(out, "{s}");
            }
            "Sub" => match subcircuit(c, cx) {
                Ok((sub_name, npins)) => {
                    let mut s = format!("X{name}");
                    for k in 0..npins {
                        let _ = write!(s, " {}", n(k));
                    }
                    let _ = writeln!(out, "{s} {sub_name}");
                }
                Err(e) => {
                    cx.unsupported.insert(e);
                }
            },
            _ => {
                cx.unsupported.insert(t.to_string());
            }
        }
    }
    let _ = conn;
}

/// SPICE node name for every net.
fn node_names(conn: &Connectivity) -> Vec<String> {
    conn.nets.iter().map(|n| if n.name.eq_ignore_ascii_case("gnd") { "0".to_string() } else { n.name.split_whitespace().collect::<Vec<_>>().join("_") }).collect()
}

fn resolve(file: &str, dir: Option<&Path>) -> PathBuf {
    let p = Path::new(file);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.map(|d| d.join(p)).unwrap_or_else(|| p.to_path_buf())
    }
}

/// Define (once) the `.SUBCKT` of a `Sub` component; returns its name and pin count.
fn subcircuit(c: &Component, cx: &mut Ctx) -> Result<(String, usize), String> {
    let file = c.props.first().map(|p| p.value.clone()).unwrap_or_default();
    let path = resolve(&file, cx.dir);
    if let Some((_, n, text)) = cx.subckts.iter().find(|s| s.0 == path) {
        let pins = text.lines().next().map(|l| l.split_whitespace().count().saturating_sub(2)).unwrap_or(0);
        return Ok((n.clone(), pins));
    }
    if cx.depth > 8 {
        return Err(format!("{} (subcircuits nested too deep)", c.name));
    }
    let sub = Schematic::load(&path).map_err(|e| format!("{} ({e})", c.name))?;
    let sdir = path.parent().map(|d| d.to_path_buf());
    let conn = Connectivity::build(&sub, sdir.as_deref());
    let names = node_names(&conn);
    // ports ordered by their number
    let mut ports: Vec<(i64, usize)> = sub
        .components
        .iter()
        .enumerate()
        .filter(|(_, k)| k.typ == "Port")
        .map(|(i, k)| (k.props.first().and_then(|p| p.value.trim().parse().ok()).unwrap_or(0), i))
        .collect();
    ports.sort();
    let pin_net = |ci: usize, k: usize| conn.pins.iter().find(|p| p.comp == ci && p.pin == k).map(|p| p.net);
    let port_nodes: Vec<String> = ports.iter().map(|&(_, ci)| pin_net(ci, 0).map(|n| names[n].clone()).unwrap_or_else(|| "0".into())).collect();
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "sub".into());
    let sub_name = format!("Sub_{}", stem.replace(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_', "_"));
    let mut body = String::new();
    cx.depth += 1;
    {
        let sd = sdir.clone();
        let mut inner = Ctx { dir: sd.as_deref(), depth: cx.depth, subckts: std::mem::take(&mut cx.subckts), unsupported: std::mem::take(&mut cx.unsupported) };
        let node = |ci: usize, k: usize| pin_net(ci, k).map(|n| names[n].clone()).unwrap_or_else(|| format!("_nc_{ci}_{k}"));
        components(&sub, &conn, &node, &mut inner, &mut body);
        cx.subckts = inner.subckts;
        cx.unsupported = inner.unsupported;
    }
    cx.depth -= 1;
    let text = format!(".SUBCKT {sub_name} {}\n{body}.ENDS\n", port_nodes.join(" "));
    cx.subckts.push((path, sub_name.clone(), text));
    Ok((sub_name, ports.len()))
}

/// Build the netlist; `Err` lists what the built-in netlister cannot translate.
pub fn build(sch: &Schematic, conn: &Connectivity, title: &str, dir: Option<&Path>, mode: Mode) -> Result<Netlist, Vec<String>> {
    let names = node_names(conn);
    let mut cx = Ctx { dir, depth: 0, subckts: Vec::new(), unsupported: BTreeSet::new() };
    let mut comps = String::new();
    let pin_node: HashMap<(usize, usize), usize> = conn.pins.iter().map(|p| ((p.comp, p.pin), p.net)).collect();
    let node = |ci: usize, k: usize| pin_node.get(&(ci, k)).map(|&n| names[n].clone()).unwrap_or_else(|| format!("_nc_{ci}_{k}"));
    components(sch, conn, &node, &mut cx, &mut comps);

    fn active_of<'s>(sch: &'s Schematic, t: &'s str) -> impl Iterator<Item = &'s Component> + 's {
        sch.components.iter().filter(move |c| c.typ == t && is_active(c))
    }
    let active = |t: &'static str| active_of(sch, t);
    if mode == Mode::Simulations {
        for c in sch.components.iter().filter(|c| c.typ.starts_with('.') && is_active(c)) {
            if !SUPPORTED_SIMS.contains(&c.typ.as_str()) {
                cx.unsupported.insert(format!("{} simulation", c.typ));
            }
        }
        for c in active(".AC") {
            let ty = prop(c, "Type");
            if ty != "log" && ty != "lin" {
                cx.unsupported.insert(format!(".AC type '{ty}'"));
            }
        }
        for c in active(".TR") {
            if prop(c, "Type") != "lin" {
                cx.unsupported.insert(format!(".TR type '{}'", prop(c, "Type")));
            }
        }
    }
    if active("Eqn").next().is_some() {
        cx.unsupported.insert("Eqn (Qucs equations)".into());
    }
    if !cx.unsupported.is_empty() {
        return Err(cx.unsupported.into_iter().collect());
    }

    let mut s = String::new();
    let _ = writeln!(s, "* qucs-s-rs {}  {title}", env!("CARGO_PKG_VERSION"));
    s.push_str(".func limexp(x) {x<80.0?exp(x):exp(80)*(x-80)}\n.func step(x) {u(x)}\n.func stp(x)  {u(x)}\n");
    for c in active("SpicePar") {
        for p in &c.props {
            if !p.value.trim().is_empty() {
                let _ = writeln!(s, ".PARAM {}", p.value.trim());
            }
        }
    }
    for (_, _, text) in &cx.subckts {
        s.push_str(text);
    }
    s.push_str(&comps);
    s.push_str("\n.control\n\n");

    if mode == Mode::OperatingPoint {
        let _ = write!(s, "op\nprint all > {OP_OUTPUT}\ndestroy all\nreset\n\nexit\n.endc\n.END\n");
        return Ok(Netlist { text: s, node_names: names });
    }

    // output variables: labelled nets attached to an active component, probes
    let mut vars: BTreeSet<String> = BTreeSet::new();
    for (ni, net) in conn.nets.iter().enumerate() {
        if !net.named || net.name.eq_ignore_ascii_case("gnd") {
            continue;
        }
        if net.pins.iter().any(|&p| is_active(&sch.components[conn.pins[p].comp])) {
            vars.insert(names[ni].clone());
        }
    }
    let mut nods: Vec<String> = vars.iter().map(|v| format!("v({v})")).collect();
    for c in sch.components.iter().filter(|c| is_active(c)) {
        match c.typ.as_str() {
            "IProbe" => nods.push(format!("i(V{})", c.name)),
            "VProbe" => nods.push(format!("v({})", c.name)),
            _ => {}
        }
    }
    // document order, like Qucs-S
    for c in sch.components.iter().filter(|c| SUPPORTED_SIMS.contains(&c.typ.as_str()) && is_active(c)) {
        let sim_name = c.name.to_lowercase();
        match c.typ.as_str() {
            ".AC" => {
                let (f1, f2) = (num(&prop(c, "Start")), num(&prop(c, "Stop")));
                let np = num(&prop(c, "Points"));
                let head = if prop(c, "Type") == "log" {
                    let nd = (f2 / f1).log10().ceil().max(1.0);
                    format!("DEC {}", gnum(((np - 1.0) / nd).ceil().max(1.0)))
                } else {
                    format!("LIN {}", prop(c, "Points").trim())
                };
                let _ = writeln!(s, "{}", format!("ac {head} {} {} ", normalize_value(&prop(c, "Start")), normalize_value(&prop(c, "Stop"))).to_lowercase());
            }
            ".TR" => {
                let (t0, t1) = (num(&prop(c, "Start")), num(&prop(c, "Stop")));
                let np = num(&prop(c, "Points")).max(2.0);
                let step = (t1 - t0) / (np - 1.0);
                let mut l = format!("tran {} {} {} ", gnum(step), gnum(t1), gnum(t0));
                let ms = normalize_value(&prop(c, "MaxStep"));
                if !ms.is_empty() && ms != "0" {
                    l.push_str(&ms);
                }
                let _ = writeln!(s, "{l}");
            }
            ".DC" => s.push_str("op\n"),
            _ => {}
        }
        let mut dep = Vec::new();
        for e in active("NutmegEq") {
            let used = prop_at(e, 0).to_lowercase();
            let numbered = used.chars().next().map(|ch| ch.is_alphabetic()).unwrap_or(false) && used.chars().last().map(|ch| ch.is_ascii_digit()).unwrap_or(false);
            let matches = if numbered { sim_name == used } else { sim_name.starts_with(&used) };
            if matches || used == "all" {
                for p in e.props.iter().skip(1) {
                    if let Some((k, v)) = p.value.split_once('=') {
                        let _ = writeln!(s, "let {} = {}", k.trim(), v.trim());
                        dep.push(k.trim().to_string());
                    }
                }
            }
        }
        let mut all = nods.clone();
        all.extend(dep);
        if c.typ == ".DC" {
            let _ = writeln!(s, "print {} > spice4qucs.{sim_name}.ngspice.dc.print", all.join(" "));
        } else if !all.is_empty() {
            let _ = writeln!(s, "write spice4qucs.{}.plot {}", sim_name.replace(' ', "_"), all.join(" "));
        }
        s.push_str("destroy all\nreset\n\n");
    }
    s.push_str("exit\n.endc\n.END\n");
    Ok(Netlist { text: s, node_names: names })
}

/// Netlist a schematic file with the built-in netlister.
pub fn build_file(sch_path: &Path, mode: Mode) -> Result<(Netlist, Connectivity), Vec<String>> {
    let sch = Schematic::load(sch_path).map_err(|e| vec![e])?;
    let dir = sch_path.parent();
    let conn = Connectivity::build(&sch, dir);
    let title = sch_path.display().to_string().replace('\\', "/");
    let n = build(&sch, &conn, &title, dir, mode)?;
    Ok((n, conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_like_qucs_s() {
        assert_eq!(normalize_value("10k"), "10K");
        assert_eq!(normalize_value("50 Ohm"), "50");
        assert_eq!(normalize_value("1 GOhm"), "1G");
        assert_eq!(normalize_value("1 MOhm"), "1MEG");
        assert_eq!(normalize_value("1.5 nF"), "1.5N");
        assert_eq!(normalize_value("1500 nH"), "1500N");
        assert_eq!(normalize_value("13.56 MHz"), "13.56MEG");
        assert_eq!(normalize_value("1 kHz"), "1K");
        assert_eq!(normalize_value("6.76915 mA"), "6.76915M");
        assert_eq!(normalize_value("20 us"), "20U");
        assert_eq!(normalize_value("15 V"), "15");
        assert_eq!(normalize_value("Rload"), "{RLOAD}");
        assert_eq!(normalize_value("1e6"), "1E6");
        assert_eq!(normalize_value("'a*2'"), "'a*2'");
    }

    #[test]
    fn g_format() {
        assert_eq!(gnum(5e-6), "5e-06");
        assert_eq!(gnum(0.003), "0.003");
        assert_eq!(gnum(5e-10), "5e-10");
        assert_eq!(gnum(2e-5), "2e-05");
        assert_eq!(gnum(50.0), "50");
        assert_eq!(gnum(0.0), "0");
        assert_eq!(gnum(123456.0), "123456");
        assert_eq!(gnum(1234567.0), "1.23457e+06");
    }

    fn lines_of(path: &str) -> Vec<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let (n, _) = build_file(&p, Mode::Simulations).expect("netlist");
        n.text.lines().skip(4).map(|l| l.to_string()).collect()
    }

    #[test]
    fn inverting_amp_matches_qucs_s() {
        let l = lines_of("examples/inverting_amp.sch");
        let has = |s: &str| l.iter().any(|x| x.trim_end() == s.trim_end());
        assert!(has("V1 in 0 DC 0 SIN(0 1 1K 0 0 0) AC 1 ACPHASE 0"), "{l:#?}");
        assert!(has("RL out 0  2K tc1=0.0 tc2=0.0"), "{l:#?}");
        assert!(l.iter().any(|x| x.starts_with("B_OP1 out 0 V = 1E6*V(0,")), "{l:#?}");
        assert!(has("ac dec 50 10 10meg"), "{l:#?}");
        assert!(has("let gain_db = dB(v(out)/v(in))"), "{l:#?}");
        assert!(has("write spice4qucs.ac1.plot v(in) v(out) gain_db"), "{l:#?}");
        assert!(has("tran 5e-06 0.003 0"), "{l:#?}");
        assert!(has("write spice4qucs.tr1.plot v(in) v(out)"), "{l:#?}");
    }

    #[test]
    fn plasma_example_matches_qucs_s() {
        let l = lines_of("examples/arxiv1902_02074_rf_plasma.sch");
        let has = |s: &str| l.iter().any(|x| x.trim_end() == s.trim_end());
        assert!(l.iter().any(|x| x.ends_with("0 DC 0 SIN(0 100 13.56MEG 0 0 0) AC 100 ACPHASE 0")), "{l:#?}");
        assert!(has("Cm1 0 tl  1550P"), "{l:#?}");
        assert!(has("BIe1 e1  p1   I = 2.91495*exp(min(-(V(p1)-V(e1))/4.73,30))") || l.iter().any(|x| x.starts_with("BIe1 e1  p1 ")), "{l:#?}");
        assert!(has("Ii1 p1 e1 DC 6.76915M"), "{l:#?}");
        assert!(has("Bq1 q1  0   I = i(Vam1)") || l.iter().any(|x| x.starts_with("Bq1 q1  0 ")), "{l:#?}");
        assert!(has("tran 5e-10 2e-05 0"), "{l:#?}");
        assert!(has("write spice4qucs.tr1.plot v(e1) v(p1) v(p2) v(q1) v(q2) v(tl) v(vl)"), "{l:#?}");
    }

    #[test]
    fn op_mode_prints_everything() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/inverting_amp.sch");
        let (n, conn) = build_file(&p, Mode::OperatingPoint).unwrap();
        assert!(n.text.contains(&format!("print all > {OP_OUTPUT}")));
        assert!(!n.text.contains("tran "));
        assert_eq!(n.node_names.len(), conn.nets.len());
        assert!(n.node_names.iter().any(|x| x == "0"));
    }

    #[test]
    fn supported_types_exist() {
        for t in SUPPORTED_COMPONENTS.iter().chain(SUPPORTED_SIMS.iter()) {
            assert!(lib().info(t).is_some(), "{t}");
            assert!(is_supported(t));
        }
        assert!(!is_supported(".HB"));
    }

    #[test]
    fn unsupported_is_reported() {
        let mut sch = Schematic::new_empty("t");
        sch.components.push(crate::library::new_component("Eqn", "Eqn1", 0, 0).unwrap());
        sch.components.push(crate::library::new_component(".HB", "HB1", 0, 100).unwrap());
        let conn = Connectivity::build(&sch, None);
        let e = build(&sch, &conn, "t", None, Mode::Simulations).err().unwrap();
        assert!(e.iter().any(|x| x.contains("Eqn")), "{e:?}");
        assert!(e.iter().any(|x| x.contains(".HB")), "{e:?}");
    }
}
