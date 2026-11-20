//! RF design calculators equivalent to the stand-alone tools that ship with
//! Qucs-S: `qucs-sfilter` (lumped LC ladder filters), `qucs-sattenuator`
//! (resistive attenuators) and `qucs-strans` (microstrip / coax line
//! calculator).  Everything here is a pure function; the results are
//! [`Design`]s (a list of parts with nets) that the editor or the MCP server
//! can place and wire.
//!
//! Sources of the formulas:
//! * Filter prototypes: G. Matthaei, L. Young, E. M. T. Jones, *Microwave
//!   Filters, Impedance-Matching Networks and Coupling Structures* (1964),
//!   ch. 4 (Butterworth / Chebyshev closed forms, Bessel table 4.07-1) and
//!   D. M. Pozar, *Microwave Engineering*, ch. 8 (impedance / frequency
//!   scaling and LP→HP/BP/BS transformations).
//! * Attenuators: the same closed forms as `qucs-sattenuator`
//!   (`attenuatorfunc.cpp`), i.e. standard Pi/Tee pads for unequal
//!   impedances and the bridged-T pad.
//! * Microstrip: E. Hammerstad, Ø. Jensen, "Accurate Models for Microstrip
//!   Computer-Aided Design", MTT-S 1980 (static Z0/εeff with thickness
//!   correction); M. Kirschning, R. H. Jansen, Electronics Letters 1982
//!   (εeff dispersion).  The frequency dependence of Z0 uses the
//!   Bianco/Getsinger relation Z0(f) = Z0·√(εe0/εe(f))·(εe(f)−1)/(εe0−1).

use serde::Serialize;
use serde_json::json;
use std::f64::consts::PI;

/// Free-space wave impedance (Ω).
pub const ETA0: f64 = 376.730_313_668;
/// Speed of light in vacuum (m/s).
pub const C0: f64 = 299_792_458.0;

/// A component to place: Qucs type, property values by name, and its two
/// (or more) terminal nets.
#[derive(Clone, Debug, Serialize)]
pub struct Part {
    pub typ: String,
    pub name: String,
    pub props: Vec<(String, String)>,
    pub nets: Vec<String>,
}

/// Result of a calculator: parts to place, warnings, and raw numbers.
#[derive(Clone, Debug, Serialize)]
pub struct Design {
    pub title: String,
    pub parts: Vec<Part>,
    pub notes: Vec<String>,
    pub values: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Value formatting / parsing
// ---------------------------------------------------------------------------

const PREFIXES: [(f64, &str); 11] = [
    (1e-15, "f"),
    (1e-12, "p"),
    (1e-9, "n"),
    (1e-6, "u"),
    (1e-3, "m"),
    (1.0, ""),
    (1e3, "k"),
    (1e6, "M"),
    (1e9, "G"),
    (1e12, "T"),
    (1e15, "P"),
];

/// Engineering notation as Qucs writes it, 4 significant digits:
/// `format_value(3.1831e-9, "F") == "3.183 nF"`, `format_value(50.0, "Ohm")
/// == "50 Ohm"`.
pub fn format_value(v: f64, unit: &str) -> String {
    if !v.is_finite() {
        return format!("{v} {unit}").trim_end().to_string();
    }
    if v == 0.0 {
        return format!("0 {unit}").trim_end().to_string();
    }
    let a = v.abs();
    // pick the prefix so that the mantissa lies in [1, 1000)
    let mut idx = PREFIXES
        .iter()
        .rposition(|(s, _)| a >= *s * (1.0 - 1e-12))
        .unwrap_or(0);
    let mut m = v / PREFIXES[idx].0;
    let mut text = sig4(m);
    // rounding may have produced 1000 -> move to the next prefix
    if text.trim_start_matches('-').starts_with("1000") && idx + 1 < PREFIXES.len() {
        idx += 1;
        m = v / PREFIXES[idx].0;
        text = sig4(m);
    }
    let unit_full = format!("{}{}", PREFIXES[idx].1, unit);
    if unit_full.is_empty() {
        text
    } else {
        format!("{text} {unit_full}")
    }
}

/// Mantissa with 4 significant digits, trailing zeros removed.
fn sig4(m: f64) -> String {
    let a = m.abs();
    let decimals = if a >= 1000.0 {
        0
    } else if a >= 100.0 {
        1
    } else if a >= 10.0 {
        2
    } else {
        3
    };
    let mut s = format!("{:.*}", decimals, m);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// Inverse of [`format_value`]; accepts SPICE/Qucs style values such as
/// `"10k"`, `"1.5 nF"`, `"2.2u"`, `"1e-9"`, `"1 MHz"`, `"4.7 kOhm"`,
/// `"1.6 mm"`, `"10Meg"`.  `m` is milli, `M` and `Meg` are mega.
pub fn parse_value(s: &str) -> Option<f64> {
    let s = s.trim();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits_start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    // exponent: e/E followed by an optional sign and at least one digit
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let num: f64 = s[..i].parse().ok()?;
    let rest = s[i..].trim_start();
    let (scale, unit) = if let Some(u) = rest
        .strip_prefix("Meg")
        .or_else(|| rest.strip_prefix("meg"))
        .or_else(|| rest.strip_prefix("MEG"))
    {
        (1e6, u)
    } else if let Some(u) = rest.strip_prefix('µ').or_else(|| rest.strip_prefix('μ')) {
        (1e-6, u)
    } else {
        let mut chars = rest.chars();
        match chars.next() {
            None => (1.0, ""),
            Some(c) => {
                let sc = match c {
                    'f' => Some(1e-15),
                    'p' => Some(1e-12),
                    'n' => Some(1e-9),
                    'u' => Some(1e-6),
                    'm' => Some(1e-3),
                    'k' | 'K' => Some(1e3),
                    'M' => Some(1e6),
                    'G' => Some(1e9),
                    'T' => Some(1e12),
                    'P' => Some(1e15),
                    _ => None,
                };
                match sc {
                    Some(sc) => (sc, chars.as_str()),
                    None => (1.0, rest),
                }
            }
        }
    };
    // whatever follows must be a plain unit name (letters only)
    if !unit.chars().all(|c| c.is_alphabetic()) {
        return None;
    }
    Some(num * scale)
}

// ---------------------------------------------------------------------------
// Design builder
// ---------------------------------------------------------------------------

struct Builder {
    parts: Vec<Part>,
    counts: [usize; 3], // R, C, L
    net_count: usize,
    elements: Vec<serde_json::Value>,
}

impl Builder {
    fn new() -> Self {
        Builder { parts: Vec::new(), counts: [0; 3], net_count: 0, elements: Vec::new() }
    }

    fn net(&mut self) -> String {
        self.net_count += 1;
        format!("n{}", self.net_count)
    }

    fn add(&mut self, typ: &str, value: f64, a: &str, b: &str) {
        let (k, unit) = match typ {
            "R" => (0, "Ohm"),
            "C" => (1, "F"),
            _ => (2, "H"),
        };
        self.counts[k] += 1;
        let name = format!("{typ}{}", self.counts[k]);
        let text = format_value(value, unit);
        self.elements.push(json!({
            "name": name, "type": typ, "value": value, "text": text, "nets": [a, b]
        }));
        self.parts.push(Part {
            typ: typ.to_string(),
            name,
            props: vec![(typ.to_string(), text)],
            nets: vec![a.to_string(), b.to_string()],
        });
    }
}

// ---------------------------------------------------------------------------
// LC filter synthesis
// ---------------------------------------------------------------------------

/// Approximation used for the lowpass prototype.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterResponse {
    Butterworth,
    Chebyshev { ripple_db: f64 },
    Bessel,
}

/// Filter class; `f1` is the (3 dB / ripple) corner, for band filters `f1`
/// and `f2` are the lower and upper band edges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterClass {
    LowPass,
    HighPass,
    BandPass,
    BandStop,
}

