//! Qucs-S component libraries (`share/qucs-s/library/*.lib`) and subcircuit
//! symbols: parsing of `<Symbol>` sections (paintings, `.PortSym`, `.ID`).

use crate::library::{Pen, Prim, SymbolShape};
use crate::schematic::{painting_tokens, unquote, Component, Prop, Schematic};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub default: String,
    pub show: bool,
    pub description: String,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedSymbol {
    pub shape: SymbolShape,
    pub params: Vec<Param>,
    pub id_prefix: String,
    pub pin_names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LibComp {
    pub name: String,
    pub description: String,
    pub symbol: ParsedSymbol,
    pub has_spice: bool,
    pub has_qucs_model: bool,
}

#[derive(Clone, Debug)]
pub struct LibFile {
    pub name: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub comps: Vec<LibComp>,
    pub ngspice_ok: bool,
}

pub fn library_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("QUCS_S_LIBRARY") {
        v.push(PathBuf::from(d));
    }
    if let Ok(q) = crate::sim::qucs_s_path() {
        if let Some(bin) = q.parent() {
            v.push(bin.join("..").join("share").join("qucs-s").join("library"));
        }
    }
    v.push(PathBuf::from(r"C:\Program Files\Qucs-S\share\qucs-s\library"));
    v.push(PathBuf::from("/usr/share/qucs-s/library"));
    v.push(PathBuf::from("/usr/local/share/qucs-s/library"));
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        v.push(PathBuf::from(home).join(".qucs").join("user_lib"));
    }
    v.retain(|d| d.is_dir());
    let mut seen = Vec::new();
    v.retain(|d| {
        let c = d.canonicalize().unwrap_or(d.clone());
        if seen.contains(&c) {
            false
        } else {
            seen.push(c);
            true
        }
    });
    v
}

static LIBS: OnceLock<Vec<LibFile>> = OnceLock::new();

/// All libraries (parsed once).
pub fn libraries() -> &'static [LibFile] {
    LIBS.get_or_init(|| {
        let mut out = Vec::new();
        for dir in library_dirs() {
            let black: Vec<String> = std::fs::read_to_string(dir.join("ngspice.blacklist"))
                .unwrap_or_default()
                .lines()
                .map(|l| l.trim().trim_end_matches(".lib").to_string())
                .filter(|l| !l.is_empty())
                .collect();
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "lib").unwrap_or(false)).collect();
            files.sort();
            for f in files {
                if let Some(lf) = parse_lib_file(&f) {
                    if out.iter().any(|o: &LibFile| o.name == lf.name) {
                        continue;
                    }
                    let ok = !black.contains(&lf.name);
                    out.push(LibFile { ngspice_ok: ok, ..lf });
                }
            }
        }
        out
    })
}

pub fn find(lib: &str, comp: &str) -> Option<(&'static LibFile, &'static LibComp)> {
    let l = libraries().iter().find(|l| l.name == lib)?;
    let c = l.comps.iter().find(|c| c.name == comp)?;
    Some((l, c))
}

fn section<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let a = body.find(&open)? + open.len();
    let b = body[a..].find(&close)? + a;
    Some(&body[a..b])
}

pub fn parse_lib_file(path: &Path) -> Option<LibFile> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let first = text.lines().next()?;
    if !first.starts_with("<Qucs Library") {
        return None;
    }
    let name = first.split('"').nth(1).map(|s| s.to_string()).unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
    let mut comps = Vec::new();
    let mut rest: &str = &text;
    while let Some(i) = rest.find("<Component ") {
        let after = &rest[i + 11..];
        let Some(gt) = after.find('>') else { break };
        let cname = after[..gt].trim().to_string();
        let body_end = after.find("</Component>").unwrap_or(after.len());
        let body = &after[gt + 1..body_end];
        let description = section(body, "Description").map(|d| d.trim().lines().map(|l| l.trim()).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let sym = section(body, "Symbol").map(|s| parse_symbol(s.lines())).unwrap_or_default();
        comps.push(LibComp {
            name: cname,
            description,
            symbol: sym,
            has_spice: body.contains("<Spice>") || body.contains("<SpiceAttach"),
            has_qucs_model: body.contains("<Model>"),
        });
        rest = &after[body_end.min(after.len())..];
    }
    Some(LibFile { name, path: path.to_path_buf(), comps, ngspice_ok: true })
}

