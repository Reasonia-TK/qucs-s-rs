//! Number formatting shared by the result diagrams and the GUI.

/// Engineering notation with SI prefix: 1.5e-9 -> "1.5n".
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