/// Specification of an LC ladder filter.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterSpec {
    pub response: FilterResponse,
    pub class: FilterClass,
    pub order: usize,
    pub f1: f64,
    /// Upper band edge for band-pass / band-stop.
    pub f2: Option<f64>,
    pub z0: f64,
    /// `true`: the first element is a series branch (T start),
    /// `false`: a shunt branch (PI start).
    pub first_series: bool,
}

/// Bessel (maximally flat delay) prototype values for equal terminations,
/// normalized to unit group delay (Matthaei/Young/Jones table 4.07-1).
/// The last entry of each row is g_{n+1} = 1 and omitted.
const BESSEL_DELAY: [&[f64]; 10] = [
    &[2.0000],
    &[1.5774, 0.4226],
    &[1.2550, 0.5528, 0.1922],
    &[1.0598, 0.5116, 0.3181, 0.1104],
    &[0.9303, 0.4577, 0.3312, 0.2090, 0.0718],
    &[0.8377, 0.4116, 0.3158, 0.2364, 0.1480, 0.0505],
    &[0.7677, 0.3744, 0.2944, 0.2378, 0.1778, 0.1104, 0.0375],
    &[0.7125, 0.3446, 0.2735, 0.2297, 0.1867, 0.1387, 0.0855, 0.0289],
    &[0.6678, 0.3203, 0.2547, 0.2184, 0.1859, 0.1506, 0.1111, 0.0682, 0.0230],
    &[0.6305, 0.3002, 0.2384, 0.2066, 0.1808, 0.1539, 0.1240, 0.0911, 0.0557, 0.0187],
];