fn pen_of(t: &[String], i: usize) -> Pen {
    let style = t.get(i + 2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(1);
    Pen {
        color: t.get(i).cloned().unwrap_or_else(|| "#000080".into()),
        width: t.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(1.0),
        style: if style > 1 { "DashLine".into() } else { "SolidLine".into() },
    }
}

/// Parse the lines of a `<Symbol>` section.
pub fn parse_symbol<'a>(lines: impl Iterator<Item = &'a str>) -> ParsedSymbol {
    let mut prims = Vec::new();
    let mut ports: Vec<Option<[f64; 2]>> = Vec::new();
    let mut params = Vec::new();
    let mut id = None;
    let mut prefix = String::new();
    let mut bb = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let grow = |x: f64, y: f64, bb: &mut [f64; 4]| {
        bb[0] = bb[0].min(x);
        bb[1] = bb[1].min(y);
        bb[2] = bb[2].max(x);
        bb[3] = bb[3].max(y);
    };
    for raw in lines {
        let l = raw.trim();
        if !l.starts_with('<') {
            continue;
        }
        let t = painting_tokens(l);
        if t.is_empty() {
            continue;
        }
        let f = |i: usize| t.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        match t[0].as_str() {
            ".PortSym" | "PortSym" => {
                let n = f(3).max(1.0) as usize;
                if ports.len() < n {
                    ports.resize(n, None);
                }
                ports[n - 1] = Some([f(1), f(2)]);
                grow(f(1), f(2), &mut bb);
            }
            ".ID" => {
                id = Some((f(1), f(2)));
                prefix = t.get(3).cloned().unwrap_or_default();
                for q in t.iter().skip(4) {
                    let q = unquote(q);
                    let parts: Vec<&str> = q.splitn(4, '=').collect();
                    if parts.len() >= 3 {
                        params.push(Param {
                            show: parts[0] == "1",
                            name: parts[1].to_string(),
                            default: parts[2].to_string(),
                            description: parts.get(3).map(|s| s.to_string()).unwrap_or_default(),
                        });
                    }
                }
            }
            "Line" => {
                let (x, y, dx, dy) = (f(1), f(2), f(3), f(4));
                prims.push(Prim::Line { x1: x, y1: y, x2: x + dx, y2: y + dy, pen: pen_of(&t, 5) });
                grow(x, y, &mut bb);
                grow(x + dx, y + dy, &mut bb);
            }
            "Rectangle" | "Ellipse" => {
                let (x, y, w, h) = (f(1), f(2), f(3), f(4));
                let brush = if f(10) as i32 == 1 { t.get(8).cloned() } else { None };
                let pen = pen_of(&t, 5);
                prims.push(if t[0] == "Rectangle" { Prim::Rect { x, y, w, h, pen, brush } } else { Prim::Ellipse { x, y, w, h, pen, brush } });
                grow(x, y, &mut bb);
                grow(x + w, y + h, &mut bb);
            }
            "EArc" => {
                let (x, y, w, h) = (f(1), f(2), f(3), f(4));
                prims.push(Prim::Arc { x, y, w, h, angle: f(5) as i32, arclen: f(6) as i32, pen: pen_of(&t, 7) });
                grow(x, y, &mut bb);
                grow(x + w, y + h, &mut bb);
            }
            "Arrow" => {
                let (x, y, dx, dy, hh, hw) = (f(1), f(2), f(3), f(4), f(5), f(6) / 2.0);
                let pen = pen_of(&t, 7);
                prims.push(Prim::Line { x1: x, y1: y, x2: x + dx, y2: y + dy, pen: pen.clone() });
                let len = (dx * dx + dy * dy).sqrt().max(1e-9);
                let (ux, uy) = (dx / len, dy / len);
                let tip = [x + dx, y + dy];
                let base = [tip[0] - ux * hh, tip[1] - uy * hh];
                prims.push(Prim::Polyline {
                    points: vec![[base[0] - uy * hw, base[1] + ux * hw], tip, [base[0] + uy * hw, base[1] - ux * hw]],
                    pen: pen.clone(),
                    brush: Some(pen.color.clone()),
                });
                grow(x, y, &mut bb);
                grow(tip[0], tip[1], &mut bb);
            }
            "Text" => {
                let s = l.find('"').map(|i| l[i + 1..].trim_end_matches('>').trim_end_matches('"').to_string()).unwrap_or_default();
                let ang = f(5).to_radians();
                let size = f(3).max(6.0);
                for (k, line) in s.replace("\\n", "\n").split('\n').enumerate() {
                    prims.push(Prim::Text {
                        x: f(1),
                        y: f(2) + k as f64 * size * 1.3,
                        s: line.to_string(),
                        color: t.get(4).cloned().unwrap_or_else(|| "#000000".into()),
                        size,
                        mcos: ang.cos(),
                        msin: ang.sin(),
                    });
                }
                grow(f(1), f(2), &mut bb);
            }
            _ => {}
        }
    }
    if bb[0] > bb[2] {
        bb = [-20.0, -20.0, 20.0, 20.0];
    }
    let ports: Vec<[f64; 2]> = ports.into_iter().map(|p| p.unwrap_or([0.0, 0.0])).collect();
    // pin names from short text labels drawn right next to a port (e.g. "VCC", "OUT")
    let mut pin_names: Vec<String> = (1..=ports.len()).map(|i| i.to_string()).collect();
    let texts: Vec<(f64, f64, String)> = prims
        .iter()
        .filter_map(|p| match p {
            Prim::Text { x, y, s, size, .. } => {
                let t = s.trim();
                let ok = !t.is_empty() && t.len() <= 6 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '_');
                ok.then(|| {
                    let (w, h) = crate::library::text_size(t, *size);
                    (x + w / 2.0, y + h / 2.0, t.to_string())
                })
            }
            _ => None,
        })
        .collect();
    let mut used: Vec<String> = Vec::new();
    for (i, q) in ports.iter().enumerate() {
        let best = texts
            .iter()
            .map(|t| (((t.0 - q[0]).powi(2) + (t.1 - q[1]).powi(2)).sqrt(), &t.2))
            .filter(|(d, _)| *d < 32.0)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, name)) = best {
            // must be closest to this port than to any other port, and unique
            let nearest_port = ports.iter().enumerate().min_by(|a, b| {
                let t = texts.iter().find(|t| &t.2 == name).unwrap();
                let da = (t.0 - a.1[0]).powi(2) + (t.1 - a.1[1]).powi(2);
                let db = (t.0 - b.1[0]).powi(2) + (t.1 - b.1[1]).powi(2);
                da.total_cmp(&db)
            });
            if nearest_port.map(|n| n.0) == Some(i) && !used.contains(name) && name.parse::<usize>().is_err() {
                pin_names[i] = name.clone();
                used.push(name.clone());
            }
        }
    }
    let (tx, ty) = id.unwrap_or((bb[0] + 4.0, bb[3] + 4.0));
    ParsedSymbol { shape: SymbolShape { prims, ports, bbox: bb, tx, ty, rotated: 0 }, params, id_prefix: prefix, pin_names }
}

