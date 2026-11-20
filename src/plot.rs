//! Simple result plots rendered to SVG (and PNG through resvg).

use crate::render::NET_PALETTE;
use crate::sim::Plot;
use std::fmt::Write as _;

pub struct Series {
    pub label: String,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
}

pub struct Panel {
    pub ylabel: String,
    pub series: Vec<Series>,
}

fn nice_ticks(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    if !(hi > lo) {
        return vec![lo];
    }
    let raw = (hi - lo) / n as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(raw);
    let mut t = (lo / step).ceil() * step;
    let mut out = Vec::new();
    while t <= hi + step * 1e-9 {
        out.push(if t.abs() < step * 1e-9 { 0.0 } else { t });
        t += step;
    }
    out
}

pub fn eng(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    let (d, s) = [(1e12, "T"), (1e9, "G"), (1e6, "M"), (1e3, "k"), (1.0, ""), (1e-3, "m"), (1e-6, "µ"), (1e-9, "n"), (1e-12, "p"), (1e-15, "f")]
        .iter()
        .find(|(d, _)| a >= *d * 0.999)
        .copied()
        .unwrap_or((1e-15, "f"));
    let x = v / d;
    let t = format!("{:.3}", x);
    let t = t.trim_end_matches('0').trim_end_matches('.').to_string();
    format!("{t}{s}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn chart_svg(title: &str, xlabel: &str, logx: bool, panels: &[Panel], w: f64, h_panel: f64) -> String {
    let (ml, mr, mt, mb) = (72.0, 20.0, 36.0, 44.0);
    let ph = h_panel;
    let h = mt + panels.len() as f64 * (ph + 18.0) + mb;
    let pw = w - ml - mr;
    let mut s = String::new();
    let _ = write!(s, r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="Segoe UI, Arial, sans-serif"><rect width="{w}" height="{h}" fill="#fff"/>"##);
    let _ = write!(s, r##"<text x="{}" y="22" font-size="15" font-weight="bold" fill="#222" text-anchor="middle">{}</text>"##, w / 2.0, esc(title));
    // x range over all series
    let mut xmin = f64::MAX;
    let mut xmax = f64::MIN;
    for p in panels {
        for se in &p.series {
            for &x in &se.x {
                if logx && x <= 0.0 {
                    continue;
                }
                xmin = xmin.min(x);
                xmax = xmax.max(x);
            }
        }
    }
    if xmin >= xmax {
        xmax = xmin + 1.0;
    }
    let fx = |x: f64| -> f64 {
        if logx {
            (x.max(1e-300).log10() - xmin.log10()) / (xmax.log10() - xmin.log10())
        } else {
            (x - xmin) / (xmax - xmin)
        }
    };
    for (pi, p) in panels.iter().enumerate() {
        let top = mt + pi as f64 * (ph + 18.0);
        let mut ymin = f64::MAX;
        let mut ymax = f64::MIN;
        for se in &p.series {
            for &y in &se.y {
                if y.is_finite() {
                    ymin = ymin.min(y);
                    ymax = ymax.max(y);
                }
            }
        }
        if ymin > ymax {
            ymin = 0.0;
            ymax = 1.0;
        }
        if (ymax - ymin).abs() < 1e-30 {
            ymin -= 1.0_f64.max(ymin.abs() * 0.1);
            ymax += 1.0_f64.max(ymax.abs() * 0.1);
        }
        let pad = (ymax - ymin) * 0.06;
        let (ymin, ymax) = (ymin - pad, ymax + pad);
        let fy = |y: f64| top + ph - (y - ymin) / (ymax - ymin) * ph;
        let _ = write!(s, r##"<rect x="{ml}" y="{top}" width="{pw}" height="{ph}" fill="#fafbfc" stroke="#999"/>"##);
        for t in nice_ticks(ymin, ymax, 6) {
            let y = fy(t);
            let _ = write!(s, r##"<line x1="{ml}" y1="{y:.1}" x2="{}" y2="{y:.1}" stroke="#e3e6ea"/><text x="{}" y="{:.1}" font-size="11" fill="#444" text-anchor="end">{}</text>"##, ml + pw, ml - 5.0, y + 4.0, eng(t));
        }
        // x grid
        if logx {
            let mut d = 10f64.powf(xmin.log10().floor());
            while d <= xmax * 1.0001 {
                for m in 1..10 {
                    let x = d * m as f64;
                    if x < xmin || x > xmax {
                        continue;
                    }
                    let px = ml + fx(x) * pw;
                    let col = if m == 1 { "#cfd4da" } else { "#eef0f3" };
                    let _ = write!(s, r##"<line x1="{px:.1}" y1="{top}" x2="{px:.1}" y2="{}" stroke="{col}"/>"##, top + ph);
                    if m == 1 && pi == panels.len() - 1 {
                        let _ = write!(s, r##"<text x="{px:.1}" y="{}" font-size="11" fill="#444" text-anchor="middle">{}</text>"##, top + ph + 15.0, eng(x));
                    }
                }
                d *= 10.0;
            }
        } else {
            for t in nice_ticks(xmin, xmax, 8) {
                let px = ml + fx(t) * pw;
                let _ = write!(s, r##"<line x1="{px:.1}" y1="{top}" x2="{px:.1}" y2="{}" stroke="#e3e6ea"/>"##, top + ph);
                if pi == panels.len() - 1 {
                    let _ = write!(s, r##"<text x="{px:.1}" y="{}" font-size="11" fill="#444" text-anchor="middle">{}</text>"##, top + ph + 15.0, eng(t));
                }
            }
        }
        let _ = write!(s, r##"<text x="16" y="{}" font-size="12" fill="#333" text-anchor="middle" transform="rotate(-90 16 {})">{}</text>"##, top + ph / 2.0, top + ph / 2.0, esc(&p.ylabel));
        for (k, se) in p.series.iter().enumerate() {
            let c = NET_PALETTE[k % NET_PALETTE.len()].hex();
            let mut pts = String::new();
            // decimate to <= 4000 points
            let step = (se.x.len() / 4000).max(1);
            for i in (0..se.x.len().min(se.y.len())).step_by(step) {
                let (x, y) = (se.x[i], se.y[i]);
                if !y.is_finite() || (logx && x <= 0.0) {
                    continue;
                }
                let _ = write!(pts, "{:.1},{:.1} ", ml + fx(x) * pw, fy(y));
            }
            let _ = write!(s, r##"<polyline points="{pts}" fill="none" stroke="{c}" stroke-width="1.8"/>"##);
            let ly = top + 14.0 + k as f64 * 15.0;
            let _ = write!(s, r##"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{c}" stroke-width="2.5"/><text x="{}" y="{}" font-size="11" fill="#222">{}</text>"##, ml + pw - 150.0, ly - 4.0, ml + pw - 130.0, ly - 4.0, ml + pw - 125.0, ly, esc(&se.label));
        }
    }
    let _ = write!(s, r##"<text x="{}" y="{}" font-size="12" fill="#333" text-anchor="middle">{}</text>"##, ml + pw / 2.0, h - 10.0, esc(xlabel));
    s.push_str("</svg>");
    s
}

/// Plot selected vectors of (possibly several sweep steps of) an analysis.
pub fn plot_analysis(plots: &[&Plot], vectors: &[String], db: Option<bool>) -> Result<String, String> {
    let p0 = plots.first().ok_or("no data")?;
    let scale = p0.scale().ok_or("this analysis has no x axis (operating point) – use get_results")?;
    let names: Vec<String> = if vectors.is_empty() {
        p0.vectors.iter().skip(1).filter(|v| v.name.starts_with("v(") || !v.name.contains('#')).take(6).map(|v| v.name.clone()).collect()
    } else {
        vectors.to_vec()
    };
    let logx = p0.kind == "ac" || p0.kind == "noise";
    let complex = names.iter().any(|n| p0.vector(n).map(|v| v.is_complex()).unwrap_or(false));
    let use_db = db.unwrap_or(complex);
    let mut mag = Panel { ylabel: if use_db { "magnitude [dB]".into() } else { "value".into() }, series: vec![] };
    let mut ph = Panel { ylabel: "phase [deg]".into(), series: vec![] };
    for p in plots {
        let x = p.vectors[0].re.clone();
        for n in &names {
            let v = p.vector(n).ok_or_else(|| format!("no vector '{n}' in {}. Vectors: {}", p.name, p.vectors.iter().map(|v| v.name.clone()).collect::<Vec<_>>().join(", ")))?;
            let mut label = v.name.clone();
            if let Some(sw) = &p.sweep {
                label = format!("{label} {}", sw.as_object().map(|o| o.iter().map(|(k, v)| format!("{k}={}", eng(v.as_f64().unwrap_or(0.0)))).collect::<Vec<_>>().join(" ")).unwrap_or_default());
            }
            let n = v.re.len();
            if v.is_complex() || use_db {
                let y: Vec<f64> = (0..n).map(|i| if use_db { 20.0 * v.mag(i).max(1e-300).log10() } else { v.mag(i) }).collect();
                mag.series.push(Series { label: label.clone(), x: x.clone(), y });
                if v.is_complex() {
                    // unwrapped phase
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
                    ph.series.push(Series { label, x: x.clone(), y: out });
                }
            } else {
                mag.series.push(Series { label, x: x.clone(), y: v.re.clone() });
            }
        }
    }
    let mut panels = vec![mag];
    if !ph.series.is_empty() {
        panels.push(ph);
    }
    let title = format!("{} – {}", p0.base, p0.title);
    Ok(chart_svg(&title, &format!("{} [{}]", scale.name, scale.unit), logx, &panels, 900.0, if panels.len() > 1 { 230.0 } else { 380.0 }))
}
