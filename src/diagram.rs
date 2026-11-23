//! Qucs-S style result diagrams rendered to SVG: cartesian (Rect), polar,
//! Smith chart and tabular views, with interpolated marker read-outs.
//!
//! Smith charts expect reflection coefficients (S-parameters, Gamma); any
//! complex vector is drawn as-is, so impedance data must be converted to
//! Gamma = (Z - z0) / (Z + z0) by the caller to be meaningful.

// The module is integrated into the GUI / MCP server separately.

use crate::plot::eng;
use crate::render::NET_PALETTE;
use crate::sim::{Plot, Vector};
use serde_json::{json, Value};
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum DiagramKind {
    Rect,
    Polar,
    Smith,
    Table,
}

#[derive(Clone, Debug)]
pub struct DiagramSpec {
    pub kind: DiagramKind,
    /// empty = sensible default (up to 4 node voltages)
    pub vectors: Vec<String>,
    /// None = auto (log for ac/noise)
    pub x_log: Option<bool>,
    pub y_log: bool,
    /// None = auto (dB for complex vectors in Rect)
    pub db: Option<bool>,
    /// None = auto (second panel with phase for complex vectors in Rect)
    pub phase: Option<bool>,
    /// x positions (Rect/Table) or frequency (Polar/Smith) to mark
    pub markers: Vec<f64>,
    pub x_range: Option<(f64, f64)>,
    /// Rect: y range of the main panel; Polar: (_, max radius)
    pub y_range: Option<(f64, f64)>,
    pub title: Option<String>,
    /// Cartesian comparisons append previous-run plots starting at this index.
    pub previous_start: Option<usize>,
    /// Smith reference impedance, default 50
    pub z0: f64,
}

impl Default for DiagramSpec {
    fn default() -> Self {
        DiagramSpec {
            kind: DiagramKind::Rect,
            vectors: vec![],
            x_log: None,
            y_log: false,
            db: None,
            phase: None,
            markers: vec![],
            x_range: None,
            y_range: None,
            title: None,
            previous_start: None,
            z0: 50.0,
        }
    }
}

impl DiagramSpec {
    pub fn parse_kind(s: &str) -> Option<DiagramKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rect" | "cartesian" | "xy" => Some(DiagramKind::Rect),
            "polar" => Some(DiagramKind::Polar),
            "smith" => Some(DiagramKind::Smith),
            "table" | "tabular" => Some(DiagramKind::Table),
            _ => None,
        }
    }
}

#[allow(dead_code)] // width/height are part of the API for callers that lay the image out
pub struct DiagramOut {
    pub svg: String,
    pub width: f64,
    pub height: f64,
    /// marker read-outs: [{"x":.., "vector":.., "value": .. or {"re","im","mag","db","phase_deg"} , "step": sweep obj or null}]
    pub markers: Value,
    /// pixel mapping of the main plot area so a GUI can convert a click to an x value:
    /// (left, top, width, height, x_min, x_max, x_log)
    pub plot_area: Option<(f64, f64, f64, f64, f64, f64, bool)>,
}

const FONT: &str = "Segoe UI, Arial, sans-serif";
const C_FRAME: &str = "#8a9099";
const C_GRID: &str = "#e1e4e8";
const C_GRID_MINOR: &str = "#f0f2f4";
const C_TEXT: &str = "#333";
const C_TICK: &str = "#404040";
const C_MARKER: &str = "#444";
const LEGEND_FONT: f64 = 13.0;
const LEGEND_LABEL_X: f64 = 36.0;
const LEGEND_ROW: f64 = 18.0;

// ---------------------------------------------------------------- helpers

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Rough text width for layout (Segoe UI / Arial average glyph width).
fn tw(s: &str, size: f64) -> f64 {
    s.chars().count() as f64 * size * 0.56
}

fn nice_step(raw: f64) -> f64 {
    if !(raw > 0.0) || !raw.is_finite() {
        return 1.0;
    }
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw * 0.999).unwrap_or(raw)
}

fn lin_ticks(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    if !(hi > lo) {
        return vec![lo];
    }
    let step = nice_step((hi - lo) / n.max(1) as f64);
    let mut out = Vec::new();
    let mut k = (lo / step - 1e-9).ceil();
    loop {
        let t = k * step;
        if t > hi + step * 1e-9 {
            break;
        }
        out.push(if t.abs() < step * 1e-9 { 0.0 } else { t });
        k += 1.0;
        if out.len() > 200 {
            break;
        }
    }
    out
}

fn sweep_label(p: &Plot) -> String {
    match &p.sweep {
        Some(Value::Object(o)) => o
            .iter()
            .map(|(k, v)| match v.as_f64() {
                Some(f) => format!("{k}={}", eng(f)),
                None => format!("{k}={}", v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())),
            })
            .collect::<Vec<_>>()
            .join(" "),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn unwrap_phase(v: &Vector) -> Vec<f64> {
    let n = v.re.len();
    let mut out = Vec::with_capacity(n);
    let mut off = 0.0;
    let mut prev: Option<f64> = None;
    for i in 0..n {
        let mut a = v.phase_deg(i);
        if let Some(pv) = prev {
            while a + off - pv > 180.0 {
                off -= 360.0;
            }
            while a + off - pv < -180.0 {
                off += 360.0;
            }
        }
        a += off;
        prev = Some(a);
        out.push(a);
    }
    out
}

/// Locate `x0` on the (monotone or piecewise monotone) scale: segment index and fraction.
fn locate(x: &[f64], x0: f64, log: bool) -> Option<(usize, f64)> {
    if x.len() == 1 {
        return if (x[0] - x0).abs() <= 1e-12 * x0.abs().max(1e-300) { Some((0, 0.0)) } else { None };
    }
    for i in 0..x.len().saturating_sub(1) {
        let (a, b) = (x[i], x[i + 1]);
        if (a <= x0 && x0 <= b) || (b <= x0 && x0 <= a) {
            let t = if a == b {
                0.0
            } else if log && a > 0.0 && b > 0.0 && x0 > 0.0 {
                (x0.ln() - a.ln()) / (b.ln() - a.ln())
            } else {
                (x0 - a) / (b - a)
            };
            return Some((i, t));
        }
    }
    None
}

fn lerp(a: &[f64], i: usize, t: f64) -> f64 {
    if t == 0.0 || i + 1 >= a.len() {
        a[i]
    } else {
        a[i] + (a[i + 1] - a[i]) * t
    }
}

/// Interpolated complex value (re, im) of `v` at scale position `x0`.
fn sample(x: &[f64], v: &Vector, x0: f64, log: bool) -> Option<(f64, f64)> {
    let n = x.len().min(v.re.len());
    let (i, t) = locate(&x[..n], x0, log)?;
    let re = lerp(&v.re, i, t);
    let im = v.im.as_ref().map(|im| lerp(im, i, t)).unwrap_or(0.0);
    Some((re, im))
}

fn complex_json(re: f64, im: f64) -> Value {
    let mag = (re * re + im * im).sqrt();
    json!({"re": re, "im": im, "mag": mag, "db": 20.0 * mag.max(1e-300).log10(), "phase_deg": im.atan2(re).to_degrees()})
}

fn fmt_c(re: f64, im: f64) -> String {
    format!("{}{}j{}", eng4(re), if im < 0.0 { "−" } else { "+" }, eng4(im.abs()))
}

fn fmt_polar(re: f64, im: f64) -> String {
    format!("{} / {:.1}°", eng4((re * re + im * im).sqrt()), im.atan2(re).to_degrees())
}

/// Engineering notation with 4 significant digits (for read-outs).
fn eng4(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return if v == 0.0 { "0".into() } else { v.to_string() };
    }
    let a = v.abs();
    let (d, suffix) = [(1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "k"), (1.0, ""), (1e-3, "m"), (1e-6, "µ"), (1e-9, "n"), (1e-12, "p"), (1e-15, "f")]
        .iter()
        .copied()
        .find(|(d, _)| a >= *d * 0.9995)
        .unwrap_or((1e-15, "f"));
    let x = v / d;
    let ip = (x.abs().log10().floor() as i32 + 1).clamp(1, 4);
    let t = format!("{:.*}", (4 - ip) as usize, x);
    let t = if t.contains('.') { t.trim_end_matches('0').trim_end_matches('.').to_string() } else { t };
    format!("{t}{suffix}")
}

fn with_unit(v: String, unit: &str) -> String {
    if unit.is_empty() {
        v
    } else {
        format!("{v}{unit}")
    }
}

struct Trace<'a> {
    label: String,
    color: String,
    x: &'a [f64],
    v: &'a Vector,
    step: Value,
    style: TraceStyle,
}

#[derive(Clone, Copy)]
struct TraceStyle {
    index: usize,
    previous: bool,
}