/// 3 dB frequency (rad/s) of the unit-delay Bessel lowpass of order `n`,
/// found from the reverse Bessel polynomial θ_n(s).
pub fn bessel_3db_omega(n: usize) -> f64 {
    // coefficients a_k of s^k: (2n-k)! / (2^(n-k) k! (n-k)!)
    let fact = |m: usize| (1..=m).fold(1.0f64, |p, x| p * x as f64);
    let coef: Vec<f64> = (0..=n)
        .map(|k| fact(2 * n - k) / (2f64.powi((n - k) as i32) * fact(k) * fact(n - k)))
        .collect();
    let mag2 = |w: f64| {
        // θ(jw) = Σ a_k (jw)^k
        let (mut re, mut im) = (0.0, 0.0);
        for (k, a) in coef.iter().enumerate() {
            let p = a * w.powi(k as i32);
            match k % 4 {
                0 => re += p,
                1 => im += p,
                2 => re -= p,
                _ => im -= p,
            }
        }
        (coef[0] * coef[0]) / (re * re + im * im)
    };
    let (mut lo, mut hi) = (1e-3, 20.0);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if mag2(mid) > 0.5 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Normalized lowpass prototype values g_1..g_{n+1} (g_0 = 1), cutoff
/// 1 rad/s (3 dB for Butterworth/Bessel, ripple edge for Chebyshev).
pub fn prototype_g(response: FilterResponse, n: usize) -> Result<Vec<f64>, String> {
    if n == 0 {
        return Err("filter order must be >= 1".into());
    }
    match response {
        FilterResponse::Butterworth => {
            if n > 20 {
                return Err("Butterworth order must be <= 20".into());
            }
            let mut g: Vec<f64> =
                (1..=n).map(|k| 2.0 * ((2 * k - 1) as f64 * PI / (2 * n) as f64).sin()).collect();
            g.push(1.0);
            Ok(g)
        }
        FilterResponse::Chebyshev { ripple_db } => {
            if !(ripple_db > 0.0 && ripple_db.is_finite()) {
                return Err("Chebyshev ripple must be > 0 dB".into());
            }
            if n > 20 {
                return Err("Chebyshev order must be <= 20".into());
            }
            let beta = (1.0 / (ripple_db / 17.371_779_276).tanh()).ln();
            let gamma = (beta / (2 * n) as f64).sinh();
            let a: Vec<f64> =
                (1..=n).map(|k| ((2 * k - 1) as f64 * PI / (2 * n) as f64).sin()).collect();
            let b: Vec<f64> =
                (1..=n).map(|k| gamma * gamma + (k as f64 * PI / n as f64).sin().powi(2)).collect();
            let mut g = vec![2.0 * a[0] / gamma];
            for k in 1..n {
                let prev = g[k - 1];
                g.push(4.0 * a[k - 1] * a[k] / (b[k - 1] * prev));
            }
            g.push(if n % 2 == 1 { 1.0 } else { (1.0 / (beta / 4.0).tanh()).powi(2) });
            Ok(g)
        }
        FilterResponse::Bessel => {
            if n > 10 {
                return Err("Bessel order must be <= 10 (tabulated values)".into());
            }
            let w3 = bessel_3db_omega(n);
            let mut g: Vec<f64> = BESSEL_DELAY[n - 1].iter().map(|x| x * w3).collect();
            g.push(1.0);
            Ok(g)
        }
    }
}

/// Synthesize a lumped LC ladder filter (like `qucs-sfilter`).
///
/// Nets: `in`, `out`, `gnd` and internal `n1`, `n2`, ...  The design does not
/// include the source / load terminations; the required load resistance is
/// reported in `values.z_load` (differs from `z0` for even-order Chebyshev).
pub fn lc_filter(spec: &FilterSpec) -> Result<Design, String> {
    let n = spec.order;
    if !(spec.z0 > 0.0 && spec.z0.is_finite()) {
        return Err("z0 must be > 0".into());
    }
    if !(spec.f1 > 0.0 && spec.f1.is_finite()) {
        return Err("f1 must be > 0".into());
    }
    let band = matches!(spec.class, FilterClass::BandPass | FilterClass::BandStop);
    let f2 = if band {
        match spec.f2 {
            Some(f2) if f2 > spec.f1 && f2.is_finite() => f2,
            _ => return Err("band-pass/band-stop needs f2 > f1".into()),
        }
    } else {
        0.0
    };
    let g = prototype_g(spec.response, n)?;
    let z0 = spec.z0;
    let w1 = 2.0 * PI * spec.f1;
    let w2 = 2.0 * PI * f2;
    let w0 = (w1 * w2).sqrt();
    let bw = w2 - w1;

    let series_at = |k: usize| (k % 2 == 0) == spec.first_series; // k is 0-based
    let n_series = (0..n).filter(|&k| series_at(k)).count();

    let mut notes = Vec::new();
    let mut bld = Builder::new();
    let mut cur = "in".to_string();
    let mut series_seen = 0;
    for (k, &gk) in g.iter().take(n).enumerate() {
        if series_at(k) {
            series_seen += 1;
            let next = if series_seen == n_series { "out".to_string() } else { bld.net() };
            match spec.class {
                FilterClass::LowPass => bld.add("L", gk * z0 / w1, &cur, &next),
                FilterClass::HighPass => bld.add("C", 1.0 / (gk * z0 * w1), &cur, &next),
                FilterClass::BandPass => {
                    // series resonator
                    let mid = bld.net();
                    bld.add("L", gk * z0 / bw, &cur, &mid);
                    bld.add("C", bw / (w0 * w0 * gk * z0), &mid, &next);
                }
                FilterClass::BandStop => {
                    // parallel resonator in the series branch
                    bld.add("L", gk * z0 * bw / (w0 * w0), &cur, &next);
                    bld.add("C", 1.0 / (gk * z0 * bw), &cur, &next);
                }
            }
            cur = next;
        } else {
            match spec.class {
                FilterClass::LowPass => bld.add("C", gk / (z0 * w1), &cur, "gnd"),
                FilterClass::HighPass => bld.add("L", z0 / (gk * w1), &cur, "gnd"),
                FilterClass::BandPass => {
                    // parallel resonator to ground
                    bld.add("L", z0 * bw / (w0 * w0 * gk), &cur, "gnd");
                    bld.add("C", gk / (z0 * bw), &cur, "gnd");
                }
                FilterClass::BandStop => {
                    // series resonator to ground
                    let mid = bld.net();
                    bld.add("L", z0 / (gk * bw), &cur, &mid);
                    bld.add("C", gk * bw / (w0 * w0 * z0), &mid, "gnd");
                }
            }
        }
    }
    if n_series == 0 {
        notes.push("single shunt element: input and output are the same node \"in\"".into());
    }

    // load termination: g_{n+1} is a resistance after a shunt element and a
    // conductance after a series element
    let gl = g[n];
    let z_load = if series_at(n - 1) { z0 / gl } else { z0 * gl };
    if (z_load - z0).abs() > 1e-9 * z0 {
        notes.push(format!(
            "even-order Chebyshev: the prototype needs unequal terminations; terminate the output with {} (source {})",
            format_value(z_load, "Ohm"),
            format_value(z0, "Ohm")
        ));
    }
    if band && (f2 / spec.f1) > 3.0 {
        notes.push("wide band (f2/f1 > 3): lumped band transformation still exact, but consider a LP+HP cascade".into());
    }

    let (resp, ripple) = match spec.response {
        FilterResponse::Butterworth => ("Butterworth", None),
        FilterResponse::Chebyshev { ripple_db } => ("Chebyshev", Some(ripple_db)),
        FilterResponse::Bessel => ("Bessel", None),
    };
    let class = match spec.class {
        FilterClass::LowPass => "low-pass",
        FilterClass::HighPass => "high-pass",
        FilterClass::BandPass => "band-pass",
        FilterClass::BandStop => "band-stop",
    };
    let title = if band {
        format!(
            "{resp} {class}, order {n}, {} .. {}, {}",
            format_value(spec.f1, "Hz"),
            format_value(f2, "Hz"),
            format_value(z0, "Ohm")
        )
    } else {
        format!("{resp} {class}, order {n}, {}, {}", format_value(spec.f1, "Hz"), format_value(z0, "Ohm"))
    };
    let values = json!({
        "response": resp,
        "ripple_db": ripple,
        "class": class,
        "order": n,
        "f1": spec.f1,
        "f2": if band { Some(f2) } else { None },
        "f0": if band { Some(w0 / (2.0 * PI)) } else { None },
        "z0": z0,
        "z_load": z_load,
        "topology": if spec.first_series { "T (series first)" } else { "PI (shunt first)" },
        "g": g,
        "elements": bld.elements,
    });
    Ok(Design { title, parts: bld.parts, notes, values })
}

// ---------------------------------------------------------------------------
// Attenuators
// ---------------------------------------------------------------------------

/// Resistive attenuator topologies (as in `qucs-sattenuator`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AttenuatorTopology {
    Pi,
    Tee,
    BridgedTee,
}

