//! Minimal PDF writer: one page holding a raster image of the schematic
//! (rendered by the same scene code as PNG export). Used for PDF export and
//! printing (the PDF is handed to the system viewer).

use std::fmt::Write as _;

/// A4 in PDF points.
const A4: (f64, f64) = (595.28, 841.89);
const MARGIN: f64 = 28.35; // 10 mm

/// Build a single-page PDF from RGBA pixels (alpha is flattened onto white).
/// The page is A4, landscape when the image is wider than tall.
pub fn image_pdf(w: u32, h: u32, rgba: &[u8], title: &str) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 || rgba.len() < (w as usize) * (h as usize) * 4 {
        return Err("empty image".into());
    }
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for px in rgba.as_chunks::<4>().0 {
        let a = px[3] as u32;
        for c in &px[..3] {
            // premultiplied alpha composited onto white
            rgb.push((*c as u32 + (255 - a)).min(255) as u8);
        }
    }
    let data = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
    let (pw, ph) = if w > h { (A4.1, A4.0) } else { A4 };
    let k = ((pw - 2.0 * MARGIN) / w as f64).min((ph - 2.0 * MARGIN) / h as f64);
    let (iw, ih) = (w as f64 * k, h as f64 * k);
    let (ix, iy) = ((pw - iw) / 2.0, (ph - ih) / 2.0);
    let content = format!("q {iw:.3} 0 0 {ih:.3} {ix:.3} {iy:.3} cm /Im0 Do Q\n");

    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let mut obj = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        let n = offsets.len();
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    obj(&mut out, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    obj(
        &mut out,
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>").as_bytes(),
    );
    let mut img = format!("<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n", data.len()).into_bytes();
    img.extend_from_slice(&data);
    img.extend_from_slice(b"\nendstream");
    obj(&mut out, &img);
    let mut cs = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    cs.extend_from_slice(content.as_bytes());
    cs.extend_from_slice(b"endstream");
    obj(&mut out, &cs);
    obj(&mut out, format!("<< /Title ({}) /Producer (qucs-s-rs {}) >>", pdf_string(title), env!("CARGO_PKG_VERSION")).as_bytes());
    let xref = out.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
    for o in &offsets {
        let _ = writeln!(x, "{o:010} 00000 n ");
    }
    let _ = write!(x, "trailer\n<< /Size {} /Root 1 0 R /Info {} 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1, offsets.len());
    out.extend_from_slice(x.as_bytes());
    Ok(out)
}

/// ASCII-only, escaped PDF literal string.
fn pdf_string(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            c if c.is_ascii() && !c.is_ascii_control() => c.to_string(),
            _ => "?".into(),
        })
        .collect()
}

/// Render the schematic for print (white background, no overlays) and wrap it in a PDF.
pub fn schematic_pdf(sch: &crate::schematic::Schematic, conn: &crate::netlist::Connectivity, title: &str) -> Result<Vec<u8>, String> {
    let ov = crate::render::Overlay::default();
    let sc = crate::render::build_scene(sch, conn, &ov);
    // ~200 dpi on A4
    let view = crate::render::fit_view(sc.bounds, 30.0, Some(4.0), 2400.0);
    let (svg, _) = crate::render::scene_svg(&sc, &view, &crate::render::Decor::default());
    let (w, h, rgba) = crate::render::svg_to_rgba(&svg)?;
    image_pdf(w, h, &rgba, title)
}

/// Open a file with the system's default application (used to print the PDF).
pub fn open_with_system(path: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    let r = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path).creation_flags(0x0800_0000).spawn()
    };
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    r.map(|_| ()).map_err(|e| format!("cannot open {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_well_formed_pdf() {
        let (w, h) = (40u32, 20u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % 255) as u8, 0, 255, 255]).collect();
        let pdf = image_pdf(w, h, &rgba, "t (1)").unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("/Width 40 /Height 20"));
        assert!(text.contains("/Title (t \\(1\\))"));
        // landscape page for a wide image
        assert!(text.contains("/MediaBox [0 0 841.89 595.28]"));
        // xref offsets (byte positions) point at the objects
        let sx: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(pdf[sx..].starts_with(b"xref"));
        let xref = String::from_utf8(pdf[sx..].to_vec()).unwrap();
        for (k, line) in xref.lines().skip(3).take(6).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(pdf[off..].starts_with(format!("{} 0 obj", k + 1).as_bytes()), "object {}", k + 1);
        }
        // the image stream inflates back to RGB
        let start = pdf.windows(7).position(|w| w == b"stream\n").unwrap() + 7;
        let head = String::from_utf8_lossy(&pdf[..start]);
        let len: usize = head.rsplit("/Length ").next().unwrap().split_whitespace().next().unwrap().parse().unwrap();
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&pdf[start..start + len]).unwrap();
        assert_eq!(raw.len(), (w * h * 3) as usize);
    }

    #[test]
    fn schematic_to_pdf() {
        let d = crate::doc::Doc::open(std::path::Path::new("examples/inverting_amp.sch")).unwrap();
        let pdf = schematic_pdf(&d.sch, &d.conn(), "inverting_amp").unwrap();
        assert!(pdf.len() > 2000);
    }
}
