//! Monte Carlo analysis: the schematic is netlisted once by Qucs-S, then ngspice
//! runs N times in parallel with the R / C / L values (and any listed part)
//! randomly perturbed within their tolerances. Results are summarised at
//! measurement points (mean, σ, min/max, percentiles, yield against limits).

use crate::sim::{self, Plot, SimResult};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct McSpec {
    pub runs: usize,
    /// default tolerance in % by element letter: 'R', 'C', 'L'
    pub default_tol: HashMap<char, f64>,
    /// per component tolerance in % (overrides the default; 0 = fixed)
    pub tol: HashMap<String, f64>,
    pub gauss: bool,
    pub seed: u64,
    pub analysis: Option<String>,
    pub vectors: Vec<String>,
    /// x positions (time / frequency / sweep value) to measure at
    pub at: Vec<f64>,
    /// pass/fail limits: (vector, at, min, max); dB for complex vectors
    pub limits: Vec<(String, f64, Option<f64>, Option<f64>)>,
    pub timeout_s: u64,
}

impl Default for McSpec {
    fn default() -> Self {
        McSpec {
            runs: 50,
            default_tol: HashMap::from([('R', 5.0), ('C', 10.0), ('L', 10.0)]),
            tol: HashMap::new(),
            gauss: true,
            seed: 1,
            analysis: None,
            vectors: vec![],
            at: vec![],
            limits: vec![],
            timeout_s: 120,
        }
    }
}

pub struct McResult {
    pub runs: Vec<(usize, Result<SimResult, String>)>,
    pub summary: Value,
}