/// Minimum attenuation (dB) of a resistive pad matching `z_in` to `z_out`.
pub fn attenuator_min_loss_db(z_in: f64, z_out: f64) -> f64 {
    let r = z_in.max(z_out) / z_in.min(z_out);
    20.0 * (r.sqrt() + (r - 1.0).sqrt()).log10()
}

/// Resistive attenuator design.  Pi and Tee support `z_in != z_out` (the
/// attenuation must be at least [`attenuator_min_loss_db`]); the bridged-T
/// requires equal impedances.  Nets: `in`, `out`, `gnd` (and `n1`).
pub fn attenuator(topology: AttenuatorTopology, atten_db: f64, z_in: f64, z_out: f64) -> Result<Design, String> {
    if !(atten_db > 0.0 && atten_db.is_finite()) {
        return Err("attenuation must be > 0 dB".into());
    }
    if !(z_in > 0.0 && z_out > 0.0 && z_in.is_finite() && z_out.is_finite()) {
        return Err("impedances must be > 0".into());
    }
    let lmin = attenuator_min_loss_db(z_in, z_out);
    let mut notes = Vec::new();
    if (z_in - z_out).abs() > 1e-12 * z_in {
        notes.push(format!(
            "unequal impedances: minimum possible loss is {:.3} dB",
            lmin
        ));
        if atten_db < lmin - 1e-9 {
            return Err(format!(
                "attenuation {atten_db} dB is below the minimum loss {lmin:.3} dB for {} -> {}",
                format_value(z_in, "Ohm"),
                format_value(z_out, "Ohm")
            ));
        }
    }
    let lp = 10f64.powf(atten_db / 10.0); // power ratio
    let mut bld = Builder::new();
    let (name, vals) = match topology {
        AttenuatorTopology::Pi => {
            let r2 = (lp - 1.0) / 2.0 * (z_in * z_out / lp).sqrt();
            let r1 = 1.0 / ((lp + 1.0) / (z_in * (lp - 1.0)) - 1.0 / r2);
            let r3 = 1.0 / ((lp + 1.0) / (z_out * (lp - 1.0)) - 1.0 / r2);
            check_positive(&[r1, r2, r3])?;
            bld.add("R", r1, "in", "gnd");
            bld.add("R", r2, "in", "out");
            bld.add("R", r3, "out", "gnd");
            ("Pi", json!({"r_shunt_in": r1, "r_series": r2, "r_shunt_out": r3}))
        }
        AttenuatorTopology::Tee => {
            let r2 = 2.0 * (lp * z_in * z_out).sqrt() / (lp - 1.0);
            let r1 = z_in * (lp + 1.0) / (lp - 1.0) - r2;
            let r3 = z_out * (lp + 1.0) / (lp - 1.0) - r2;
            check_positive(&[r1, r2, r3])?;
            let mid = bld.net();
            bld.add("R", r1, "in", &mid);
            bld.add("R", r2, &mid, "gnd");
            bld.add("R", r3, &mid, "out");
            ("Tee", json!({"r_series_in": r1, "r_shunt": r2, "r_series_out": r3}))
        }
        AttenuatorTopology::BridgedTee => {
            if (z_in - z_out).abs() > 1e-12 * z_in {
                return Err("bridged-T attenuator requires z_in == z_out".into());
            }
            let z = z_in;
            let k = 10f64.powf(atten_db / 20.0); // voltage ratio
            let rb = z * (k - 1.0);
            let rs = z / (k - 1.0);
            let mid = bld.net();
            bld.add("R", z, "in", &mid);
            bld.add("R", z, &mid, "out");
            bld.add("R", rb, "in", "out");
            bld.add("R", rs, &mid, "gnd");
            ("Bridged-T", json!({"r_arm": z, "r_bridge": rb, "r_shunt": rs}))
        }
    };
    let mut values = vals;
    values["atten_db"] = json!(atten_db);
    values["z_in"] = json!(z_in);
    values["z_out"] = json!(z_out);
    values["min_loss_db"] = json!(lmin);
    values["elements"] = json!(bld.elements);
    Ok(Design {
        title: format!(
            "{name} attenuator {atten_db} dB, {} -> {}",
            format_value(z_in, "Ohm"),
            format_value(z_out, "Ohm")
        ),
        parts: bld.parts,
        notes,
        values,
    })
}

fn check_positive(r: &[f64]) -> Result<(), String> {
    if r.iter().all(|x| *x > 0.0 && x.is_finite()) {
        Ok(())
    } else {
        Err("no realizable (positive) resistor values for this attenuation".into())
    }
}

// ---------------------------------------------------------------------------
// Transmission lines
// ---------------------------------------------------------------------------

/// Microstrip substrate; all lengths in metres, `freq` in Hz (0 = static).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Microstrip {
    pub er: f64,
    pub h: f64,
    pub t: f64,
    pub freq: f64,
}

/// Hammerstad-Jensen line impedance of a zero-thickness strip in air.
fn hj_z01(u: f64) -> f64 {
    let f = 6.0 + (2.0 * PI - 6.0) * (-(30.666 / u).powf(0.7528)).exp();
    ETA0 / (2.0 * PI) * (f / u + (1.0 + 4.0 / (u * u)).sqrt()).ln()
}