impl TraceStyle {
    fn dash(self) -> &'static str {
        const DASHES: [&str; 6] = ["none", "11 4", "2 4", "10 4 2 4", "16 5", "4 3 11 3"];
        if self.previous { "5 5" } else { DASHES[self.index % DASHES.len()] }
    }

    fn width(self) -> f64 {
        if self.previous { 3.2 } else { 1.9 }
    }

    fn opacity(self) -> f64 {
        if self.previous { 0.6 } else { 1.0 }
    }
}

fn available(p: &Plot) -> String {
    p.vectors.iter().map(|v| v.name.clone()).collect::<Vec<_>>().join(", ")
}

fn default_names(p: &Plot, kind: &DiagramKind) -> Vec<String> {
    let skip = if p.kind == "op" { 0 } else { 1 };
    let cand = || p.vectors.iter().skip(skip);
    match kind {
        DiagramKind::Polar | DiagramKind::Smith => cand().filter(|v| v.is_complex()).take(4).map(|v| v.name.clone()).collect(),
        DiagramKind::Table if p.kind == "op" => cand().take(60).map(|v| v.name.clone()).collect(),
        _ => {
            let mut n: Vec<String> = cand().filter(|v| v.name.to_ascii_lowercase().starts_with("v(")).take(4).map(|v| v.name.clone()).collect();
            if n.is_empty() {
                n = cand().filter(|v| !v.name.contains('#')).take(4).map(|v| v.name.clone()).collect();
            }
            n
        }
    }
}

fn collect_traces<'a>(plots: &[&'a Plot], spec: &DiagramSpec) -> Result<Vec<Trace<'a>>, String> {
    let p0 = plots[0];
    let names = if spec.vectors.is_empty() { default_names(p0, &spec.kind) } else { spec.vectors.clone() };
    if names.is_empty() {
        return Err(match spec.kind {
            DiagramKind::Polar | DiagramKind::Smith => format!("no complex vectors in '{}' for a {:?} diagram. Available: {}", p0.name, spec.kind, available(p0)),
            _ => format!("no vectors to show in '{}'. Available: {}", p0.name, available(p0)),
        });
    }
    let mut out = Vec::new();
    let comparing = spec.previous_start.is_some();
    for (pi, p) in plots.iter().enumerate() {
        let previous_start = spec.previous_start.filter(|&start| pi >= start);
        let previous = previous_start.is_some();
        let matching = previous_start.and_then(|start| plots[..start].iter().position(|current| current.sweep == p.sweep));
        let x: &[f64] = match p.scale() {
            Some(s) => &s.re,
            None => &[],
        };
        let sl = sweep_label(p);
        for (signal, n) in names.iter().enumerate() {
            let v = p.vector(n).ok_or_else(|| format!("no vector '{n}' in analysis '{}'. Available vectors: {}", p.name, available(p)))?;
            if matches!(spec.kind, DiagramKind::Polar | DiagramKind::Smith) && !v.is_complex() {
                let cx: Vec<String> = p.vectors.iter().filter(|v| v.is_complex()).map(|v| v.name.clone()).collect();
                return Err(format!(
                    "vector '{}' is real; {:?} diagrams need complex data (AC / S-parameter results). Complex vectors: {}",
                    v.name,
                    spec.kind,
                    if cx.is_empty() { "none".to_string() } else { cx.join(", ") }
                ));
            }
            let label = if sl.is_empty() { v.name.clone() } else { format!("{} {sl}", v.name) };
            let k = matching.map_or(out.len(), |step| step * names.len() + signal);
            out.push(Trace {
                label: if previous { format!("{label} (previous)") } else if comparing { format!("{label} (current)") } else { label },
                color: NET_PALETTE[k % NET_PALETTE.len()].hex(),
                x,
                v,
                step: p.sweep.clone().unwrap_or(Value::Null),
                style: TraceStyle { index: k, previous },
            });
        }
    }
    Ok(out)
}

fn default_title(p: &Plot) -> String {
    let b = if p.base.is_empty() { &p.name } else { &p.base };
    if p.title.is_empty() || p.title == *b {
        b.clone()
    } else {
        format!("{b} – {}", p.title)
    }
}

fn svg_open(s: &mut String, w: f64, h: f64, title: &str) {
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="{FONT}"><rect width="{w}" height="{h}" fill="#fff"/>"##
    );
    if !title.is_empty() {
        let _ = write!(s, r##"<text x="{:.1}" y="22" font-size="15" font-weight="bold" fill="#222" text-anchor="middle">{}</text>"##, w / 2.0, esc(title));
    }
}

struct LegendItem {
    x: f64,
    y: f64,
    label: String,
    color: String,
    style: TraceStyle,
}

/// Flow layout of legend entries; returns (rows height, items with positions).
fn legend_layout(traces: &[Trace], x0: f64, width: f64) -> (f64, Vec<LegendItem>) {
    let labels: Vec<_> = if traces.len() > 16 {
        // Summarize long sweeps while retaining the distinction between runs.
        let compare = traces.iter().any(|t| t.style.previous);
        [false, true].iter().filter_map(|&previous| {
            let mut group = traces.iter().filter(|t| t.style.previous == previous);
            let first = group.next()?;
            let count = group.count() + 1;
            let name = first.v.name.as_str();
            let run = if previous { " (previous)" } else if compare { " (current)" } else { "" };
            Some((format!("{name} … {count} traces{run}"), first))
        }).collect()
    } else {
        traces.iter().map(|trace| (trace.label.clone(), trace)).collect()
    };
    let mut items = Vec::new();
    let (mut x, mut row) = (x0, 0usize);
    for (label, trace) in labels {
        let iw = LEGEND_LABEL_X + tw(&label, LEGEND_FONT) + 16.0;
        if x > x0 && x + iw > x0 + width {
            x = x0;
            row += 1;
        }
        items.push(LegendItem { x, y: row as f64 * LEGEND_ROW, label, color: trace.color.clone(), style: trace.style });
        x += iw;
    }
    let h = if items.is_empty() { 0.0 } else { (row + 1) as f64 * LEGEND_ROW };
    (h, items)
}

fn draw_legend(s: &mut String, items: &[LegendItem], top: f64) {
    for item in items {
        let (x, y) = (item.x, top + item.y + 8.0);
        let (color, style) = (&item.color, item.style);
        let _ = write!(
            s,
            r##"<line x1="{x:.1}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="{color}" stroke-width="{}" stroke-dasharray="{}" stroke-opacity="{}"/><text x="{:.1}" y="{:.1}" font-size="{LEGEND_FONT}" fill="#222">{}</text>"##,
            x + 30.0,
            style.width(),
            style.dash(),
            style.opacity(),
            x + LEGEND_LABEL_X,
            y + 4.0,
            esc(&item.label)
        );
    }
}

fn draw_trace(s: &mut String, tr: &Trace, points: &str) {
    let style = tr.style;
    let _ = write!(s, r##"<polyline points="{points}" fill="none" stroke="{}" stroke-width="{}" stroke-dasharray="{}" stroke-opacity="{}" stroke-linejoin="round"/>"##,
        tr.color, style.width(), style.dash(), style.opacity());
}

/// A value box (one or more lines) drawn with a coloured border.
struct BoxLbl {
    x: f64, // anchor (dot) position
    y: f64,
    lines: Vec<String>,
    color: String,
    by: f64, // resolved box top
    bx: f64,
}

impl BoxLbl {
    fn w(&self) -> f64 {
        self.lines.iter().map(|l| tw(l, 10.5)).fold(0.0, f64::max) + 10.0
    }
    fn h(&self) -> f64 {
        self.lines.len() as f64 * 13.0 + 4.0
    }
}

/// Place boxes next to their anchors (right, or left if no room), then push them apart
/// vertically so they never overlap, keeping them inside [ymin, ymax].
fn place_boxes(boxes: &mut [BoxLbl], xmin: f64, xmax: f64, ymin: f64, ymax: f64) {
    for b in boxes.iter_mut() {
        let w = b.w();
        b.bx = if b.x + 8.0 + w <= xmax { b.x + 8.0 } else { (b.x - 8.0 - w).max(xmin) };
        b.by = b.y - b.h() / 2.0;
    }
    // overlap resolution among boxes that share horizontal space
    let mut idx: Vec<usize> = (0..boxes.len()).collect();
    idx.sort_by(|&a, &b| boxes[a].by.partial_cmp(&boxes[b].by).unwrap_or(std::cmp::Ordering::Equal));
    for _ in 0..3 {
        for k in 0..idx.len() {
            let i = idx[k];
            for &j in &idx[..k] {
                let (a, b) = (&boxes[j], &boxes[i]);
                let hx = a.bx < b.bx + b.w() && b.bx < a.bx + a.w();
                if hx && b.by < a.by + a.h() + 2.0 && a.by < b.by + b.h() {
                    let ny = a.by + a.h() + 2.0;
                    boxes[i].by = ny;
                }
            }
        }
        // keep inside, shifting overflow upwards
        let mut limit = ymax;
        for &i in idx.iter().rev() {
            let h = boxes[i].h();
            if boxes[i].by + h > limit {
                boxes[i].by = limit - h;
            }
            if boxes[i].by < ymin {
                boxes[i].by = ymin;
            }
            limit = limit.min(boxes[i].by - 2.0).max(ymin + h);
        }
    }
}

