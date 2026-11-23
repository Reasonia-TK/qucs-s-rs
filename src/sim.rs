//! Simulation: Qucs-S generates the ngspice netlist (`qucs-s -n --ngspice`),
//! ngspice runs in batch mode and the raw / print outputs are parsed.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const NO_WINDOW: u32 = 0x0800_0000;

fn first_existing(c: &[Option<PathBuf>]) -> Option<PathBuf> {
    c.iter().flatten().find(|p| p.is_file()).cloned()
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["", ".exe"] {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub fn qucs_s_path() -> Result<PathBuf, String> {
    first_existing(&[
        std::env::var_os("QUCS_S_PATH").map(PathBuf::from),
        which("qucs-s"),
        Some(PathBuf::from(r"C:\Program Files\Qucs-S\bin\qucs-s.exe")),
        Some(PathBuf::from(r"C:\Program Files (x86)\Qucs-S\bin\qucs-s.exe")),
        Some(PathBuf::from("/usr/bin/qucs-s")),
        Some(PathBuf::from("/usr/local/bin/qucs-s")),
        Some(PathBuf::from("/Applications/qucs-s.app/Contents/MacOS/qucs-s")),
    ])
    .ok_or_else(|| "qucs-s not found (needed to generate netlists). Set QUCS_S_PATH.".to_string())
}

pub fn ngspice_path() -> Result<PathBuf, String> {
    let qbin = qucs_s_path().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    first_existing(&[
        std::env::var_os("NGSPICE_PATH").map(PathBuf::from),
        which("ngspice_con"),
        Some(PathBuf::from(r"C:\Spice64\bin\ngspice_con.exe")),
        qbin.as_ref().map(|d| d.join("ngspice_con.exe")),
        which("ngspice"),
        qbin.as_ref().map(|d| d.join("ngspice")),
    ])
    .ok_or_else(|| "ngspice not found. Set NGSPICE_PATH to ngspice_con.exe / ngspice.".to_string())
}

fn cmd(p: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(p);
    #[cfg(windows)]
    c.creation_flags(NO_WINDOW);
    c.stdin(Stdio::null());
    c
}

/// Which netlister `netlist` uses: `QUCS_RS_NETLISTER` = `auto` (default: built-in,
/// qucs-s for what it cannot translate), `builtin` or `qucs-s`.
fn netlister_pref() -> String {
    std::env::var("QUCS_RS_NETLISTER").unwrap_or_default().to_ascii_lowercase()
}

/// Netlist a saved schematic: the built-in netlister, or qucs-s for parts it does not know.
pub fn netlist(sch_path: &Path, out: &Path) -> Result<String, String> {
    let pref = netlister_pref();
    if pref != "qucs-s" && pref != "qucs" {
        match crate::spice::build_file(sch_path, crate::spice::Mode::Simulations) {
            Ok((n, _)) => {
                if let Some(d) = out.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                std::fs::write(out, &n.text).map_err(|e| format!("{}: {e}", out.display()))?;
                return Ok(n.text);
            }
            Err(missing) => {
                if pref == "builtin" || qucs_s_path().is_err() {
                    return Err(format!(
                        "the built-in netlister cannot translate: {}\nInstall Qucs-S (it is then used automatically) or replace these parts.",
                        missing.join(", ")
                    ));
                }
            }
        }
    }
    netlist_qucs_s(sch_path, out)
}

/// Netlist a saved schematic with qucs-s.
pub fn netlist_qucs_s(sch_path: &Path, out: &Path) -> Result<String, String> {
    let _ = std::fs::remove_file(out);
    let q = qucs_s_path()?;
    let o = cmd(&q)
        .args(["-n", "--ngspice", "-i"])
        .arg(sch_path)
        .arg("-o")
        .arg(out)
        .output()
        .map_err(|e| format!("cannot run qucs-s: {e}"))?;
    if !out.exists() {
        let mut msg = String::from_utf8_lossy(&o.stderr).to_string();
        msg.push_str(&String::from_utf8_lossy(&o.stdout));
        let msg: Vec<&str> = msg.lines().filter(|l| !l.starts_with("Warning:")).collect();
        return Err(format!("qucs-s failed to create a netlist:\n{}", msg.join("\n")));
    }
    Ok(String::from_utf8_lossy(&std::fs::read(out).map_err(|e| e.to_string())?).to_string())
}

#[derive(Clone, Debug)]
pub struct Vector {
    pub name: String,
    pub unit: String,
    pub re: Vec<f64>,
    pub im: Option<Vec<f64>>,
}

impl Vector {
    pub fn is_complex(&self) -> bool {
        self.im.as_ref().map(|v| v.iter().any(|x| x.abs() > 0.0)).unwrap_or(false)
    }
    pub fn mag(&self, i: usize) -> f64 {
        match &self.im {
            Some(im) => (self.re[i].powi(2) + im[i].powi(2)).sqrt(),
            None => self.re[i].abs(),
        }
    }
    pub fn phase_deg(&self, i: usize) -> f64 {
        match &self.im {
            Some(im) => im[i].atan2(self.re[i]).to_degrees(),
            None => {
                if self.re[i] < 0.0 {
                    180.0
                } else {
                    0.0
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Plot {
    pub name: String,
    pub base: String,
    pub kind: String,
    pub title: String,
    pub vectors: Vec<Vector>,
    pub sweep: Option<Value>,
}

impl Plot {
    pub fn scale(&self) -> Option<&Vector> {
        if self.kind == "op" {
            None
        } else {
            self.vectors.first()
        }
    }
    pub fn vector(&self, name: &str) -> Option<&Vector> {
        let n = name.trim().to_ascii_lowercase();
        self.vectors.iter().find(|v| v.name.to_ascii_lowercase() == n).or_else(|| {
            // v(out) <-> out
            let bare = n.trim_start_matches("v(").trim_end_matches(')').to_string();
            self.vectors.iter().find(|v| {
                let vn = v.name.to_ascii_lowercase();
                vn == bare || vn == format!("v({bare})")
            })
        })
    }
    pub fn summary(&self) -> Value {
        let mut vecs = serde_json::Map::new();
        for v in self.vectors.iter().take(80) {
            vecs.insert(v.name.clone(), describe(v));
        }
        let mut o = json!({
            "analysis": self.name,
            "kind": self.kind,
            "title": self.title,
            "points": self.vectors.first().map(|v| v.re.len()).unwrap_or(0),
            "scale": self.scale().map(|v| v.name.clone()),
            "vectors": vecs,
        });
        if let Some(s) = &self.sweep {
            o["sweep"] = s.clone();
            o["base"] = json!(self.base);
        }
        o
    }
}

fn g6(x: f64) -> Value {
    if !x.is_finite() {
        return json!(x.to_string());
    }
    let s = format!("{:.6e}", x);
    json!(s.parse::<f64>().unwrap_or(x))
}

pub fn describe(v: &Vector) -> Value {
    let n = v.re.len();
    if n == 0 {
        return json!({"n": 0});
    }
    if v.is_complex() {
        let mags: Vec<f64> = (0..n).map(|i| v.mag(i)).collect();
        let mx = mags.iter().cloned().fold(f64::MIN, f64::max);
        let mn = mags.iter().cloned().fold(f64::MAX, f64::min);
        return json!({"n": n, "unit": v.unit, "complex": true, "mag_min": g6(mn), "mag_max": g6(mx),
            "first": [g6(v.re[0]), g6(v.im.as_ref().unwrap()[0])], "last": [g6(v.re[n-1]), g6(v.im.as_ref().unwrap()[n-1])]});
    }
    if n == 1 {
        return json!({"value": g6(v.re[0]), "unit": v.unit});
    }
    let mx = v.re.iter().cloned().fold(f64::MIN, f64::max);
    let mn = v.re.iter().cloned().fold(f64::MAX, f64::min);
    let mean = v.re.iter().sum::<f64>() / n as f64;
    json!({"n": n, "unit": v.unit, "min": g6(mn), "max": g6(mx), "first": g6(v.re[0]), "last": g6(v.re[n-1]), "mean": g6(mean)})
}

fn kind_from_title(t: &str) -> &'static str {
    let t = t.to_ascii_lowercase();
    let word = |w: &str| t.split(|c: char| !c.is_alphanumeric()).any(|x| x == w);
    if t.contains("dc transfer") {
        "dc"
    } else if word("ac") {
        "ac"
    } else if word("transient") {
        "tran"
    } else if word("operating") {
        "op"
    } else if word("noise") {
        "noise"
    } else if word("fourier") {
        "fourier"
    } else if word("distortion") {
        "disto"
    } else if t.contains("pole") {
        "pz"
    } else if word("sensitivity") {
        "sens"
    } else {
        "other"
    }
}

/// ngspice writes unit *names* ("frequency", "voltage"); show SI symbols instead.
fn si_unit(u: &str) -> String {
    match u.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str() {
        "frequency" | "hertz" => "Hz".into(),
        "time" => "s".into(),
        "voltage" => "V".into(),
        "current" => "A".into(),
        "decibel" => "dB".into(),
        "degree" | "degrees" => "°".into(),
        "radian" | "radians" => "rad".into(),
        "impedance" => "Ω".into(),
        "notype" | "" => String::new(),
        _ => u.to_string(),
    }
}

const REAL_UNITS: [&str; 8] = ["frequency", "time", "decibel", "degree", "degrees", "radian", "radians", "hertz"];

pub fn parse_raw(data: &[u8], name: &str) -> Vec<Plot> {
    let mut plots = Vec::new();
    let mut pos = 0usize;
    let next_line = |pos: &mut usize| -> Option<String> {
        if *pos >= data.len() {
            return None;
        }
        let nl = data[*pos..].iter().position(|&b| b == b'\n').map(|k| *pos + k).unwrap_or(data.len());
        let l = String::from_utf8_lossy(&data[*pos..nl]).trim_end_matches('\r').to_string();
        *pos = nl + 1;
        Some(l)
    };
    loop {
        let mut hdr: Vec<(String, String)> = Vec::new();
        let mut vars: Vec<(String, String)> = Vec::new();
        let mut format = None;
        while let Some(line) = next_line(&mut pos) {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with("Variables:") {
                let nv: usize = hdr.iter().find(|h| h.0 == "No. Variables").and_then(|h| h.1.parse().ok()).unwrap_or(0);
                for _ in 0..nv {
                    let vl = next_line(&mut pos).unwrap_or_default();
                    let parts: Vec<&str> = vl.split_whitespace().collect();
                    if parts.len() >= 2 {
                        vars.push((parts[1].to_string(), parts.get(2).map(|s| s.to_string()).unwrap_or_default()));
                    }
                }
                continue;
            }
            if line.starts_with("Values:") || line.starts_with("Binary:") {
                format = Some(if line.starts_with("Binary") { "bin" } else { "ascii" });
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                hdr.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let Some(format) = format else { break };
        let get = |k: &str| hdr.iter().find(|h| h.0 == k).map(|h| h.1.clone()).unwrap_or_default();
        let complex = get("Flags").to_ascii_lowercase().contains("complex");
        let npts: usize = get("No. Points").parse().unwrap_or(0);
        let nv = vars.len();
        let mut re = vec![vec![0.0f64; npts]; nv];
        let mut im = vec![vec![0.0f64; npts]; nv];
        if format == "bin" {
            let width = if complex { 16 } else { 8 };
            let need = npts * nv * width;
            let end = (pos + need).min(data.len());
            let buf = &data[pos..end];
            let rd = |o: usize| -> f64 {
                if o + 8 <= buf.len() {
                    f64::from_le_bytes(buf[o..o + 8].try_into().unwrap())
                } else {
                    0.0
                }
            };
            for i in 0..npts {
                for j in 0..nv {
                    let o = (i * nv + j) * width;
                    re[j][i] = rd(o);
                    if complex {
                        im[j][i] = rd(o + 8);
                    }
                }
            }
            pos = end;
        } else {
            // ascii: until next "Title:"/"Plotname:"
            let rest = &data[pos..];
            let text = String::from_utf8_lossy(rest);
            let mut end = text.len();
            for key in ["\nTitle:", "\nPlotname:"] {
                if let Some(k) = text.find(key) {
                    end = end.min(k);
                }
            }
            let toks: Vec<&str> = text[..end].split_whitespace().collect();
            let mut k = 0;
            for i in 0..npts {
                k += 1; // point index
                for j in 0..nv {
                    if k >= toks.len() {
                        break;
                    }
                    let s = toks[k];
                    k += 1;
                    if complex {
                        let (a, b) = s.split_once(',').unwrap_or((s, "0"));
                        re[j][i] = a.parse().unwrap_or(0.0);
                        im[j][i] = b.parse().unwrap_or(0.0);
                    } else {
                        re[j][i] = s.parse().unwrap_or(0.0);
                    }
                }
            }
            pos += text[..end].len() + 1;
        }
        let title = get("Plotname");
        let mut vectors = Vec::new();
        for (j, (vn, unit)) in vars.iter().enumerate() {
            let mut v = Vector { name: vn.clone(), unit: si_unit(unit), re: std::mem::take(&mut re[j]), im: None };
            if complex {
                let imv = std::mem::take(&mut im[j]);
                let maxre = v.re.iter().map(|x| x.abs()).fold(0.0, f64::max) + 1e-300;
                let real_unit = REAL_UNITS.contains(&unit.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str());
                let tiny = imv.iter().all(|x| x.abs() <= 1e-12 * maxre);
                if !((j == 0 || real_unit) && tiny) {
                    v.im = Some(imv);
                }
            }
            let mut key = vn.clone();
            let mut n = 2;
            while vectors.iter().any(|x: &Vector| x.name == key) {
                key = format!("{vn}#{n}");
                n += 1;
            }
            v.name = key;
            vectors.push(v);
        }
        plots.push(Plot { name: name.into(), base: name.into(), kind: kind_from_title(&title).into(), title, vectors, sweep: None });
    }
    plots
}

fn parse_print(text: &str, name: &str) -> Plot {
    let mut vectors = Vec::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let vals: Vec<f64> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            if k.is_empty() || vals.is_empty() {
                continue;
            }
            vectors.push(Vector { name: k.into(), unit: String::new(), re: vec![vals[0]], im: vals.get(1).map(|x| vec![*x]) });
        }
    }
    Plot { name: name.into(), base: name.into(), kind: "op".into(), title: "Operating point".into(), vectors, sweep: None }
}

fn sweep_steps(text: &str) -> Vec<Value> {
    let mut names: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let tok: Vec<String> = line.replace('"', " ").split_whitespace().map(|s| s.to_string()).collect();
        if tok.is_empty() {
            continue;
        }
        if tok[0].eq_ignore_ascii_case("STEP") {
            names = tok[1..].iter().map(|n| n.rsplit('.').next().unwrap_or(n).to_string()).collect();
            continue;
        }
        let vals: Option<Vec<f64>> = tok[1..].iter().map(|v| v.parse().ok()).collect();
        if let Some(vals) = vals {
            let mut m = serde_json::Map::new();
            for (i, v) in vals.iter().enumerate() {
                let n = names.get(i).cloned().unwrap_or_else(|| format!("p{}", i + 1));
                m.insert(n, json!(v));
            }
            out.push(Value::Object(m));
        }
    }
    out
}

fn short_name(f: &str) -> String {
    let parts: Vec<&str> = f.split('.').collect();
    if parts.len() >= 3 && parts[0] == "spice4qucs" {
        parts[1].to_string()
    } else {
        f.to_string()
    }
}

fn output_files(net: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in net.lines() {
        let l = line.trim();
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("write ") || lower.starts_with("wrdata ") {
            if let Some(f) = l.split_whitespace().nth(1) {
                files.push(f.to_string());
            }
        }
        if lower.starts_with("print") {
            if let Some((_, f)) = l.rsplit_once('>') {
                let f = f.trim();
                if !f.is_empty() {
                    files.push(f.to_string());
                }
            }
        }
    }
    files
}

#[derive(Clone, Debug, Default)]
pub struct SimResult {
    pub ok: bool,
    pub plots: Vec<Plot>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub log: String,
    pub workdir: PathBuf,
    pub netlist: PathBuf,
    pub elapsed: f64,
}

impl SimResult {
    pub fn find(&self, analysis: Option<&str>) -> Result<&Plot, String> {
        if self.plots.is_empty() {
            return Err("no simulation results (run_simulation first)".into());
        }
        let Some(a) = analysis else { return Ok(&self.plots[0]) };
        let a = a.to_ascii_lowercase();
        self.plots
            .iter()
            .find(|p| p.name.to_ascii_lowercase() == a)
            .or_else(|| self.plots.iter().find(|p| p.kind == a || p.name.to_ascii_lowercase().starts_with(&a)))
            .ok_or_else(|| format!("no analysis '{a}'. Available: {}", self.plots.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", ")))
    }
    pub fn find_all(&self, analysis: Option<&str>) -> Result<Vec<&Plot>, String> {
        if let Some(a) = analysis {
            let al = a.to_ascii_lowercase();
            if !self.plots.iter().any(|p| p.name.to_ascii_lowercase() == al) {
                let steps: Vec<&Plot> = self.plots.iter().filter(|p| p.base.to_ascii_lowercase() == al && p.sweep.is_some()).collect();
                if !steps.is_empty() {
                    return Ok(steps);
                }
            }
        }
        Ok(vec![self.find(analysis)?])
    }
    pub fn summary(&self) -> Value {
        json!({
            "ok": self.ok,
            "elapsed_s": (self.elapsed * 1000.0).round() / 1000.0,
            "analyses": self.plots.iter().map(|p| p.summary()).collect::<Vec<_>>(),
            "errors": self.errors,
            "warnings": self.warnings.iter().take(15).collect::<Vec<_>>(),
            "workdir": self.workdir.display().to_string(),
            "netlist": self.netlist.display().to_string(),
        })
    }
}

/// Absolute path without the `\\?\` prefix Windows' `canonicalize` adds.
pub fn canonical(p: &Path) -> Result<PathBuf, String> {
    let c = p.canonicalize().map_err(|e| format!("{}: {e}", p.display()))?;
    let s = c.display().to_string();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    })
}

/// Marks a temporary copy of an unsaved schematic (see `unsaved_copy_path`).
const UNSAVED_TAG: &str = ".~unsaved-";

/// `<dir>/<stem>.~unsaved-<kind>.sch`: where the editor writes the unsaved state of a
/// schematic to simulate it without overwriting the file. It sits next to the original so
/// that relative subcircuit / library references still resolve.
pub fn unsaved_copy_path(sch_path: &Path, kind: &str) -> Result<PathBuf, String> {
    let stem = sch_path.file_stem().map(|s| s.to_string_lossy().to_string()).ok_or_else(|| format!("{}: not a file name", sch_path.display()))?;
    let dir = sch_path.parent().ok_or_else(|| format!("{}: no parent directory", sch_path.display()))?;
    Ok(dir.join(format!("{stem}{UNSAVED_TAG}{kind}.sch")))
}

/// `<dir>/<stem>_<suffix>` next to a schematic, plus the stem. A temporary unsaved copy
/// uses the work directory of its original.
pub fn work_dir(sch_path: &Path, suffix: &str) -> Result<(PathBuf, String), String> {
    let stem = sch_path.file_stem().map(|s| s.to_string_lossy().to_string()).ok_or_else(|| format!("{}: not a file name", sch_path.display()))?;
    let stem = stem.split(UNSAVED_TAG).next().unwrap_or_default().to_string();
    let dir = sch_path.parent().ok_or_else(|| format!("{}: no parent directory", sch_path.display()))?;
    Ok((dir.join(format!("{stem}_{suffix}")), stem))
}

/// DC operating point of a saved schematic (built-in netlister only, so that the
/// SPICE node names can be mapped back to the nets of the schematic).
pub struct OperatingPoint {
    /// voltage of every net (index = net index of the schematic's connectivity)
    pub net_volts: Vec<Option<f64>>,
    /// net names, same order
    pub net_names: Vec<String>,
    /// branch currents of voltage sources / probes: (element, A)
    pub currents: Vec<(String, f64)>,
    pub errors: Vec<String>,
}

impl OperatingPoint {
    pub fn json(&self) -> Value {
        let nodes: serde_json::Map<String, Value> = self
            .net_names
            .iter()
            .zip(&self.net_volts)
            .filter(|(n, _)| !n.eq_ignore_ascii_case("gnd"))
            .map(|(n, v)| (n.clone(), v.map(g6).unwrap_or(Value::Null)))
            .collect();
        let cur: serde_json::Map<String, Value> = self.currents.iter().map(|(n, v)| (n.clone(), g6(*v))).collect();
        json!({"node_voltages_V": nodes, "branch_currents_A": cur, "errors": self.errors})
    }
}

pub fn operating_point(sch_path: &Path, timeout_s: u64) -> Result<OperatingPoint, String> {
    let sch_path = canonical(sch_path)?;
    let (n, conn) = crate::spice::build_file(&sch_path, crate::spice::Mode::OperatingPoint)
        .map_err(|m| format!("DC operating point needs the built-in netlister, which cannot translate: {}", m.join(", ")))?;
    let (workdir, stem) = work_dir(&sch_path, "op")?;
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
    let net_path = workdir.join(format!("{stem}_op.cir"));
    std::fs::write(&net_path, &n.text).map_err(|e| e.to_string())?;
    let r = run_netlist(&n.text, &workdir, &net_path, timeout_s)?;
    let plot = r.plots.iter().find(|p| p.kind == "op").ok_or_else(|| format!("ngspice gave no operating point:\n{}", r.errors.join("\n")))?;
    let value = |name: &str| plot.vectors.iter().find(|v| v.name.eq_ignore_ascii_case(name) || v.name.eq_ignore_ascii_case(&format!("v({name})"))).and_then(|v| v.re.first().copied());
    let net_volts = n.node_names.iter().map(|nm| if nm == "0" { Some(0.0) } else { value(nm) }).collect();
    let currents = plot
        .vectors
        .iter()
        .filter_map(|v| {
            let l = v.name.to_ascii_lowercase();
            let el = l.strip_suffix("#branch").or_else(|| l.strip_prefix("i(").and_then(|x| x.strip_suffix(')')))?;
            Some((el.to_string(), *v.re.first()?))
        })
        .collect();
    Ok(OperatingPoint { net_volts, net_names: conn.nets.iter().map(|n| n.name.clone()).collect(), currents, errors: r.errors })
}

pub fn simulate(sch_path: &Path, timeout_s: u64) -> Result<SimResult, String> {
    let sch_path = canonical(sch_path)?;
    let (workdir, stem) = work_dir(&sch_path, "sim")?;
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
    // GUI snapshots and the CLI/MCP share the project's simulation outputs. Hold
    // the workspace lock until all output files have been read into the result.
    let workspace_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(workdir.join(".qucs-rs.lock"))
        .map_err(|e| format!("cannot open simulation workspace lock: {e}"))?;
    workspace_lock.lock().map_err(|e| format!("cannot lock simulation workspace: {e}"))?;
    let net_path = workdir.join(format!("{stem}.cir"));
    let net = netlist(&sch_path, &net_path)?;
    run_netlist(&net, &workdir, &net_path, timeout_s)
}

/// Run an ngspice netlist (already written to `net_path` inside `workdir`).
pub fn run_netlist(net: &str, workdir: &Path, net_path: &Path, timeout_s: u64) -> Result<SimResult, String> {
    let workdir = workdir.to_path_buf();
    let net_path = net_path.to_path_buf();
    let expected = output_files(net);
    if !net.to_ascii_lowercase().contains(".control") || expected.is_empty() {
        return Err("the schematic contains no simulation (add one with add_simulation: tr, ac, dc ...)".into());
    }
    for f in &expected {
        let _ = std::fs::remove_file(workdir.join(f));
    }
    let ng = ngspice_path()?;
    let t0 = Instant::now();
    let mut child = cmd(&ng)
        .arg("-b")
        .arg(net_path.file_name().ok_or("netlist path has no file name")?)
        .current_dir(&workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ngspice: {e}"))?;
    // read output in threads to avoid pipe dead-locks
    let (Some(mut so), Some(mut se)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("cannot capture ngspice output".into());
    };
    let h1 = std::thread::spawn(move || {
        let mut s = String::new();
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut so, &mut b);
        s.push_str(&String::from_utf8_lossy(&b));
        s
    });
    let h2 = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut se, &mut b);
        String::from_utf8_lossy(&b).to_string()
    });
    let mut timed_out = false;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if t0.elapsed().as_secs() > timeout_s {
            let _ = child.kill();
            timed_out = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut log = h1.join().unwrap_or_default();
    let err = h2.join().unwrap_or_default();
    if !err.is_empty() {
        log.push('\n');
        log.push_str(&err);
    }
    if timed_out {
        log.push_str(&format!("\nngspice timed out after {timeout_s}s"));
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let _ = std::fs::write(workdir.join("ngspice.log"), &log);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for line in log.lines() {
        let l = line.to_ascii_lowercase();
        let t = line.trim().to_string();
        let is_err = ["error", "fatal", "singular matrix", "timestep too small", "no such vector", "unknown model", "could not find", "aborted", "is not converging"]
            .iter()
            .any(|k| l.contains(k));
        if is_err {
            if !errors.contains(&t) {
                errors.push(t);
            }
        } else if l.contains("warning") && !warnings.contains(&t) {
            warnings.push(t);
        }
    }
    let mut plots = Vec::new();
    for f in &expected {
        let p = workdir.join(f);
        let Ok(data) = std::fs::read(&p) else {
            errors.push(format!("expected output '{f}' was not produced"));
            continue;
        };
        let base = short_name(f);
        if f.ends_with(".print") {
            plots.push(parse_print(&String::from_utf8_lossy(&data), &base));
        } else {
            let mut sub = parse_raw(&data, &base);
            let res = workdir.join(format!("spice4qucs.{base}.cir.res"));
            let steps = if sub.len() > 1 { std::fs::read_to_string(&res).map(|t| sweep_steps(&t)).unwrap_or_default() } else { vec![] };
            let n = sub.len();
            for (k, pl) in sub.iter_mut().enumerate() {
                pl.base = base.clone();
                pl.name = if n > 1 { format!("{base}_{}", k + 1) } else { base.clone() };
                pl.sweep = steps.get(k).cloned();
            }
            plots.extend(sub);
        }
    }
    errors.truncate(30);
    warnings.truncate(30);
    let ok = !plots.is_empty() && !errors.iter().any(|e| e.contains("expected output"));
    let tail: String = log.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect();
    Ok(SimResult { ok, plots, errors, warnings, log: tail, workdir, netlist: net_path, elapsed })
}

/// Linear interpolation of vector `v` against scale `s` at x.
pub fn interp(s: &[f64], v: &[f64], x: f64) -> Option<f64> {
    if s.len() < 2 || s.len() != v.len() {
        return v.first().copied();
    }
    for i in 0..s.len() - 1 {
        let (a, b) = (s[i], s[i + 1]);
        if (a <= x && x <= b) || (b <= x && x <= a) {
            let t = if (b - a).abs() < 1e-300 { 0.0 } else { (x - a) / (b - a) };
            return Some(v[i] + t * (v[i + 1] - v[i]));
        }
    }
    None
}