/// Hammerstad-Jensen effective permittivity of a zero-thickness strip.
fn hj_eeff(u: f64, er: f64) -> f64 {
    let a = 1.0
        + ((u.powi(4) + (u / 52.0).powi(2)) / (u.powi(4) + 0.432)).ln() / 49.0
        + (1.0 + (u / 18.1).powi(3)).ln() / 18.7;
    let b = 0.564 * ((er - 0.9) / (er + 3.0)).powf(0.053);
    (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * (1.0 + 10.0 / u).powf(-a * b)
}

/// Static Z0 and εeff including the strip-thickness correction.
fn microstrip_static(m: &Microstrip, w: f64) -> (f64, f64) {
    let u = w / m.h;
    let (u1, ur) = if m.t > 0.0 {
        let th = m.t / m.h;
        let coth = 1.0 / (6.517 * u).sqrt().tanh();
        let du1 = th / PI * (1.0 + 4.0 * std::f64::consts::E / (th * coth * coth)).ln();
        let dur = 0.5 * (1.0 + 1.0 / (m.er - 1.0).sqrt().cosh()) * du1;
        (u + du1, u + dur)
    } else {
        (u, u)
    };
    let eeff_r = hj_eeff(ur, m.er);
    let z0 = hj_z01(ur) / eeff_r.sqrt();
    let eeff = eeff_r * (hj_z01(u1) / hj_z01(ur)).powi(2);
    (z0, eeff)
}

/// Microstrip analysis: returns (Z0 in Ω, εeff) for strip width `w` (m).
///
/// Hammerstad-Jensen static model with thickness correction; when
/// `m.freq > 0` εeff is dispersed with Kirschning-Jansen and Z0 with the
/// Bianco/Getsinger relation.
pub fn microstrip_analyze(m: &Microstrip, w: f64) -> (f64, f64) {
    let (z0s, e0) = microstrip_static(m, w);
    if !(m.freq > 0.0) {
        return (z0s, e0);
    }
    let u = w / m.h;
    let er = m.er;
    let fn_ = m.freq * 1e-9 * m.h * 1e3; // GHz * mm
    let p1 = 0.27488 + (0.6315 + 0.525 / (1.0 + 0.0157 * fn_).powi(20)) * u
        - 0.065683 * (-8.7513 * u).exp();
    let p2 = 0.33622 * (1.0 - (-0.03442 * er).exp());
    let p3 = 0.0363 * (-4.6 * u).exp() * (1.0 - (-(fn_ / 38.7).powf(4.97)).exp());
    let p4 = 1.0 + 2.751 * (1.0 - (-(er / 15.916).powi(8)).exp());
    let p = p1 * p2 * ((0.1844 + p3 * p4) * fn_).powf(1.5763);
    let ef = er - (er - e0) / (1.0 + p);
    let z0f = if e0 > 1.0 { z0s * (e0 / ef).sqrt() * (ef - 1.0) / (e0 - 1.0) } else { z0s };
    (z0f, ef)
}

/// Microstrip synthesis: strip width (m) for the wanted `z0`, solved by
/// bisection on log(w) over w/h = 1e-3 .. 1e3.
pub fn microstrip_synthesize(m: &Microstrip, z0: f64) -> f64 {
    let (mut lo, mut hi) = ((1e-3 * m.h).ln(), (1e3 * m.h).ln());
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        // Z0 decreases with width
        if microstrip_analyze(m, mid.exp()).0 > z0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (0.5 * (lo + hi)).exp()
}

/// Characteristic impedance of a coaxial line (diameters in any unit).
pub fn coax_z0(er: f64, d_inner: f64, d_outer: f64) -> f64 {
    ETA0 / (2.0 * PI * er.sqrt()) * (d_outer / d_inner).ln()
}

/// Electrical length in degrees of a line of `length` metres.
pub fn electrical_length_deg(length: f64, freq: f64, eps_eff: f64) -> f64 {
    360.0 * length * freq * eps_eff.sqrt() / C0
}

/// Physical length (m) for a given electrical length in degrees.
pub fn physical_length(deg: f64, freq: f64, eps_eff: f64) -> f64 {
    deg / 360.0 * C0 / (freq * eps_eff.sqrt())
}

// ---------------------------------------------------------------------------
// Small AC analysis of an R/L/C design (for verification)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct Cx(f64, f64);

impl Cx {
    fn add(self, o: Cx) -> Cx {
        Cx(self.0 + o.0, self.1 + o.1)
    }
    fn sub(self, o: Cx) -> Cx {
        Cx(self.0 - o.0, self.1 - o.1)
    }
    fn mul(self, o: Cx) -> Cx {
        Cx(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }
    fn div(self, o: Cx) -> Cx {
        let d = o.0 * o.0 + o.1 * o.1;
        Cx((self.0 * o.0 + self.1 * o.1) / d, (self.1 * o.0 - self.0 * o.1) / d)
    }
    fn abs(self) -> f64 {
        self.0.hypot(self.1)
    }
}

/// Nodal AC analysis of a design made of R/L/C parts between the nets `in`
/// and `out` (ground = `gnd`), driven from `z_src` and loaded with `z_load`
/// (both real).  Returns (|S11| dB, |S21| dB) at `freq` (Hz, > 0).
pub fn two_port_s_db(design: &Design, freq: f64, z_src: f64, z_load: f64) -> Result<(f64, f64), String> {
    let mut nets: Vec<String> = Vec::new();
    let idx = |n: &str, nets: &mut Vec<String>| -> Option<usize> {
        if n == "gnd" {
            return None;
        }
        Some(match nets.iter().position(|x| x == n) {
            Some(i) => i,
            None => {
                nets.push(n.to_string());
                nets.len() - 1
            }
        })
    };
    let i_in = idx("in", &mut nets).unwrap_or(0);
    let mut stamps = Vec::new();
    for p in &design.parts {
        if p.nets.len() != 2 {
            return Err(format!("{}: expected 2 nets", p.name));
        }
        let v = p
            .props
            .first()
            .and_then(|(_, s)| parse_value(s))
            .ok_or_else(|| format!("{}: bad value", p.name))?;
        let w = 2.0 * PI * freq;
        let y = match p.typ.as_str() {
            "R" => Cx(1.0 / v, 0.0),
            "C" => Cx(0.0, w * v),
            "L" => Cx(0.0, -1.0 / (w * v)),
            t => return Err(format!("{}: unsupported type {t}", p.name)),
        };
        let a = idx(&p.nets[0], &mut nets);
        let b = idx(&p.nets[1], &mut nets);
        stamps.push((a, b, y));
    }
    let i_out = if design.parts.iter().any(|p| p.nets.iter().any(|n| n == "out")) {
        idx("out", &mut nets).unwrap_or(0)
    } else {
        i_in
    };
    let n = nets.len();
    let mut m = vec![vec![Cx(0.0, 0.0); n + 1]; n];
    for (a, b, y) in stamps {
        if let Some(a) = a {
            m[a][a] = m[a][a].add(y);
        }
        if let Some(b) = b {
            m[b][b] = m[b][b].add(y);
        }
        if let (Some(a), Some(b)) = (a, b) {
            m[a][b] = m[a][b].sub(y);
            m[b][a] = m[b][a].sub(y);
        }
    }
    m[i_in][i_in] = m[i_in][i_in].add(Cx(1.0 / z_src, 0.0));
    m[i_out][i_out] = m[i_out][i_out].add(Cx(1.0 / z_load, 0.0));
    m[i_in][n] = Cx(1.0 / z_src, 0.0); // Norton equivalent of a 1 V source
    // Gaussian elimination with partial pivoting
    for c in 0..n {
        let piv = (c..n)
            .max_by(|&x, &y| m[x][c].abs().total_cmp(&m[y][c].abs()))
            .unwrap_or(c);
        if m[piv][c].abs() < 1e-300 {
            return Err("singular circuit".into());
        }
        m.swap(c, piv);
        for r in 0..n {
            if r != c {
                let f = m[r][c].div(m[c][c]);
                if f.abs() != 0.0 {
                    for k in c..=n {
                        let t = f.mul(m[c][k]);
                        m[r][k] = m[r][k].sub(t);
                    }
                }
            }
        }
    }
    let v = |i: usize| m[i][n].div(m[i][i]);
    let vin = v(i_in);
    let vout = v(i_out);
    let s11 = vin.mul(Cx(2.0, 0.0)).sub(Cx(1.0, 0.0));
    let s21 = vout.mul(Cx(2.0 * (z_src / z_load).sqrt(), 0.0));
    Ok((20.0 * s11.abs().log10(), 20.0 * s21.abs().log10()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs().max(1e-30)
    }

    fn part_value(d: &Design, name: &str) -> f64 {
        let p = d.parts.iter().find(|p| p.name == name).unwrap();
        parse_value(&p.props[0].1).unwrap()
    }

    fn spec(response: FilterResponse, class: FilterClass, order: usize, first_series: bool) -> FilterSpec {
        FilterSpec { response, class, order, f1: 1e9, f2: Some(2e9), z0: 50.0, first_series }
    }

    #[test]
    fn format_values() {
        assert_eq!(format_value(3.18309886e-9, "F"), "3.183 nF");
        assert_eq!(format_value(50.0, "Ohm"), "50 Ohm");
        assert_eq!(format_value(1.2e-6, "H"), "1.2 uH");
        assert_eq!(format_value(1e3, "Ohm"), "1 kOhm");
        assert_eq!(format_value(999.96, "Ohm"), "1 kOhm");
        assert_eq!(format_value(159.16e-12, "F"), "159.2 pF");
        assert_eq!(format_value(15.9155e-9, "H"), "15.92 nH");
        assert_eq!(format_value(0.0, "V"), "0 V");
        assert_eq!(format_value(-2.5e-3, "A"), "-2.5 mA");
        assert_eq!(format_value(1e6, ""), "1 M");
        assert_eq!(format_value(42.0, ""), "42");
        assert_eq!(format_value(f64::INFINITY, ""), "inf");
    }

    #[test]
    fn parse_values() {
        let cases = [
            ("10k", 1e4),
            ("1.5 nF", 1.5e-9),
            ("2.2u", 2.2e-6),
            ("1e-9", 1e-9),
            ("1 MHz", 1e6),
            ("50 Ohm", 50.0),
            ("4.7 kOhm", 4.7e3),
            ("1.6 mm", 1.6e-3),
            ("10Meg", 1e7),
            ("3 µH", 3e-6),
            ("2.5E3", 2.5e3),
            ("-3 dB", -3.0),
            ("1e", 1.0),
            ("7 GHz", 7e9),
            ("1 f", 1e-15),
            ("1 pF", 1e-12),
            ("1T", 1e12),
            ("1 P", 1e15),
            ("1 K", 1e3),
        ];
        for (s, v) in cases {
            let got = parse_value(s).unwrap_or_else(|| panic!("parse {s}"));
            assert!(close(got, v, 1e-12), "{s}: {got} != {v}");
        }
        assert_eq!(parse_value("abc"), None);
        assert_eq!(parse_value(""), None);
        assert_eq!(parse_value("10k5"), None);
    }

    #[test]
    fn round_trip() {
        for &v in &[1.0, 3.183e-12, 15.92e-9, 96.25, 71.15, 2.2e6, 4.7e-15, 123.4e9] {
            for unit in ["F", "H", "Ohm", "Hz", ""] {
                let s = format_value(v, unit);
                let back = parse_value(&s).unwrap();
                assert!(close(back, v, 5e-4), "{v} {unit} -> {s} -> {back}");
            }
        }
    }

    #[test]
    fn butterworth_g() {
        let g = prototype_g(FilterResponse::Butterworth, 3).unwrap();
        for (a, b) in g.iter().zip([1.0, 2.0, 1.0, 1.0]) {
            assert!(close(*a, b, 1e-12));
        }
        assert!(prototype_g(FilterResponse::Butterworth, 0).is_err());
        assert!(prototype_g(FilterResponse::Butterworth, 21).is_err());
    }

    #[test]
    fn chebyshev_g() {
        // Matthaei table 4.05-2(a), 0.5 dB ripple
        let g3 = prototype_g(FilterResponse::Chebyshev { ripple_db: 0.5 }, 3).unwrap();
        for (a, b) in g3.iter().zip([1.5963, 1.0967, 1.5963, 1.0]) {
            assert!(close(*a, b, 2e-4), "{a} {b}");
        }
        let g4 = prototype_g(FilterResponse::Chebyshev { ripple_db: 0.5 }, 4).unwrap();
        for (a, b) in g4.iter().zip([1.6703, 1.1926, 2.3661, 0.8419, 1.9841]) {
            assert!(close(*a, b, 2e-4), "{a} {b}");
        }
        assert!(prototype_g(FilterResponse::Chebyshev { ripple_db: 0.0 }, 3).is_err());
        assert!(prototype_g(FilterResponse::Chebyshev { ripple_db: 1.0 }, 21).is_err());
    }

    #[test]
    fn bessel_g() {
        assert!(close(bessel_3db_omega(1), 1.0, 1e-9));
        assert!(close(bessel_3db_omega(3), 1.7557, 1e-3));
        let g = prototype_g(FilterResponse::Bessel, 2).unwrap();
        assert!(close(g[0], 1.5774 * 1.3617, 1e-3));
        assert!(prototype_g(FilterResponse::Bessel, 11).is_err());
    }

    #[test]
    fn butterworth_lowpass_values() {
        let d = lc_filter(&spec(FilterResponse::Butterworth, FilterClass::LowPass, 3, false)).unwrap();
        assert_eq!(d.parts.len(), 3);
        assert_eq!(d.parts[0].props[0].1, "3.183 pF");
        assert_eq!(d.parts[1].props[0].1, "15.92 nH");
        assert_eq!(d.parts[0].nets, vec!["in", "gnd"]);
        assert_eq!(d.parts[1].nets, vec!["in", "out"]);
        assert_eq!(d.parts[2].nets, vec!["out", "gnd"]);
        assert!(d.notes.is_empty());
        assert_eq!(d.values["elements"].as_array().unwrap().len(), 3);
        // T start
        let t = lc_filter(&spec(FilterResponse::Butterworth, FilterClass::LowPass, 3, true)).unwrap();
        assert!(close(part_value(&t, "L1"), 7.958e-9, 1e-3));
        assert!(close(part_value(&t, "C1"), 6.366e-12, 1e-3));
        assert_eq!(t.parts[1].nets, vec!["n1", "gnd"]);
    }

    /// Every class/response/topology gives the expected response at the
    /// band edges according to the built-in nodal analysis.
    #[test]
    fn filter_responses_check() {
        let resps = [
            (FilterResponse::Butterworth, 3.0103),
            (FilterResponse::Chebyshev { ripple_db: 0.5 }, 0.5),
            (FilterResponse::Chebyshev { ripple_db: 1.0 }, 1.0),
            (FilterResponse::Bessel, 3.0103),
        ];
        let classes = [FilterClass::LowPass, FilterClass::HighPass, FilterClass::BandPass, FilterClass::BandStop];
        for (resp, edge_db) in resps {
            for class in classes {
                for order in 1..=7 {
                    for first_series in [false, true] {
                        let s = spec(resp, class, order, first_series);
                        let mut d = lc_filter(&s).unwrap();
                        // analyze the unrounded element values
                        let exact: Vec<f64> = d.values["elements"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|e| e["value"].as_f64().unwrap())
                            .collect();
                        for (p, v) in d.parts.iter_mut().zip(exact) {
                            p.props[0].1 = format!("{v:e}");
                        }
                        let zl = d.values["z_load"].as_f64().unwrap();
                        let tol = if resp == FilterResponse::Bessel { 0.05 } else { 1e-6 };
                        let edges: &[f64] = match class {
                            FilterClass::LowPass | FilterClass::HighPass => &[1e9],
                            _ => &[1e9, 2e9],
                        };
                        for &f in edges {
                            let (_, s21) = two_port_s_db(&d, f, 50.0, zl).unwrap();
                            assert!(
                                (s21 + edge_db).abs() < tol,
                                "{:?} {:?} n={order} T={first_series} f={f}: {s21}",
                                resp,
                                class
                            );
                        }
                        // pass / stop band checks
                        let f0 = (2e18f64).sqrt();
                        let (pass, stop) = match class {
                            FilterClass::LowPass => (1e7, 1e10),
                            FilterClass::HighPass => (1e11, 1e8),
                            FilterClass::BandPass => (f0, 1e8),
                            FilterClass::BandStop => (1e7, f0),
                        };
                        let (_, sp) = two_port_s_db(&d, pass, 50.0, zl).unwrap();
                        let (_, ss) = two_port_s_db(&d, stop, 50.0, zl).unwrap();
                        assert!(sp > -edge_db - 0.01, "{resp:?} {class:?} n={order}: pass {sp}");
                        assert!(ss < -10.0, "{resp:?} {class:?} n={order}: stop {ss}");
                    }
                }
            }
        }
    }

    #[test]
    fn chebyshev_even_note_and_load() {
        let d = lc_filter(&spec(FilterResponse::Chebyshev { ripple_db: 0.5 }, FilterClass::LowPass, 4, false)).unwrap();
        assert_eq!(d.notes.len(), 1);
        // last element series L -> load = z0 / g5
        assert!(close(d.values["z_load"].as_f64().unwrap(), 50.0 / 1.9841, 1e-3));
        let d = lc_filter(&spec(FilterResponse::Chebyshev { ripple_db: 0.5 }, FilterClass::LowPass, 4, true)).unwrap();
        assert!(close(d.values["z_load"].as_f64().unwrap(), 50.0 * 1.9841, 1e-3));
    }

    #[test]
    fn band_filters_and_errors() {
        let d = lc_filter(&spec(FilterResponse::Butterworth, FilterClass::BandPass, 3, true)).unwrap();
        assert_eq!(d.parts.len(), 6);
        assert!(d.values["f0"].as_f64().unwrap() > 1.41e9);
        let wide = FilterSpec { f2: Some(5e9), ..spec(FilterResponse::Butterworth, FilterClass::BandStop, 3, false) };
        assert!(lc_filter(&wide).unwrap().notes.iter().any(|n| n.contains("wide band")));
        let one = lc_filter(&spec(FilterResponse::Butterworth, FilterClass::LowPass, 1, false)).unwrap();
        assert_eq!(one.notes.len(), 1);
        let (_, s21) = two_port_s_db(&one, 1e9, 50.0, 50.0).unwrap();
        assert!((s21 + 3.0103).abs() < 0.01);
        let mut bad = spec(FilterResponse::Butterworth, FilterClass::BandPass, 3, true);
        bad.f2 = None;
        assert!(lc_filter(&bad).is_err());
        bad.f2 = Some(0.5e9);
        assert!(lc_filter(&bad).is_err());
        let mut bad = spec(FilterResponse::Butterworth, FilterClass::LowPass, 3, true);
        bad.z0 = 0.0;
        assert!(lc_filter(&bad).is_err());
        bad.z0 = 50.0;
        bad.f1 = -1.0;
        assert!(lc_filter(&bad).is_err());
        let titled = lc_filter(&spec(FilterResponse::Bessel, FilterClass::HighPass, 2, true)).unwrap();
        assert!(titled.title.starts_with("Bessel high-pass"));
        // JSON serialisation works
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("\"typ\":\"L\""));
    }

    #[test]
    fn pi_attenuator() {
        let d = attenuator(AttenuatorTopology::Pi, 10.0, 50.0, 50.0).unwrap();
        assert!(close(part_value(&d, "R1"), 96.25, 1e-3));
        assert!(close(part_value(&d, "R2"), 71.15, 1e-3));
        assert!(close(d.values["r_shunt_out"].as_f64().unwrap(), 96.248, 1e-4));
        let (s11, s21) = two_port_s_db(&d, 1e6, 50.0, 50.0).unwrap();
        assert!((s21 + 10.0).abs() < 1e-3 && s11 < -60.0);
        assert!(d.notes.is_empty());
    }

    #[test]
    fn tee_and_bridged() {
        let d = attenuator(AttenuatorTopology::Tee, 10.0, 50.0, 50.0).unwrap();
        assert!(close(d.values["r_series_in"].as_f64().unwrap(), 25.97, 1e-3));
        assert!(close(d.values["r_shunt"].as_f64().unwrap(), 35.14, 1e-3));
        let d = attenuator(AttenuatorTopology::BridgedTee, 6.0, 75.0, 75.0).unwrap();
        let (s11, s21) = two_port_s_db(&d, 1e6, 75.0, 75.0).unwrap();
        assert!((s21 + 6.0).abs() < 1e-3 && s11 < -60.0, "{s11} {s21}");
        assert!(attenuator(AttenuatorTopology::BridgedTee, 6.0, 50.0, 75.0).is_err());
    }

    #[test]
    fn mismatched_attenuators() {
        let lmin = attenuator_min_loss_db(50.0, 75.0);
        assert!(close(lmin, 5.719, 1e-3));
        for top in [AttenuatorTopology::Pi, AttenuatorTopology::Tee] {
            for (zi, zo) in [(50.0, 75.0), (75.0, 50.0)] {
                let d = attenuator(top, 10.0, zi, zo).unwrap();
                assert_eq!(d.notes.len(), 1);
                let (s11, s21) = two_port_s_db(&d, 1e6, zi, zo).unwrap();
                assert!((s21 + 10.0).abs() < 1e-3 && s11 < -60.0, "{top:?}: {s11} {s21}");
                assert!(attenuator(top, 3.0, zi, zo).is_err());
            }
        }
        assert!(attenuator(AttenuatorTopology::Pi, 0.0, 50.0, 50.0).is_err());
        assert!(attenuator(AttenuatorTopology::Pi, 3.0, -50.0, 50.0).is_err());
        assert!(check_positive(&[1.0, -1.0]).is_err());
    }

    #[test]
    fn microstrip_fr4() {
        let m = Microstrip { er: 4.4, h: 1.6e-3, t: 35e-6, freq: 0.0 };
        let w = microstrip_synthesize(&m, 50.0);
        assert!(w > 2.85e-3 && w < 3.15e-3, "w = {w}");
        let (z0, ee) = microstrip_analyze(&m, w);
        assert!((z0 - 50.0).abs() < 1e-6);
        assert!(ee > 3.2 && ee < 3.5, "eeff {ee}");
        // zero thickness: slightly wider line needed
        let m0 = Microstrip { t: 0.0, ..m };
        assert!(microstrip_synthesize(&m0, 50.0) > w);
        // dispersion raises eps_eff
        let mf = Microstrip { freq: 10e9, ..m };
        let (_, ef) = microstrip_analyze(&mf, w);
        assert!(ef > ee && ef < m.er);
        let wf = microstrip_synthesize(&mf, 50.0);
        assert!((microstrip_analyze(&mf, wf).0 - 50.0).abs() < 1e-6);
        // classic check: alumina er=9.8, w/h=1 -> ~48-49 Ohm
        let al = Microstrip { er: 9.8, h: 1e-3, t: 0.0, freq: 0.0 };
        let (z, _) = microstrip_analyze(&al, 1e-3);
        assert!(z > 47.0 && z < 50.0, "{z}");
    }

    #[test]
    fn coax_and_length() {
        // RG-58-ish: PE (2.25), d=0.9 mm, D=2.95 mm -> ~47.5..50 Ohm
        let z = coax_z0(2.25, 0.9, 2.95);
        assert!(z > 47.0 && z < 49.0, "{z}");
        assert!(close(coax_z0(1.0, 1.0, std::f64::consts::E), 59.958, 1e-4));
        let l = physical_length(90.0, 1e9, 1.0);
        assert!(close(l, 0.0749481, 1e-5));
        assert!(close(electrical_length_deg(l, 1e9, 1.0), 90.0, 1e-12));
        assert!(close(electrical_length_deg(0.05, 1e9, 4.0), 120.083, 1e-4));
    }

    #[test]
    fn two_port_errors() {
        let mut d = attenuator(AttenuatorTopology::Pi, 10.0, 50.0, 50.0).unwrap();
        d.parts[0].typ = "Q".into();
        assert!(two_port_s_db(&d, 1e6, 50.0, 50.0).is_err());
        d.parts[0].typ = "R".into();
        d.parts[0].props[0].1 = "xyz".into();
        assert!(two_port_s_db(&d, 1e6, 50.0, 50.0).is_err());
        d.parts[0].nets.push("x".into());
        assert!(two_port_s_db(&d, 1e6, 50.0, 50.0).is_err());
    }
}