/// Symbol of a `Lib` component (props: Lib, Comp, params...).
pub fn lib_symbol(c: &Component) -> Option<ParsedSymbol> {
    let lib = c.props.first()?.value.clone();
    let comp = c.props.get(1)?.value.clone();
    find(&lib, &comp).map(|(_, lc)| lc.symbol.clone()).filter(|s| !s.shape.prims.is_empty() || !s.shape.ports.is_empty())
}

static SUB_CACHE: OnceLock<Mutex<HashMap<PathBuf, (Option<std::time::SystemTime>, Option<ParsedSymbol>)>>> = OnceLock::new();

/// Symbol of a `Sub` component from the `<Symbol>` section of the subcircuit file.
pub fn sub_symbol(c: &Component, sch_dir: Option<&Path>) -> Option<ParsedSymbol> {
    let file = c.props.first()?.value.clone();
    let p = Path::new(&file);
    let path = if p.is_absolute() { p.to_path_buf() } else { sch_dir?.join(p) };
    let m = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let cache = SUB_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((mt, sym)) = cache.lock().ok()?.get(&path) {
        if *mt == m {
            return sym.clone();
        }
    }
    let sym = Schematic::load(&path).ok().and_then(|sub| {
        if sub.symbol.iter().any(|l| l.contains(".PortSym")) {
            Some(parse_symbol(sub.symbol.iter().map(|s| s.as_str())))
        } else {
            None
        }
    });
    if let Ok(mut c) = cache.lock() {
        c.insert(path, (m, sym.clone()));
    }
    sym
}

/// New `Lib` component with default parameters.
pub fn new_lib_component(lib: &str, comp: &str, name: &str, x: i32, y: i32) -> Result<Component, String> {
    let (_, lc) = find(lib, comp).ok_or_else(|| format!("no library component {lib}/{comp} (search_library)"))?;
    let mut props = vec![Prop { value: lib.into(), display: "0".into() }, Prop { value: comp.into(), display: "1".into() }];
    for p in &lc.symbol.params {
        props.push(Prop { value: p.default.clone(), display: if p.show { "1" } else { "0" }.into() });
    }
    Ok(Component {
        typ: "Lib".into(),
        name: name.into(),
        active: 1,
        x,
        y,
        tx: lc.symbol.shape.tx.round() as i32,
        ty: lc.symbol.shape.ty.round() as i32,
        mirror: 0,
        rotate: 0,
        props,
    })
}

/// Property name for Lib / Sub parameters.
pub fn param_name(c: &Component, i: usize, sch_dir: Option<&Path>) -> Option<String> {
    match c.typ.as_str() {
        "Lib" => match i {
            0 => Some("Lib".into()),
            1 => Some("Comp".into()),
            _ => lib_symbol(c).and_then(|s| s.params.get(i - 2).map(|p| p.name.clone())),
        },
        "Sub" => match i {
            0 => Some("File".into()),
            _ => sub_symbol(c, sch_dir).and_then(|s| s.params.get(i - 1).map(|p| p.name.clone())),
        },
        _ => None,
    }
}

/// Search libraries: every word must match library name, component name or description.
pub fn search(query: &str, limit: usize) -> Vec<(&'static LibFile, &'static LibComp)> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    for l in libraries() {
        for c in &l.comps {
            let hay = format!("{} {} {}", l.name, c.name, c.description).to_lowercase();
            if words.iter().all(|w| hay.contains(w)) {
                out.push((l, c));
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}