fn draw_boxes(s: &mut String, boxes: &[BoxLbl]) {
    for b in boxes {
        let _ = write!(
            s,
            r##"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="2" fill="#fff" fill-opacity="0.92" stroke="{}" stroke-width="1"/>"##,
            b.bx,
            b.by,
            b.w(),
            b.h(),
            b.color
        );
        for (k, l) in b.lines.iter().enumerate() {
            let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="10.5" fill="#222">{}</text>"##, b.bx + 5.0, b.by + 12.5 + k as f64 * 13.0, esc(l));
        }
    }
}

fn dot(s: &mut String, x: f64, y: f64, col: &str) {
    let _ = write!(s, r##"<circle cx="{x:.1}" cy="{y:.1}" r="3.5" fill="{col}" stroke="#fff" stroke-width="1.2"/>"##);
}

// ---------------------------------------------------------------- axes

#[derive(Clone, Copy)]
struct Axis {
    lo: f64,
    hi: f64,
    log: bool,
    p0: f64,
    len: f64,
    flip: bool,
}

impl Axis {
    fn t(&self, v: f64) -> f64 {
        if self.log {
            (v.max(1e-300).log10() - self.lo.log10()) / (self.hi.log10() - self.lo.log10())
        } else {
            (v - self.lo) / (self.hi - self.lo)
        }
    }
    fn px(&self, v: f64) -> f64 {
        let t = self.t(v);
        if self.flip {
            self.p0 + self.len - t * self.len
        } else {
            self.p0 + t * self.len
        }
    }
    fn contains(&self, v: f64) -> bool {
        let e = (self.hi - self.lo).abs() * 1e-9;
        v >= self.lo - e && v <= self.hi + e
    }
    /// (value, major, labelled)
    fn ticks(&self, n: usize) -> Vec<(f64, bool, bool)> {
        if !self.log {
            return lin_ticks(self.lo, self.hi, n).into_iter().map(|t| (t, true, true)).collect();
        }
        let decades = (self.hi / self.lo).log10();
        let mut out = Vec::new();
        let mut d = 10f64.powf(self.lo.log10().floor());
        // every n-th decade labelled when the range is wide
        let every = ((decades / n.max(1) as f64).ceil() as i64).max(1);
        let mut di = self.lo.log10().floor() as i64;
        while d <= self.hi * 1.0001 {
            for m in 1..10 {
                let x = d * m as f64;
                if !self.contains(x) {
                    continue;
                }
                let major = m == 1;
                let label = (major && di.rem_euclid(every) == 0) || (decades <= 2.5 && (m == 2 || m == 5));
                if decades > 12.0 && !major {
                    continue;
                }
                out.push((x, major, label));
            }
            d *= 10.0;
            di += 1;
        }
        if !out.iter().any(|t| t.2) {
            out.push((self.lo, true, true));
            out.push((self.hi, true, true));
        }
        out
    }
}

/// Automatic range for values; linear ranges are rounded to nice tick steps.
fn auto_range(vals: &[f64], log: bool, n: usize) -> (f64, f64) {
    let mut lo = f64::MAX;
    let mut hi = f64::MIN;
    for &v in vals {
        if v.is_finite() && (!log || v > 0.0) {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if lo > hi {
        return if log { (1.0, 10.0) } else { (0.0, 1.0) };
    }
    if log {
        let (a, b) = (10f64.powf(lo.log10().floor()), 10f64.powf((hi.log10() - 1e-9).ceil()));
        return if b > a { (a, b) } else { (a, a * 10.0) };
    }
    if (hi - lo).abs() <= 1e-12 * hi.abs().max(lo.abs()).max(1e-300) {
        let d = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
        lo -= d;
        hi += d;
    }
    let step = nice_step((hi - lo) / n.max(1) as f64);
    ((lo / step + 1e-9).floor() * step, (hi / step - 1e-9).ceil() * step)
}

// ---------------------------------------------------------------- Rect

struct PanelData {
    ylabel: String,
    ylog: bool,
    /// per trace y values (NaN = gap)
    ys: Vec<Vec<f64>>,
    range: Option<(f64, f64)>,
    /// is this the phase panel
    phase: bool,
}

fn y_label_for(traces: &[Trace], use_db: bool) -> String {
    let mut units: Vec<&str> = traces.iter().map(|t| t.v.unit.as_str()).filter(|u| !u.is_empty()).collect();
    units.dedup();
    let names: Vec<&str> = {
        let mut n: Vec<&str> = traces.iter().map(|t| t.v.name.as_str()).collect();
        n.dedup();
        n
    };
    let cx = traces.iter().any(|t| t.v.is_complex());
    let what = if names.len() == 1 {
        names[0].to_string()
    } else if cx {
        "magnitude".into()
    } else {
        match units.as_slice() {
            ["V"] => "voltage",
            ["A"] => "current",
            _ => "value",
        }.into()
    };
    if use_db {
        format!("{} [dB]", if names.len() == 1 { format!("dB({})", names[0]) } else { "magnitude".into() })
    } else if units.len() == 1 {
        format!("{}{} [{}]", if cx && names.len() == 1 { "|" } else { "" }, if cx && names.len() == 1 { format!("{what}|") } else { what }, units[0])
    } else if cx && names.len() == 1 {
        format!("|{what}|")
    } else {
        what
    }
}

fn rect(plots: &[&Plot], spec: &DiagramSpec, traces: &[Trace], w: f64, h: f64) -> Result<DiagramOut, String> {
    let p0 = plots[0];
    let scale = p0.scale().ok_or("operating point results have no x axis; use a Table diagram")?;
    let mut xlog = spec.x_log.unwrap_or(p0.kind == "ac" || p0.kind == "noise");
    let any_complex = traces.iter().any(|t| t.v.is_complex());
    let use_db = spec.db.unwrap_or(any_complex);
    let show_phase = spec.phase.unwrap_or(any_complex);
    let ylog = spec.y_log && !use_db;

    // x range
    let (mut xmin, mut xmax) = (f64::MAX, f64::MIN);
    for t in traces {
        for &x in t.x.iter() {
            if x.is_finite() && (!xlog || x > 0.0) {
                xmin = xmin.min(x);
                xmax = xmax.max(x);
            }
        }
    }
    if xmin > xmax {
        if xlog {
            xlog = false;
            for t in traces {
                for &x in t.x.iter() {
                    xmin = xmin.min(x);
                    xmax = xmax.max(x);
                }
            }
        }
        if xmin > xmax {
            return Err("analysis has no data points".into());
        }
    }
    if let Some((a, b)) = spec.x_range {
        let (a, b) = (a.min(b), a.max(b));
        if !xlog || a > 0.0 {
            xmin = a;
            xmax = b;
        }
    }
    if xmax <= xmin {
        let d = if xmin == 0.0 { 1.0 } else { xmin.abs() * 0.1 };
        if xlog {
            xmin /= 2.0;
            xmax *= 2.0;
        } else {
            xmin -= d;
            xmax += d;
        }
    }

    // panel data
    let main_ys: Vec<Vec<f64>> = traces
        .iter()
        .map(|t| {
            let n = t.v.re.len().min(t.x.len());
            (0..n)
                .map(|i| {
                    if use_db {
                        let m = t.v.mag(i);
                        if m > 0.0 {
                            20.0 * m.log10()
                        } else {
                            f64::NAN
                        }
                    } else if t.v.is_complex() {
                        t.v.mag(i)
                    } else {
                        t.v.re[i]
                    }
                })
                .collect()
        })
        .collect();
    let mut panels = vec![PanelData { ylabel: y_label_for(traces, use_db), ylog, ys: main_ys, range: spec.y_range, phase: false }];
    let phase_traces: Vec<usize> = (0..traces.len()).filter(|&k| traces[k].v.is_complex() || !any_complex).collect();
    if show_phase && !phase_traces.is_empty() {
        let ys = (0..traces.len())
            .map(|k| if phase_traces.contains(&k) { unwrap_phase(traces[k].v) } else { vec![] })
            .collect();
        panels.push(PanelData { ylabel: "phase [deg]".into(), ylog: false, ys, range: None, phase: true });
    }

    // layout
    let title = spec.title.clone().unwrap_or_else(|| default_title(p0));
    let (ml, mr) = (74.0, 22.0);
    let pw = (w - ml - mr).max(50.0);
    let (leg_h, leg_items) = legend_layout(traces, ml, pw);
    let top0 = if title.is_empty() { 14.0 } else { 36.0 } + if spec.markers.is_empty() { 0.0 } else { 16.0 };
    let bottom = 20.0 + 22.0 + if leg_h > 0.0 { leg_h + 8.0 } else { 0.0 } + 4.0;
    let gap = 20.0;
    let avail = (h - top0 - bottom - gap * (panels.len() as f64 - 1.0)).max(60.0);
    let heights: Vec<f64> = if panels.len() == 2 { vec![avail * 0.6, avail * 0.4] } else { vec![avail] };
    let h = top0 + heights.iter().sum::<f64>() + gap * (panels.len() as f64 - 1.0) + bottom;

    let mut s = String::new();
    svg_open(&mut s, w, h, &title);
    let xn = ((pw / 85.0) as usize).clamp(2, 10);
    let mut marker_json = Vec::new();
    let mut tops = Vec::new();
    let mut top = top0;
    let mut axes: Vec<(Axis, Axis)> = Vec::new();
    for (pi, pd) in panels.iter().enumerate() {
        let ph = heights[pi];
        tops.push(top);
        let xa = Axis { lo: xmin, hi: xmax, log: xlog, p0: ml, len: pw, flip: false };
        let yn = ((ph / 42.0) as usize).clamp(2, 10);
        // y range from visible points
        let mut vis = Vec::new();
        for (k, ys) in pd.ys.iter().enumerate() {
            for (i, &y) in ys.iter().enumerate() {
                if let Some(&x) = traces[k].x.get(i) {
                    if xa.contains(x) {
                        vis.push(y);
                    }
                }
            }
        }
        let (ylo, yhi) = match pd.range {
            Some((a, b)) if b > a && (!pd.ylog || a > 0.0) => (a, b),
            _ => {
                if pd.phase {
                    // phase: snap to 45° multiples
                    let fin = vis.iter().copied().filter(|v| v.is_finite());
                    let (a, b) = (fin.clone().fold(f64::MAX, f64::min), fin.fold(f64::MIN, f64::max));
                    let (a, b) = if a > b { (-180.0, 180.0) } else { (a - 1e-6, b + 1e-6) };
                    let st = if b - a > 360.0 { 90.0 } else { 45.0 };
                    let (a2, b2) = ((a / st).floor() * st, (b / st).ceil() * st);
                    if b2 > a2 {
                        (a2, b2)
                    } else {
                        (a2 - st, b2 + st)
                    }
                } else {
                    auto_range(&vis, pd.ylog, yn)
                }
            }
        };
        let ya = Axis { lo: ylo, hi: yhi, log: pd.ylog, p0: top, len: ph, flip: true };
        let _ = write!(s, r##"<rect x="{ml}" y="{top:.1}" width="{pw:.1}" height="{ph:.1}" fill="#fcfcfd"/>"##);
        // grid
        let yt = if pd.phase {
            let st = nice_step((yhi - ylo) / yn as f64).max(15.0);
            let st = [15.0, 30.0, 45.0, 90.0, 180.0, 360.0, 720.0].iter().copied().find(|v| *v >= st).unwrap_or(st);
            let mut v = Vec::new();
            let mut t = (ylo / st - 1e-9).ceil() * st;
            while t <= yhi + 1e-9 && v.len() < 100 {
                v.push((t, true, true));
                t += st;
            }
            v
        } else {
            ya.ticks(yn)
        };
        for (t, major, lab) in yt {
            let y = ya.px(t);
            let _ = write!(s, r##"<line x1="{ml}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="{}"/>"##, ml + pw, if major { C_GRID } else { C_GRID_MINOR });
            if lab {
                let txt = if pd.phase { format!("{t:.0}") } else { eng(t) };
                let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="13" fill="{C_TICK}" text-anchor="end">{}</text>"##, ml - 6.0, y + 4.0, esc(&txt));
            }
        }
        let last = pi == panels.len() - 1;
        for (t, major, lab) in xa.ticks(xn) {
            let x = xa.px(t);
            let _ = write!(s, r##"<line x1="{x:.1}" y1="{top:.1}" x2="{x:.1}" y2="{:.1}" stroke="{}"/>"##, top + ph, if major { C_GRID } else { C_GRID_MINOR });
            if lab && last {
                let _ = write!(s, r##"<text x="{x:.1}" y="{:.1}" font-size="13" fill="{C_TICK}" text-anchor="middle">{}</text>"##, top + ph + 17.0, eng(t));
            }
        }
        let _ = write!(s, r##"<rect x="{ml}" y="{top:.1}" width="{pw:.1}" height="{ph:.1}" fill="none" stroke="{C_FRAME}"/>"##);
        let ylabel = esc(&pd.ylabel);
        if ph >= tw(&pd.ylabel, 14.0) + 8.0 {
            let cy = top + ph / 2.0;
            let _ = write!(s, r##"<text x="16" y="{cy:.1}" font-size="14" fill="{C_TEXT}" text-anchor="middle" transform="rotate(-90 16 {cy:.1})">{ylabel}</text>"##);
        } else {
            let _ = write!(s, r##"<text x="{ml:.1}" y="{:.1}" font-size="13" fill="{C_TEXT}">{ylabel}</text>"##, top - 4.0);
        }
        // traces
        let _ = write!(s, r##"<clipPath id="clip{pi}"><rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}"/></clipPath><g clip-path="url(#clip{pi})">"##, ml - 2.0, top - 2.0, pw + 4.0, ph + 4.0);
        // Draw the previous run underneath, so even exactly coincident traces can be seen.
        let split = traces.iter().position(|tr| tr.style.previous).unwrap_or(traces.len());
        for k in (split..traces.len()).chain(0..split) {
            let ys = &pd.ys[k];
            if ys.is_empty() {
                continue;
            }
            let tr = &traces[k];
            let n = ys.len().min(tr.x.len());
            let step = (n / 4000).max(1);
            let mut segs: Vec<String> = vec![String::new()];
            for i in (0..n).step_by(step) {
                let (x, y) = (tr.x[i], ys[i]);
                if !y.is_finite() || (xlog && x <= 0.0) || (pd.ylog && y <= 0.0) {
                    if !segs.last().unwrap().is_empty() {
                        segs.push(String::new());
                    }
                    continue;
                }
                let (px, py) = (xa.px(x), ya.px(y).clamp(-1e5, 1e5));
                let _ = write!(segs.last_mut().unwrap(), "{px:.1},{py:.1} ");
            }
            for pts in segs.iter().filter(|p| !p.is_empty()) {
                draw_trace(&mut s, tr, pts);
            }
        }
        s.push_str("</g>");
        axes.push((xa, ya));
        top += ph + gap;
    }
    let plot_bottom = top - gap;
    let (xa0, _) = axes[0];
    let _ = write!(
        s,
        r##"<text x="{:.1}" y="{:.1}" font-size="14" fill="{C_TEXT}" text-anchor="middle">{}</text>"##,
        ml + pw / 2.0,
        plot_bottom + 34.0,
        esc(&if scale.unit.is_empty() { scale.name.clone() } else { format!("{} [{}]", scale.name, scale.unit) })
    );
    draw_legend(&mut s, &leg_items, plot_bottom + 44.0);

    // markers
    let mut xlabels: Vec<(f64, f64, String)> = Vec::new();
    for &mx in &spec.markers {
        let inside = xa0.contains(mx) && (!xlog || mx > 0.0);
        for t in traces {
            let v = sample(t.x, t.v, mx, xlog);
            let value = match v {
                Some((re, im)) if t.v.is_complex() => complex_json(re, im),
                Some((re, _)) => json!(re),
                None => Value::Null,
            };
            let mut mark = json!({"x": mx, "vector": t.v.name, "label": t.label, "value": value, "step": t.step});
            if spec.previous_start.is_some() {
                mark["run"] = json!(if t.style.previous { "previous" } else { "current" });
            }
            marker_json.push(mark);
        }
        if !inside {
            continue;
        }
        let px = xa0.px(mx);
        let _ = write!(
            s,
            r##"<line x1="{px:.1}" y1="{:.1}" x2="{px:.1}" y2="{plot_bottom:.1}" stroke="{C_MARKER}" stroke-width="1" stroke-dasharray="4 3"/>"##,
            top0
        );
        let xl = with_unit(eng4(mx), &scale.unit);
        let lw = tw(&xl, 10.5) + 10.0;
        xlabels.push((px - lw / 2.0, lw, xl));
        for (pi, pd) in panels.iter().enumerate() {
            let (_, ya) = axes[pi];
            let mut boxes = Vec::new();
            for (k, ys) in pd.ys.iter().enumerate() {
                if ys.is_empty() {
                    continue;
                }
                let tr = &traces[k];
                let n = ys.len().min(tr.x.len());
                let Some((i, f)) = locate(&tr.x[..n], mx, xlog) else { continue };
                let y = if pd.phase || !tr.v.is_complex() || f == 0.0 {
                    lerp(ys, i, f)
                } else {
                    // interpolate complex value, then derive magnitude / dB
                    let (re, im) = sample(tr.x, tr.v, mx, xlog).unwrap_or((0.0, 0.0));
                    let m = (re * re + im * im).sqrt();
                    if use_db {
                        20.0 * m.max(1e-300).log10()
                    } else {
                        m
                    }
                };
                if !y.is_finite() || (pd.ylog && y <= 0.0) {
                    continue;
                }
                let py = ya.px(y);
                if py < ya.p0 - 0.5 || py > ya.p0 + ya.len + 0.5 {
                    continue;
                }
                dot(&mut s, px, py, &tr.color);
                let txt = if pd.phase {
                    format!("{y:.2}°")
                } else if use_db {
                    format!("{y:.2} dB")
                } else {
                    with_unit(eng4(y), &tr.v.unit)
                };
                boxes.push(BoxLbl { x: px, y: py, lines: vec![txt], color: tr.color.clone(), bx: 0.0, by: 0.0 });
            }
            place_boxes(&mut boxes, ml + 2.0, ml + pw - 2.0, ya.p0 + 2.0, ya.p0 + ya.len - 2.0);
            draw_boxes(&mut s, &boxes);
        }
    }
    // marker x labels above the plot, pushed apart so they never overlap
    xlabels.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut right = ml;
    for l in xlabels.iter_mut() {
        l.0 = l.0.max(right);
        right = l.0 + l.1 + 3.0;
    }
    let mut left = ml + pw;
    for l in xlabels.iter_mut().rev() {
        l.0 = l.0.min(left - l.1);
        left = l.0 - 3.0;
    }
    for (lx, lw, xl) in &xlabels {
        let _ = write!(
            s,
            r##"<rect x="{lx:.1}" y="{:.1}" width="{lw:.1}" height="14" rx="2" fill="{C_MARKER}"/><text x="{:.1}" y="{:.1}" font-size="10.5" fill="#fff" text-anchor="middle">{}</text>"##,
            top0 - 16.0,
            lx + lw / 2.0,
            top0 - 5.5,
            esc(xl)
        );
    }
    s.push_str("</svg>");
    Ok(DiagramOut { svg: s, width: w, height: h, markers: Value::Array(marker_json), plot_area: Some((ml, tops[0], pw, heights[0], xmin, xmax, xlog)) })
}

// ---------------------------------------------------------------- Polar / Smith

struct Round {
    cx: f64,
    cy: f64,
    r: f64,
    legend_top: f64,
    h: f64,
    items: Vec<LegendItem>,
}

fn round_layout(traces: &[Trace], title: &str, w: f64, h: f64, label_pad: f64) -> Round {
    let (leg_h, mut items) = legend_layout(traces, 20.0, w - 40.0);
    let top = if title.is_empty() { 10.0 } else { 34.0 };
    let bottom = leg_h + 14.0;
    let avail_h = (h - top - bottom - 2.0 * label_pad).max(80.0);
    let r = ((w - 2.0 * label_pad - 20.0).min(avail_h) / 2.0).max(40.0);
    let cy = top + label_pad + r;
    let h = cy + r + label_pad + bottom;
    // centre legend
    for row in items.chunk_by_mut(|a, b| a.y == b.y) {
        let end = row.last().unwrap();
        let row_right = end.x + LEGEND_LABEL_X + tw(&end.label, LEGEND_FONT);
        let shift = ((w - 20.0 - row_right) / 2.0).max(0.0);
        for item in row {
            item.x += shift;
        }
    }
    Round { cx: w / 2.0, cy, r, legend_top: cy + r + label_pad + 4.0, h, items }
}

fn marker_scale(p: &Plot, spec: &DiagramSpec) -> bool {
    spec.x_log.unwrap_or(p.kind == "ac" || p.kind == "noise")
}

/// Trace polyline in Gamma / complex plane coordinates.
fn complex_polyline(s: &mut String, tr: &Trace, sc: f64, cx: f64, cy: f64, x_range: Option<(f64, f64)>) {
    let n = tr.v.re.len();
    let im = tr.v.im.as_ref();
    let step = (n / 4000).max(1);
    let mut pts = String::new();
    for i in (0..n).step_by(step) {
        if let (Some((a, b)), Some(&x)) = (x_range, tr.x.get(i)) {
            if x < a.min(b) || x > a.max(b) {
                continue;
            }
        }
        let (re, imv) = (tr.v.re[i], im.map(|v| v[i]).unwrap_or(0.0));
        if !re.is_finite() || !imv.is_finite() {
            continue;
        }
        let _ = write!(pts, "{:.1},{:.1} ", cx + re * sc, cy - imv * sc);
    }
    draw_trace(s, tr, &pts);
    // start marker (small square) to show direction
    if n > 0 {
        let (re, imv) = (tr.v.re[0], im.map(|v| v[0]).unwrap_or(0.0));
        let _ = write!(s, r##"<rect x="{:.1}" y="{:.1}" width="5" height="5" fill="{}"/>"##, cx + re * sc - 2.5, cy - imv * sc - 2.5, tr.color);
    }
}

fn polar(plots: &[&Plot], spec: &DiagramSpec, traces: &[Trace], w: f64, h: f64) -> Result<DiagramOut, String> {
    let p0 = plots[0];
    let scale = p0.scale().ok_or("operating point results cannot be shown in a polar diagram")?;
    let title = spec.title.clone().unwrap_or_else(|| default_title(p0));
    let lay = round_layout(traces, &title, w, h, 26.0);
    let (cx, cy, r) = (lay.cx, lay.cy, lay.r);
    let mut maxm = 0.0f64;
    for t in traces {
        for i in 0..t.v.re.len() {
            let m = t.v.mag(i);
            if m.is_finite() {
                maxm = maxm.max(m);
            }
        }
    }
    let rmax = match spec.y_range {
        Some((_, b)) if b > 0.0 => b,
        _ => {
            if maxm <= 0.0 {
                1.0
            } else {
                let st = nice_step(maxm / 4.0);
                (maxm / st - 1e-9).ceil() * st
            }
        }
    };
    let sc = r / rmax;
    let mut s = String::new();
    svg_open(&mut s, w, lay.h, &title);
    let _ = write!(s, r##"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="#fcfcfd"/>"##);
    for a in (0..360).step_by(30) {
        let ar = (a as f64).to_radians();
        let _ = write!(s, r##"<line x1="{cx:.1}" y1="{cy:.1}" x2="{:.1}" y2="{:.1}" stroke="{C_GRID}"/>"##, cx + r * ar.cos(), cy - r * ar.sin());
        let (lx, ly) = (cx + (r + 8.0) * ar.cos(), cy - (r + 8.0) * ar.sin());
        let anchor = if ar.cos() > 0.3 { "start" } else if ar.cos() < -0.3 { "end" } else { "middle" };
        let dy = if ar.sin() > 0.3 { -1.0 } else if ar.sin() < -0.3 { 9.0 } else { 4.0 };
        let deg = if a > 180 { a - 360 } else { a };
        let _ = write!(s, r##"<text x="{lx:.1}" y="{:.1}" font-size="10.5" fill="{C_TICK}" text-anchor="{anchor}">{deg}°</text>"##, ly + dy);
    }
    let rings = lin_ticks(0.0, rmax, 5);
    for &t in rings.iter().filter(|t| **t > 0.0) {
        let rr = t * sc;
        let _ = write!(s, r##"<circle cx="{cx:.1}" cy="{cy:.1}" r="{rr:.1}" fill="none" stroke="{}"/>"##, if (t - rmax).abs() < 1e-9 * rmax { C_FRAME } else { C_GRID });
        let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="10" fill="{C_TICK}" text-anchor="end">{}</text>"##, cx + rr - 2.0, cy + 12.0, eng(t));
    }
    if (rings.last().copied().unwrap_or(0.0) - rmax).abs() > 1e-9 * rmax {
        let _ = write!(s, r##"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="none" stroke="{C_FRAME}"/>"##);
    }
    let _ = write!(s, r##"<clipPath id="clipr"><rect x="0" y="0" width="{w}" height="{:.1}"/></clipPath><g clip-path="url(#clipr)">"##, lay.legend_top - 4.0);
    for t in traces {
        complex_polyline(&mut s, t, sc, cx, cy, spec.x_range);
    }
    s.push_str("</g>");
    draw_legend(&mut s, &lay.items, lay.legend_top);
    let xlog = marker_scale(p0, spec);
    let mut mj = Vec::new();
    let mut boxes = Vec::new();
    for &f in &spec.markers {
        for t in traces {
            let v = sample(t.x, t.v, f, xlog);
            mj.push(json!({"x": f, "vector": t.v.name, "label": t.label, "value": v.map(|(a, b)| complex_json(a, b)).unwrap_or(Value::Null), "step": t.step}));
            if let Some((re, im)) = v {
                let (px, py) = (cx + re * sc, cy - im * sc);
                dot(&mut s, px, py, &t.color);
                boxes.push(BoxLbl { x: px, y: py, lines: vec![format!("{}: {}", with_unit(eng4(f), &scale.unit), fmt_polar(re, im))], color: t.color.clone(), bx: 0.0, by: 0.0 });
            }
        }
    }
    place_boxes(&mut boxes, 4.0, w - 4.0, 30.0, lay.legend_top - 6.0);
    draw_boxes(&mut s, &boxes);
    s.push_str("</svg>");
    Ok(DiagramOut { svg: s, width: w, height: lay.h, markers: Value::Array(mj), plot_area: None })
}

/// Complex division helper.
fn cdiv(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let d = b.0 * b.0 + b.1 * b.1;
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}

fn smith(plots: &[&Plot], spec: &DiagramSpec, traces: &[Trace], w: f64, h: f64) -> Result<DiagramOut, String> {
    let p0 = plots[0];
    let scale = p0.scale().ok_or("operating point results cannot be shown in a Smith chart")?;
    let title = spec.title.clone().unwrap_or_else(|| default_title(p0));
    let lay = round_layout(traces, &title, w, h, 24.0);
    let (cx, cy, r) = (lay.cx, lay.cy, lay.r);
    let z0 = if spec.z0 > 0.0 { spec.z0 } else { 50.0 };
    let mut s = String::new();
    svg_open(&mut s, w, lay.h, &title);
    let _ = write!(s, r##"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="#fcfcfd"/>"##);
    let _ = write!(s, r##"<clipPath id="clipu"><circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}"/></clipPath><g clip-path="url(#clipu)" fill="none" stroke="{C_GRID}">"##);
    // constant-resistance circles: centre r/(1+r), radius 1/(1+r)
    for rv in [0.2, 0.5, 1.0, 2.0, 5.0] {
        let (c, rr) = (rv / (1.0 + rv), 1.0 / (1.0 + rv));
        let _ = write!(s, r##"<circle cx="{:.1}" cy="{cy:.1}" r="{:.1}"/>"##, cx + c * r, rr * r);
    }
    // constant-reactance arcs: centre (1, 1/x), radius 1/|x|
    for xv in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        for sg in [1.0, -1.0] {
            let _ = write!(s, r##"<circle cx="{:.1}" cy="{:.1}" r="{:.1}"/>"##, cx + r, cy - sg * r / xv, r / xv);
        }
    }
    let _ = write!(s, r##"<line x1="{:.1}" y1="{cy:.1}" x2="{:.1}" y2="{cy:.1}"/>"##, cx - r, cx + r);
    s.push_str("</g>");
    let _ = write!(s, r##"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="none" stroke="{C_FRAME}" stroke-width="1.2"/>"##);
    for rv in [0.0, 0.2, 0.5, 1.0, 2.0, 5.0] {
        let g = (rv - 1.0) / (rv + 1.0);
        let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="10" fill="{C_TICK}">{}</text>"##, cx + g * r + 2.0, cy - 3.0, rv);
    }
    for xv in [0.2f64, 0.5, 1.0, 2.0, 5.0] {
        for sg in [1.0, -1.0] {
            let g = cdiv((-1.0, sg * xv), (1.0, sg * xv));
            let a = g.1.atan2(g.0);
            let (lx, ly) = (cx + (r + 7.0) * a.cos(), cy - (r + 7.0) * a.sin());
            let anchor = if a.cos() > 0.3 { "start" } else if a.cos() < -0.3 { "end" } else { "middle" };
            let dy = if a.sin() > 0.0 { 0.0 } else { 8.0 };
            let _ = write!(s, r##"<text x="{lx:.1}" y="{:.1}" font-size="10" fill="{C_TICK}" text-anchor="{anchor}">{}j{}</text>"##, ly + dy, if sg < 0.0 { "−" } else { "+" }, xv);
        }
    }
    let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="10" fill="{C_TICK}" text-anchor="start">∞</text>"##, cx + r + 5.0, cy + 4.0);
    let _ = write!(s, r##"<clipPath id="clips"><rect x="0" y="0" width="{w}" height="{:.1}"/></clipPath><g clip-path="url(#clips)">"##, lay.legend_top - 4.0);
    for t in traces {
        complex_polyline(&mut s, t, r, cx, cy, spec.x_range);
    }
    s.push_str("</g>");
    draw_legend(&mut s, &lay.items, lay.legend_top);
    let xlog = marker_scale(p0, spec);
    let mut mj = Vec::new();
    let mut boxes = Vec::new();
    for &f in &spec.markers {
        for t in traces {
            let v = sample(t.x, t.v, f, xlog);
            let mut m = json!({"x": f, "vector": t.v.name, "label": t.label, "value": Value::Null, "step": t.step});
            if let Some((re, im)) = v {
                m["value"] = complex_json(re, im);
                let z = cdiv((1.0 + re, im), (1.0 - re, -im));
                let z = (z.0 * z0, z.1 * z0);
                m["z"] = json!({"re": z.0, "im": z.1});
                let (px, py) = (cx + re * r, cy - im * r);
                dot(&mut s, px, py, &t.color);
                boxes.push(BoxLbl {
                    x: px,
                    y: py,
                    lines: vec![format!("{}  Γ={}", with_unit(eng4(f), &scale.unit), fmt_polar(re, im)), format!("Z={}Ω", fmt_c(z.0, z.1))],
                    color: t.color.clone(),
                    bx: 0.0,
                    by: 0.0,
                });
            }
            mj.push(m);
        }
    }
    place_boxes(&mut boxes, 4.0, w - 4.0, 30.0, lay.legend_top - 6.0);
    draw_boxes(&mut s, &boxes);
    s.push_str("</svg>");
    Ok(DiagramOut { svg: s, width: w, height: lay.h, markers: Value::Array(mj), plot_area: None })
}

// ---------------------------------------------------------------- Table

fn table_svg(title: &str, header: &[String], rows: &[Vec<String>], min_w: f64) -> (String, f64, f64) {
    let ncol = header.len();
    let mut cw = vec![0.0f64; ncol];
    for (c, hd) in header.iter().enumerate() {
        cw[c] = tw(hd, 12.0) + 20.0;
        for r in rows {
            if let Some(t) = r.get(c) {
                cw[c] = cw[c].max(tw(t, 12.0) + 20.0);
            }
        }
        cw[c] = cw[c].max(60.0);
    }
    let tot: f64 = cw.iter().sum();
    let w = (tot + 40.0).max(min_w).max(tw(title, 15.0) + 40.0);
    // spread spare width over the columns (capped so cells don't become huge)
    let extra = ((w - 40.0 - tot) / ncol as f64).min(80.0);
    if extra > 0.0 {
        cw.iter_mut().for_each(|c| *c += extra);
    }
    let tot: f64 = cw.iter().sum();
    let x0 = ((w - tot) / 2.0).max(20.0);
    let rh = 20.0;
    let top = if title.is_empty() { 12.0 } else { 38.0 };
    let h = top + rh * (rows.len() as f64 + 1.0) + 14.0;
    let mut s = String::new();
    svg_open(&mut s, w, h, title);
    let _ = write!(s, r##"<rect x="{x0:.1}" y="{top:.1}" width="{tot:.1}" height="{rh}" fill="#e9eef4"/>"##);
    for (ri, _) in rows.iter().enumerate() {
        if ri % 2 == 1 {
            let _ = write!(s, r##"<rect x="{x0:.1}" y="{:.1}" width="{tot:.1}" height="{rh}" fill="#f6f8fa"/>"##, top + rh * (ri as f64 + 1.0));
        }
    }
    let mut x = x0;
    for c in 0..ncol {
        let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="12" font-weight="bold" fill="#222" text-anchor="end">{}</text>"##, x + cw[c] - 10.0, top + 14.5, esc(&header[c]));
        for (ri, r) in rows.iter().enumerate() {
            let t = r.get(c).map(|s| s.as_str()).unwrap_or("");
            let _ = write!(s, r##"<text x="{:.1}" y="{:.1}" font-size="12" fill="{}" text-anchor="end">{}</text>"##, x + cw[c] - 10.0, top + rh * (ri as f64 + 1.0) + 14.5, if c == 0 { "#444" } else { "#222" }, esc(t));
        }
        if c > 0 {
            let _ = write!(s, r##"<line x1="{x:.1}" y1="{top:.1}" x2="{x:.1}" y2="{:.1}" stroke="{C_GRID}"/>"##, top + rh * (rows.len() as f64 + 1.0));
        }
        x += cw[c];
    }
    let _ = write!(s, r##"<line x1="{x0:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{C_FRAME}"/>"##, top + rh, x0 + tot, top + rh);
    let _ = write!(s, r##"<rect x="{x0:.1}" y="{top:.1}" width="{tot:.1}" height="{:.1}" fill="none" stroke="{C_FRAME}"/>"##, rh * (rows.len() as f64 + 1.0));
    s.push_str("</svg>");
    (s, w, h)
}

fn fmt_cell(v: Option<(f64, f64)>, complex: bool, db: bool, unit: &str) -> String {
    match v {
        None => "–".into(),
        Some((re, im)) if complex => {
            let m = (re * re + im * im).sqrt();
            let ph = im.atan2(re).to_degrees();
            if db {
                format!("{:.3} dB / {ph:.2}°", 20.0 * m.max(1e-300).log10())
            } else {
                format!("{} / {ph:.2}°", with_unit(eng4(m), unit))
            }
        }
        Some((re, _)) => with_unit(eng4(re), unit),
    }
}

fn table(plots: &[&Plot], spec: &DiagramSpec, traces: &[Trace], w: f64) -> Result<DiagramOut, String> {
    let p0 = plots[0];
    let title = spec.title.clone().unwrap_or_else(|| default_title(p0));
    let db = spec.db.unwrap_or(false);
    if p0.kind == "op" {
        let header = vec!["name".to_string(), "value".to_string()];
        let rows: Vec<Vec<String>> = traces
            .iter()
            .map(|t| {
                let v = if t.v.re.is_empty() { None } else { Some((t.v.re[0], t.v.im.as_ref().map(|i| i[0]).unwrap_or(0.0))) };
                vec![t.label.clone(), fmt_cell(v, t.v.is_complex(), db, &t.v.unit)]
            })
            .collect();
        let (svg, w, h) = table_svg(&title, &header, &rows, w);
        let mj: Vec<Value> = traces.iter().map(|t| json!({"x": Value::Null, "vector": t.v.name, "label": t.label, "value": t.v.re.first(), "step": t.step})).collect();
        return Ok(DiagramOut { svg, width: w, height: h, markers: Value::Array(mj), plot_area: None });
    }
    let scale = p0.scale().ok_or("no scale")?;
    let xlog = marker_scale(p0, spec);
    let xs: Vec<f64> = if !spec.markers.is_empty() {
        spec.markers.clone()
    } else {
        let base: Vec<f64> = scale
            .re
            .iter()
            .copied()
            .filter(|x| spec.x_range.map(|(a, b)| *x >= a.min(b) && *x <= a.max(b)).unwrap_or(true))
            .collect();
        let n = base.len();
        if n <= 25 {
            base
        } else {
            (0..25).map(|k| base[(k * (n - 1) + 12) / 24]).collect()
        }
    };
    let mut header = vec![if scale.unit.is_empty() { scale.name.clone() } else { format!("{} [{}]", scale.name, scale.unit) }];
    for t in traces {
        header.push(t.label.clone());
    }
    let mut mj = Vec::new();
    let rows: Vec<Vec<String>> = xs
        .iter()
        .map(|&x| {
            let mut r = vec![eng4(x)];
            for t in traces {
                let v = sample(t.x, t.v, x, xlog);
                r.push(fmt_cell(v, t.v.is_complex(), db, &t.v.unit));
                if !spec.markers.is_empty() {
                    let value = match v {
                        Some((re, im)) if t.v.is_complex() => complex_json(re, im),
                        Some((re, _)) => json!(re),
                        None => Value::Null,
                    };
                    mj.push(json!({"x": x, "vector": t.v.name, "label": t.label, "value": value, "step": t.step}));
                }
            }
            r
        })
        .collect();
    let (svg, w, h) = table_svg(&title, &header, &rows, w);
    Ok(DiagramOut { svg, width: w, height: h, markers: Value::Array(mj), plot_area: None })
}

// ---------------------------------------------------------------- entry

/// Render a diagram of one analysis (or all steps of a parameter sweep, overlaid).
/// Table diagrams size their height to the content; `DiagramOut` reports the final size.
pub fn diagram_svg(plots: &[&Plot], spec: &DiagramSpec, width: f64, height: f64) -> Result<DiagramOut, String> {
    let p0 = *plots.first().ok_or("no simulation data")?;
    if spec.previous_start.is_some_and(|start| spec.kind != DiagramKind::Rect || start == 0 || start >= plots.len()) {
        return Err("a comparison needs current and previous plots in a rectangular diagram".into());
    }
    let (width, height) = (width.max(240.0), height.max(200.0));
    if p0.kind == "op" && spec.kind != DiagramKind::Table {
        return Err(format!("'{}' is an operating point (single values); only a Table diagram can show it", p0.name));
    }
    let traces = collect_traces(plots, spec)?;
    match spec.kind {
        DiagramKind::Rect => rect(plots, spec, &traces, width, height),
        DiagramKind::Polar => polar(plots, spec, &traces, width, height),
        DiagramKind::Smith => smith(plots, spec, &traces, width, height),
        DiagramKind::Table => table(plots, spec, &traces, width),
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn logspace(a: f64, b: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| 10f64.powf(a.log10() + (b.log10() - a.log10()) * i as f64 / (n - 1) as f64)).collect()
    }

    /// RC low-pass (R=1k, C=100n -> fc = 1591.55 Hz), AC.
    fn rc_ac(r: f64, sweep: Option<Value>) -> Plot {
        let c = 100e-9;
        let f = logspace(10.0, 1e6, 201);
        let (mut re, mut im) = (vec![], vec![]);
        for &fr in &f {
            let wrc = 2.0 * std::f64::consts::PI * fr * r * c;
            let d = 1.0 + wrc * wrc;
            re.push(1.0 / d);
            im.push(-wrc / d);
        }
        Plot {
            name: "ac1".into(),
            base: "ac1".into(),
            kind: "ac".into(),
            title: "AC Analysis".into(),
            vectors: vec![
                Vector { name: "frequency".into(), unit: "Hz".into(), re: f.clone(), im: None },
                Vector { name: "v(in)".into(), unit: "V".into(), re: vec![1.0; f.len()], im: Some(vec![0.0; f.len()]) },
                Vector { name: "v(out)".into(), unit: "V".into(), re, im: Some(im) },
            ],
            sweep,
        }
    }

    /// S11 of a 50 Ohm line terminated in 100 Ohm || 1 pF-ish load (synthetic Gamma sweep).
    fn gamma_sp() -> Plot {
        let f = (0..101).map(|i| 1e8 + i as f64 * 2e7).collect::<Vec<_>>();
        let (mut re, mut im) = (vec![], vec![]);
        for &fr in &f {
            let zl = (100.0, 2.0 * std::f64::consts::PI * fr * 10e-9 - 1.0 / (2.0 * std::f64::consts::PI * fr * 5e-12));
            let g = cdiv((zl.0 - 50.0, zl.1), (zl.0 + 50.0, zl.1));
            re.push(g.0);
            im.push(g.1);
        }
        Plot {
            name: "sp1".into(),
            base: "sp1".into(),
            kind: "ac".into(),
            title: "S-parameter".into(),
            vectors: vec![
                Vector { name: "frequency".into(), unit: "Hz".into(), re: f, im: None },
                Vector { name: "s_1_1".into(), unit: "".into(), re, im: Some(im) },
            ],
            sweep: None,
        }
    }

    fn tran() -> Plot {
        let t: Vec<f64> = (0..500).map(|i| i as f64 * 1e-5).collect();
        let v: Vec<f64> = t.iter().map(|t| 1.0 - (-t / 1e-4).exp()).collect();
        Plot {
            name: "tran1".into(),
            base: "tran1".into(),
            kind: "tran".into(),
            title: "Transient".into(),
            vectors: vec![
                Vector { name: "time".into(), unit: "s".into(), re: t, im: None },
                Vector { name: "v(out)".into(), unit: "V".into(), re: v, im: None },
            ],
            sweep: None,
        }
    }

    fn parses(svg: &str) {
        let opt = resvg::usvg::Options::default();
        resvg::usvg::Tree::from_str(svg, &opt).expect("svg parses");
    }

    #[test]
    fn parse_kind_names() {
        assert_eq!(DiagramSpec::parse_kind("Cartesian"), Some(DiagramKind::Rect));
        assert_eq!(DiagramSpec::parse_kind("smith"), Some(DiagramKind::Smith));
        assert_eq!(DiagramSpec::parse_kind("tabular"), Some(DiagramKind::Table));
        assert_eq!(DiagramSpec::parse_kind("polar"), Some(DiagramKind::Polar));
        assert_eq!(DiagramSpec::parse_kind("pie"), None);
        assert_eq!(DiagramSpec::default().z0, 50.0);
    }

    #[test]
    fn rect_marker_at_corner() {
        let p = rc_ac(1e3, None);
        let fc = 1.0 / (2.0 * std::f64::consts::PI * 1e3 * 100e-9);
        let spec = DiagramSpec { vectors: vec!["v(out)".into()], markers: vec![fc], ..Default::default() };
        let out = diagram_svg(&[&p], &spec, 900.0, 600.0).unwrap();
        parses(&out.svg);
        let m = &out.markers[0];
        let mag = m["value"]["mag"].as_f64().unwrap();
        assert!((mag - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.003, "mag {mag}");
        assert!((m["value"]["db"].as_f64().unwrap() + 3.01).abs() < 0.05);
        assert!((m["value"]["phase_deg"].as_f64().unwrap() + 45.0).abs() < 0.5);
        let pa = out.plot_area.unwrap();
        assert!(pa.6 && (pa.4 - 10.0).abs() < 1e-9 && (pa.5 - 1e6).abs() < 1e-3);
        assert!(out.svg.contains("phase [deg]"));
    }

    #[test]
    fn comparison_overlays_the_same_signal_on_one_scale() {
        let current = rc_ac(2e3, None);
        let previous = rc_ac(1e3, None);
        let plots = [&current, &previous];
        let spec = DiagramSpec {
            vectors: vec!["v(out)".into()],
            markers: vec![1000.0],
            previous_start: Some(1),
            ..Default::default()
        };
        let traces = collect_traces(&plots, &spec).unwrap();
        assert_eq!(traces[0].color, traces[1].color);
        assert_ne!(traces[0].style.dash(), traces[1].style.dash());
        let output = diagram_svg(&plots, &spec, 560.0, 440.0).unwrap();
        parses(&output.svg);
        assert!(output.svg.contains("v(out) (previous)"));
        assert_eq!(output.markers[0]["run"], "current");
        assert_eq!(output.markers[1]["run"], "previous");
        assert_ne!(output.markers[0]["value"], output.markers[1]["value"]);
    }

    #[test]
    fn rect_real_linear_marker() {
        let p = tran();
        let spec = DiagramSpec { markers: vec![1e-4], ..Default::default() };
        let out = diagram_svg(&[&p], &spec, 800.0, 400.0).unwrap();
        parses(&out.svg);
        let v = out.markers[0]["value"].as_f64().unwrap();
        assert!((v - (1.0 - (-1f64).exp())).abs() < 1e-6, "{v}");
        assert!(!out.plot_area.unwrap().6);
        assert!(!out.svg.contains("phase"));
    }

    #[test]
    fn sweep_overlay_labels() {
        let a = rc_ac(1e3, Some(json!({"R": 1000.0})));
        let b = rc_ac(2e3, Some(json!({"R": 2000.0})));
        let spec = DiagramSpec { vectors: vec!["out".into()], markers: vec![1000.0], ..Default::default() };
        let out = diagram_svg(&[&a, &b], &spec, 900.0, 600.0).unwrap();
        parses(&out.svg);
        assert!(out.svg.contains("R=1k") && out.svg.contains("R=2k"));
        assert_eq!(out.markers.as_array().unwrap().len(), 2);
        assert_eq!(out.markers[1]["step"]["R"], 2000.0);
    }

    #[test]
    fn polar_and_smith() {
        let p = gamma_sp();
        let f0 = 1e9;
        let spec = DiagramSpec { kind: DiagramKind::Smith, markers: vec![f0], ..Default::default() };
        let out = diagram_svg(&[&p], &spec, 600.0, 600.0).unwrap();
        parses(&out.svg);
        let m = &out.markers[0];
        // expected impedance at 1 GHz
        let w = 2.0 * std::f64::consts::PI * f0;
        let x = w * 10e-9 - 1.0 / (w * 5e-12);
        let zr = m["z"]["re"].as_f64().unwrap();
        let zi = m["z"]["im"].as_f64().unwrap();
        assert!((zr - 100.0).abs() < 2.0, "{zr}");
        assert!((zi - x).abs() < 0.03 * x.abs() + 1.0, "{zi} vs {x}");

        let spec = DiagramSpec { kind: DiagramKind::Polar, markers: vec![f0], ..Default::default() };
        let out = diagram_svg(&[&p], &spec, 600.0, 600.0).unwrap();
        parses(&out.svg);
        assert!(out.markers[0]["value"]["mag"].as_f64().unwrap() < 1.0);
    }

    #[test]
    fn table_rows_and_op() {
        let p = rc_ac(1e3, None);
        let spec = DiagramSpec { kind: DiagramKind::Table, vectors: vec!["v(out)".into()], markers: vec![1591.55, 10e3], ..Default::default() };
        let out = diagram_svg(&[&p], &spec, 500.0, 300.0).unwrap();
        parses(&out.svg);
        assert_eq!(out.markers.as_array().unwrap().len(), 2);
        assert!(out.svg.contains(" / -45"));
        let spec = DiagramSpec { kind: DiagramKind::Table, ..Default::default() };
        let out = diagram_svg(&[&p], &spec, 500.0, 300.0).unwrap();
        assert_eq!(out.svg.matches("<text").count(), 1 + 3 * 26); // title + header + 25 rows

        let op = Plot {
            name: "op".into(),
            base: "op".into(),
            kind: "op".into(),
            title: "Operating Point".into(),
            vectors: vec![Vector { name: "v(out)".into(), unit: "V".into(), re: vec![2.5], im: None }, Vector { name: "i(v1)".into(), unit: "A".into(), re: vec![-1e-3], im: None }],
            sweep: None,
        };
        let out = diagram_svg(&[&op], &spec, 400.0, 300.0).unwrap();
        parses(&out.svg);
        assert!(out.svg.contains("2.5V") && out.svg.contains("-1mA"));
        assert!(diagram_svg(&[&op], &DiagramSpec::default(), 400.0, 300.0).is_err());
    }

    #[test]
    fn helpful_errors() {
        let p = rc_ac(1e3, None);
        let e = diagram_svg(&[&p], &DiagramSpec { vectors: vec!["v(nope)".into()], ..Default::default() }, 800.0, 500.0).err().unwrap();
        assert!(e.contains("v(out)") && e.contains("v(nope)"), "{e}");
        let t = tran();
        let e = diagram_svg(&[&t], &DiagramSpec { kind: DiagramKind::Smith, ..Default::default() }, 800.0, 500.0).err().unwrap();
        assert!(e.contains("complex"), "{e}");
        let e = diagram_svg(&[&t], &DiagramSpec { kind: DiagramKind::Polar, vectors: vec!["v(out)".into()], ..Default::default() }, 800.0, 500.0).err().unwrap();
        assert!(e.contains("real"), "{e}");
    }

    #[test]
    fn write_examples() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/diagram_examples");
        std::fs::create_dir_all(&dir).unwrap();
        let a = rc_ac(1e3, Some(json!({"R": 1000.0})));
        let b = rc_ac(3.3e3, Some(json!({"R": 3300.0})));
        let fc = 1591.55;
        let sp = gamma_sp();
        let cases: Vec<(&str, Vec<&Plot>, DiagramSpec, f64, f64)> = vec![
            ("rect", vec![&a, &b], DiagramSpec { vectors: vec!["v(out)".into()], markers: vec![fc, 20e3], ..Default::default() }, 900.0, 600.0),
            ("polar", vec![&sp], DiagramSpec { kind: DiagramKind::Polar, markers: vec![5e8, 1.5e9], ..Default::default() }, 640.0, 640.0),
            ("smith", vec![&sp], DiagramSpec { kind: DiagramKind::Smith, markers: vec![5e8, 1e9, 2e9], ..Default::default() }, 640.0, 640.0),
            ("table", vec![&a], DiagramSpec { kind: DiagramKind::Table, vectors: vec!["v(in)".into(), "v(out)".into()], ..Default::default() }, 500.0, 400.0),
        ];
        let t = tran();
        let mut cases = cases;
        cases.push(("rect_tran", vec![&t], DiagramSpec { markers: vec![1e-4, 3e-4], ..Default::default() }, 800.0, 420.0));
        for (name, plots, spec, w, h) in cases {
            let out = diagram_svg(&plots, &spec, w, h).unwrap();
            assert!(out.svg.len() > 500);
            let png = crate::render::svg_to_png(&out.svg).unwrap();
            std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
        }
    }
}