// ---------------------------------------------------------------------------
// random numbers (xorshift64*, Box-Muller)
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn gauss(&mut self) -> f64 {
        let u1 = self.next().max(1e-300);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

// ---------------------------------------------------------------------------
// SPICE numbers
// ---------------------------------------------------------------------------

/// Parse a SPICE number ("978.5N", "10k", "1meg", "2.2u", "1e-9").
pub fn spice_num(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_lowercase();
    let end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e')).unwrap_or(t.len());
    // "1e" followed by a letter: the 'e' belongs to the number only if digits follow
    let (num, suf) = t.split_at(end);
    let num = num.trim_end_matches('e');
    let v: f64 = num.parse().ok()?;
    let mult = if suf.starts_with("meg") {
        1e6
    } else {
        match suf.chars().next() {
            Some('t') => 1e12,
            Some('g') => 1e9,
            Some('k') => 1e3,
            Some('m') => 1e-3,
            Some('u') | Some('µ') => 1e-6,
            Some('n') => 1e-9,
            Some('p') => 1e-12,
            Some('f') => 1e-15,
            _ => 1.0,
        }
    };
    Some(v * mult)
}

/// Element lines whose value can be varied: (line index, name, value token index, value).
fn variable_elements(net: &str) -> Vec<(usize, String, usize, f64)> {
    let mut out = Vec::new();
    let mut in_control = false;
    for (i, line) in net.lines().enumerate() {
        let l = line.trim();
        let low = l.to_ascii_lowercase();
        if low.starts_with(".control") {
            in_control = true;
        }
        if low.starts_with(".endc") {
            in_control = false;
            continue;
        }
        if in_control || l.is_empty() || l.starts_with('*') || l.starts_with('.') {
            continue;
        }
        let first = l.chars().next().unwrap().to_ascii_uppercase();
        if !matches!(first, 'R' | 'C' | 'L') {
            continue;
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        if toks.len() < 4 {
            continue;
        }
        if let Some(v) = spice_num(toks[3]) {
            if v != 0.0 {
                out.push((i, toks[0].to_string(), 3, v));
            }
        }
    }
    out
}

fn tolerance(spec: &McSpec, name: &str) -> f64 {
    // netlist names are the Qucs names (R1, C2 ...); some dialects prefix them
    if let Some(t) = spec.tol.get(name).or_else(|| spec.tol.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)) {
        return *t;
    }
    let letter = name.chars().next().unwrap_or('X').to_ascii_uppercase();
    spec.default_tol.get(&letter).copied().unwrap_or(0.0)
}

/// Run the Monte Carlo analysis for a saved schematic.
pub fn run(sch_path: &Path, spec: &McSpec) -> Result<McResult, String> {
    let sch_path = sim::canonical(sch_path)?;
    let (base, stem) = sim::work_dir(&sch_path, "mc")?;
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let net = sim::netlist(&sch_path, &base.join("nominal.cir"))?;
    if !net.to_ascii_lowercase().contains(".control") {
        return Err("the schematic contains no simulation (add one with add_simulation)".into());
    }
    let elems = variable_elements(&net);
    let varied: Vec<&(usize, String, usize, f64)> = elems.iter().filter(|e| tolerance(spec, &e.1) > 0.0).collect();
    if varied.is_empty() {
        return Err("no R / C / L element with a tolerance > 0 found in the netlist".into());
    }
    let runs = spec.runs.clamp(1, 2000);
    let lines: Vec<&str> = net.lines().collect();
    let mut rng = Rng(spec.seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut jobs: Vec<(usize, PathBuf, PathBuf, String)> = Vec::new();
    let mut params = Vec::new();
    for k in 0..runs {
        let mut l2: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let mut used = Vec::new();
        for (li, name, ti, v) in &varied {
            let tol = tolerance(spec, name) / 100.0;
            // run 0 is the nominal circuit
            let f = if k == 0 {
                1.0
            } else if spec.gauss {
                1.0 + (rng.gauss() * tol / 3.0).clamp(-tol, tol)
            } else {
                1.0 + (rng.next() * 2.0 - 1.0) * tol
            };
            let nv = v * f;
            let mut toks: Vec<String> = l2[*li].split_whitespace().map(|s| s.to_string()).collect();
            toks[*ti] = format!("{nv:.6e}");
            l2[*li] = toks.join(" ");
            used.push((name.clone(), nv));
        }
        params.push(used);
        let dir = base.join(format!("run{k:04}"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let cir = dir.join(format!("{stem}.cir"));
        let text = l2.join("\n") + "\n";
        std::fs::write(&cir, &text).map_err(|e| e.to_string())?;
        jobs.push((k, dir, cir, text));
    }
    // parallel ngspice
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 12);
    let jobs = std::sync::Arc::new(std::sync::Mutex::new(jobs));
    let results = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hs = Vec::new();
    for _ in 0..threads {
        let jobs = jobs.clone();
        let results = results.clone();
        let tmo = spec.timeout_s;
        hs.push(std::thread::spawn(move || loop {
            let job = jobs.lock().unwrap_or_else(|e| e.into_inner()).pop();
            let Some((k, dir, cir, text)) = job else { break };
            let r = sim::run_netlist(&text, &dir, &cir, tmo);
            results.lock().unwrap_or_else(|e| e.into_inner()).push((k, r));
        }));
    }
    for h in hs {
        let _ = h.join();
    }
    let mut runs_out = std::mem::take(&mut *results.lock().unwrap_or_else(|e| e.into_inner()));
    runs_out.sort_by_key(|r| r.0);
    let summary = summarize(&runs_out, &params, spec, &varied.iter().map(|v| (v.1.clone(), tolerance(spec, &v.1))).collect::<Vec<_>>());
    Ok(McResult { runs: runs_out, summary })
}

fn value_at(p: &Plot, vec: &str, x: f64) -> Option<(f64, bool)> {
    let v = p.vector(vec)?;
    let sc = p.scale();
    let (re, im) = match sc {
        Some(sc) if sc.re.len() > 1 => {
            let s = &sc.re;
            let log = p.kind == "ac" || p.kind == "noise";
            let interp = |ys: &[f64]| -> Option<f64> {
                for i in 0..s.len() - 1 {
                    let (a, b) = (s[i], s[i + 1]);
                    if (a <= x && x <= b) || (b <= x && x <= a) {
                        let t = if log && a > 0.0 && b > 0.0 { (x.ln() - a.ln()) / (b.ln() - a.ln()) } else if b != a { (x - a) / (b - a) } else { 0.0 };
                        return Some(ys[i] + t * (ys[i + 1] - ys[i]));
                    }
                }
                None
            };
            (interp(&v.re)?, v.im.as_ref().and_then(|im| interp(im)))
        }
        _ => (*v.re.first()?, v.im.as_ref().and_then(|i| i.first().copied())),
    };
    match im {
        Some(im) if v.is_complex() => Some((20.0 * (re * re + im * im).sqrt().max(1e-300).log10(), true)),
        _ => Some((re, false)),
    }
}

fn stats(xs: &[f64]) -> Value {
    if xs.is_empty() {
        return Value::Null;
    }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let sd = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| s[((p * (s.len() - 1) as f64).round() as usize).min(s.len() - 1)];
    let g = |x: f64| (x * 1e6).round() / 1e6;
    json!({"n": xs.len(), "mean": g(mean), "std": g(sd), "min": g(s[0]), "max": g(*s.last().unwrap()), "p01": g(pct(0.01)), "p50": g(pct(0.5)), "p99": g(pct(0.99))})
}

fn summarize(runs: &[(usize, Result<SimResult, String>)], params: &[Vec<(String, f64)>], spec: &McSpec, varied: &[(String, f64)]) -> Value {
    let ok: Vec<&SimResult> = runs.iter().filter_map(|r| r.1.as_ref().ok()).filter(|r| r.ok).collect();
    let failed = runs.len() - ok.len();
    let mut meas = Vec::new();
    // measurements at points
    let analysis = spec.analysis.clone();
    let first = ok.first().and_then(|r| r.find(analysis.as_deref()).ok().map(|p| p.name.clone()));
    let mut vectors = spec.vectors.clone();
    if vectors.is_empty() {
        if let Some(r) = ok.first() {
            if let Ok(p) = r.find(analysis.as_deref()) {
                let mut v: Vec<String> = p.vectors.iter().skip(if p.kind == "op" { 0 } else { 1 }).filter(|v| v.name.starts_with("v(")).map(|v| v.name.clone()).collect();
                // the output node first, if there is one
                if let Some(i) = v.iter().position(|n| n == "v(out)") {
                    let o = v.remove(i);
                    v.insert(0, o);
                }
                v.truncate(3);
                vectors = v;
            }
        }
    }
    let ats: Vec<Option<f64>> = if spec.at.is_empty() { vec![None] } else { spec.at.iter().map(|&a| Some(a)).collect() };
    for vname in &vectors {
        for at in &ats {
            let mut xs = Vec::new();
            let mut is_db = false;
            for r in &ok {
                let Ok(p) = r.find(analysis.as_deref()) else { continue };
                let x = at.unwrap_or_else(|| p.scale().map(|s| *s.re.last().unwrap_or(&0.0)).unwrap_or(0.0));
                if let Some((v, db)) = value_at(p, vname, x) {
                    xs.push(v);
                    is_db = db;
                }
            }
            let nominal = ok.first().and_then(|r| r.find(analysis.as_deref()).ok()).and_then(|p| {
                let x = at.unwrap_or_else(|| p.scale().map(|s| *s.re.last().unwrap_or(&0.0)).unwrap_or(0.0));
                value_at(p, vname, x).map(|v| v.0)
            });
            meas.push(json!({"vector": vname, "at": at, "unit": if is_db { "dB" } else { "" }, "nominal": nominal, "stats": stats(&xs)}));
        }
    }
    // which part values gave the extreme results of the first measurement
    let mut extremes = Value::Null;
    if let (Some(vname), Some(at)) = (vectors.first(), ats.first()) {
        let mut vals: Vec<(usize, f64)> = Vec::new();
        for (k, r) in runs {
            let Ok(r) = r else { continue };
            let Ok(p) = r.find(analysis.as_deref()) else { continue };
            let x = at.unwrap_or_else(|| p.scale().map(|s| *s.re.last().unwrap_or(&0.0)).unwrap_or(0.0));
            if let Some((v, _)) = value_at(p, vname, x) {
                vals.push((*k, v));
            }
        }
        let pick = |cmp: fn(f64, f64) -> bool| vals.iter().copied().reduce(|a, b| if cmp(b.1, a.1) { b } else { a });
        let describe = |e: Option<(usize, f64)>| {
            e.map(|(k, v)| json!({"run": k, "value": (v * 1e6).round() / 1e6,
                "parts": params.get(k).map(|ps| ps.iter().map(|(n, x)| json!({"element": n, "value": crate::plot::eng(*x)})).collect::<Vec<_>>())}))
        };
        extremes = json!({"vector": vname, "at": at, "lowest": describe(pick(|a, b| a < b)), "highest": describe(pick(|a, b| a > b))});
    }
    // yield
    let mut yield_v = Value::Null;
    if !spec.limits.is_empty() {
        let mut pass = 0;
        let mut fails: HashMap<String, usize> = HashMap::new();
        for r in &ok {
            let Ok(p) = r.find(analysis.as_deref()) else { continue };
            let mut good = true;
            for (v, at, lo, hi) in &spec.limits {
                match value_at(p, v, *at) {
                    Some((x, _)) => {
                        if lo.map(|l| x < l).unwrap_or(false) || hi.map(|h| x > h).unwrap_or(false) {
                            good = false;
                            *fails.entry(format!("{v} @ {at}")).or_default() += 1;
                        }
                    }
                    None => good = false,
                }
            }
            if good {
                pass += 1;
            }
        }
        yield_v = json!({"pass": pass, "total": ok.len(), "yield_percent": if ok.is_empty() { 0.0 } else { (pass as f64 * 1000.0 / ok.len() as f64).round() / 10.0 }, "failures_by_limit": fails});
    }
    json!({
        "runs": runs.len(), "successful": ok.len(), "failed": failed, "analysis": first,
        "distribution": if spec.gauss { "gauss (tolerance = 3σ)" } else { "uniform" },
        "varied": varied.iter().map(|(n, t)| json!({"element": n, "tolerance_percent": t})).collect::<Vec<_>>(),
        "measurements": meas, "extremes": extremes, "yield": yield_v,
        "first_error": runs.iter().find_map(|r| r.1.as_ref().err().cloned().or_else(|| r.1.as_ref().ok().filter(|s| !s.ok).map(|s| s.errors.join("; ")))),
    })
}

/// Overlay of all runs (nominal run highlighted first) as a diagram SVG.
pub fn overlay_svg(res: &McResult, spec: &McSpec, max_traces: usize) -> Result<String, String> {
    let ok: Vec<&SimResult> = res.runs.iter().filter_map(|r| r.1.as_ref().ok()).filter(|r| r.ok).collect();
    let mut plots: Vec<Plot> = Vec::new();
    for (k, r) in ok.iter().take(max_traces).enumerate() {
        let mut p = r.find(spec.analysis.as_deref())?.clone();
        p.sweep = Some(json!({"run": k}));
        plots.push(p);
    }
    let refs: Vec<&Plot> = plots.iter().collect();
    // overlay one vector: the requested one, else the first measured one
    let vec1 = spec.vectors.first().cloned().or_else(|| res.summary["measurements"].as_array().and_then(|m| m.first()).and_then(|m| m["vector"].as_str().map(|s| s.to_string())));
    let mut d = crate::diagram::DiagramSpec { vectors: vec1.into_iter().collect(), markers: vec![], ..Default::default() };
    d.title = Some(format!("Monte Carlo – {} runs", ok.len()));
    if refs.first().map(|p| p.kind == "op").unwrap_or(false) {
        return Err("operating point: see the statistics (no curve to overlay)".into());
    }
    d.phase = Some(false);
    Ok(crate::diagram::diagram_svg(&refs, &d, 900.0, 460.0)?.svg)
}

/// Histogram of the first measurement.
pub fn histogram_svg(res: &McResult, spec: &McSpec, vector: &str, at: Option<f64>) -> Option<String> {
    let ok: Vec<&SimResult> = res.runs.iter().filter_map(|r| r.1.as_ref().ok()).filter(|r| r.ok).collect();
    let mut xs = Vec::new();
    let mut db = false;
    for r in &ok {
        let p = r.find(spec.analysis.as_deref()).ok()?;
        let x = at.unwrap_or_else(|| p.scale().map(|s| *s.re.last().unwrap_or(&0.0)).unwrap_or(0.0));
        if let Some((v, d)) = value_at(p, vector, x) {
            xs.push(v);
            db = d;
        }
    }
    if xs.len() < 2 {
        return None;
    }
    let (lo, hi) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
    let span = if hi > lo { hi - lo } else { 1.0 };
    let nb = ((xs.len() as f64).sqrt().ceil() as usize).clamp(5, 30);
    let mut bins = vec![0usize; nb];
    for x in &xs {
        let b = (((x - lo) / span) * nb as f64).floor() as usize;
        bins[b.min(nb - 1)] += 1;
    }
    let (w, h, ml, mb, mt) = (900.0, 320.0, 60.0, 50.0, 40.0);
    let pw = w - ml - 20.0;
    let ph = h - mb - mt;
    let maxc = *bins.iter().max().unwrap() as f64;
    let mut s = format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" font-family="Segoe UI, Arial, sans-serif"><rect width="{w}" height="{h}" fill="#fff"/>"##);
    let title = format!("{vector}{} @ {} — {} runs", if db { " [dB]" } else { "" }, at.map(crate::plot::eng).unwrap_or_else(|| "end".into()), xs.len());
    s.push_str(&format!(r##"<text x="{}" y="24" font-size="15" font-weight="bold" fill="#222" text-anchor="middle">{}</text>"##, w / 2.0, title.replace('<', "&lt;")));
    for (i, &c) in bins.iter().enumerate() {
        let bx = ml + pw * i as f64 / nb as f64;
        let bh = ph * c as f64 / maxc.max(1.0);
        s.push_str(&format!(r##"<rect x="{bx:.1}" y="{:.1}" width="{:.1}" height="{bh:.1}" fill="#1f77b4" fill-opacity="0.75" stroke="#fff"/>"##, mt + ph - bh, pw / nb as f64));
    }
    s.push_str(&format!(r##"<line x1="{ml}" y1="{0}" x2="{1}" y2="{0}" stroke="#555"/>"##, mt + ph, ml + pw));
    for k in 0..=4 {
        let v = lo + span * k as f64 / 4.0;
        let x = ml + pw * k as f64 / 4.0;
        s.push_str(&format!(r##"<text x="{x:.1}" y="{:.1}" font-size="11" fill="#444" text-anchor="middle">{}</text>"##, mt + ph + 16.0, crate::plot::eng(v)));
    }
    s.push_str(&format!(r##"<text x="{}" y="{:.1}" font-size="11" fill="#444" text-anchor="end">{}</text>"##, ml - 6.0, mt + 8.0, maxc as usize));
    s.push_str("</svg>");
    Some(s)
}

/// Parse MCP / GUI arguments.
pub fn spec_from_json(a: &Value) -> McSpec {
    let mut s = McSpec::default();
    if let Some(n) = a.get("runs").and_then(|v| v.as_u64()) {
        s.runs = n as usize;
    }
    if let Some(o) = a.get("tolerances").and_then(|v| v.as_object()) {
        for (k, v) in o {
            if let Some(t) = v.as_f64() {
                if k.len() == 1 {
                    s.default_tol.insert(k.chars().next().unwrap().to_ascii_uppercase(), t);
                } else {
                    s.tol.insert(k.clone(), t);
                }
            }
        }
    }
    s.gauss = a.get("distribution").and_then(|v| v.as_str()).map(|d| !d.eq_ignore_ascii_case("uniform")).unwrap_or(true);
    if let Some(n) = a.get("seed").and_then(|v| v.as_u64()) {
        s.seed = n;
    }
    s.analysis = a.get("analysis").and_then(|v| v.as_str()).map(|x| x.to_string());
    s.vectors = a.get("vectors").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
    s.at = a.get("at").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_f64().or_else(|| x.as_str().and_then(crate::rftools::parse_value))).collect()).unwrap_or_default();
    if let Some(l) = a.get("limits").and_then(|v| v.as_array()) {
        for e in l {
            let vec = e.get("vector").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let at = e.get("at").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(crate::rftools::parse_value))).unwrap_or(0.0);
            s.limits.push((vec, at, e.get("min").and_then(|v| v.as_f64()), e.get("max").and_then(|v| v.as_f64())));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spice_numbers() {
        assert_eq!(spice_num("978.5N"), Some(978.5e-9));
        assert_eq!(spice_num("10k"), Some(10e3));
        assert_eq!(spice_num("1meg"), Some(1e6));
        assert_eq!(spice_num("2M"), Some(2e-3));
        assert_eq!(spice_num("1e-9"), Some(1e-9));
        assert_eq!(spice_num("50"), Some(50.0));
    }
    #[test]
    fn finds_elements() {
        let net = "* t\nR1 a b 10k tc1=0\nC1 b 0 1N\nV1 a 0 DC 1\n.control\nlet r = 1\n.endc\n";
        let e = variable_elements(net);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].1, "R1");
        assert_eq!(e[1].3, 1e-9);
    }
    #[test]
    fn gauss_spread() {
        let mut r = Rng(7);
        let xs: Vec<f64> = (0..20000).map(|_| r.gauss()).collect();
        let m = xs.iter().sum::<f64>() / xs.len() as f64;
        let sd = (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / xs.len() as f64).sqrt();
        assert!(m.abs() < 0.03 && (sd - 1.0).abs() < 0.03, "{m} {sd}");
    }
}
