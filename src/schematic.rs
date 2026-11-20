//! Qucs-S schematic (.sch) model: parsing and serialisation.
//!
//! Components and wires are modelled; every other section (Properties, Symbol,
//! Diagrams, Paintings) is kept verbatim so files round-trip safely.

use std::fmt::Write as _;
use std::path::Path;

pub const DEFAULT_VERSION: &str = "26.1.1";

#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub value: String,
    pub display: String, // "1" / "0" (kept verbatim)
}

impl Prop {
    pub fn visible(&self) -> bool {
        self.display.trim() == "1"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub typ: String,
    pub name: String,
    pub active: i32, // bit 0..1: 1 active, 0 off, 2 shorted; bit 2 (4) hides the name
    pub x: i32,
    pub y: i32,
    pub tx: i32,
    pub ty: i32,
    pub mirror: i32,
    pub rotate: i32,
    pub props: Vec<Prop>,
}

impl Component {
    pub fn show_name(&self) -> bool {
        self.active & 4 == 0
    }

    pub fn parse(body: &str) -> Result<Component, String> {
        let t = tokenize(body);
        if t.len() < 9 {
            return Err(format!("malformed component line: <{body}>"));
        }
        let int = |s: &str| -> Result<i32, String> {
            s.parse::<f64>()
                .map(|v| v.round() as i32)
                .map_err(|_| format!("bad number '{s}' in <{body}>"))
        };
        let mut props = Vec::new();
        let rest = &t[9..];
        let mut i = 0;
        while i < rest.len() {
            let value = unquote(&rest[i]);
            let display = rest.get(i + 1).cloned().unwrap_or_else(|| "0".into());
            props.push(Prop { value, display });
            i += 2;
        }
        Ok(Component {
            typ: t[0].clone(),
            name: t[1].clone(),
            active: int(&t[2])?,
            x: int(&t[3])?,
            y: int(&t[4])?,
            tx: int(&t[5])?,
            ty: int(&t[6])?,
            mirror: int(&t[7])?,
            rotate: int(&t[8])?,
            props,
        })
    }

    pub fn serialize(&self) -> String {
        let mut s = format!(
            "<{} {} {} {} {} {} {} {} {}",
            self.typ, self.name, self.active, self.x, self.y, self.tx, self.ty, self.mirror, self.rotate
        );
        for p in &self.props {
            let _ = write!(s, " {} {}", quote(&p.value), p.display);
        }
        s.push('>');
        s
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wire {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub label: String,
    pub lx: i32,
    pub ly: i32,
    pub loff: i32,
    pub extra: String,
}

impl Wire {
    /// Qucs normalises wires on load so that (x1,y1) is the left / upper end.
    pub fn new(x1: i32, y1: i32, x2: i32, y2: i32) -> Wire {
        let (x1, y1, x2, y2) = if x1 > x2 || (x1 == x2 && y1 > y2) { (x2, y2, x1, y1) } else { (x1, y1, x2, y2) };
        Wire { x1, y1, x2, y2, label: String::new(), lx: 0, ly: 0, loff: 0, extra: String::new() }
    }

    /// Set the label so that its root (anchor) is at `anchor` on the wire.
    pub fn set_label(&mut self, name: &str, lx: i32, ly: i32, anchor: (i32, i32)) {
        self.label = name.to_string();
        self.lx = lx;
        self.ly = ly;
        self.loff = (anchor.0 - self.x1).abs() + (anchor.1 - self.y1).abs();
    }

    pub fn p1(&self) -> (i32, i32) {
        (self.x1, self.y1)
    }
    pub fn p2(&self) -> (i32, i32) {
        (self.x2, self.y2)
    }
    pub fn is_point(&self) -> bool {
        self.x1 == self.x2 && self.y1 == self.y2
    }
    pub fn is_horizontal(&self) -> bool {
        self.y1 == self.y2
    }
    pub fn is_vertical(&self) -> bool {
        self.x1 == self.x2
    }
    pub fn len(&self) -> i32 {
        (self.x2 - self.x1).abs() + (self.y2 - self.y1).abs()
    }

    /// True when (x, y) lies on the segment (end points included when `ends`).
    pub fn contains(&self, x: i32, y: i32, ends: bool) -> bool {
        if (x, y) == self.p1() || (x, y) == self.p2() {
            return ends;
        }
        let (x1, y1, x2, y2) = (self.x1, self.y1, self.x2, self.y2);
        if x < x1.min(x2) || x > x1.max(x2) || y < y1.min(y2) || y > y1.max(y2) {
            return false;
        }
        (x2 - x1) as i64 * (y - y1) as i64 == (y2 - y1) as i64 * (x - x1) as i64
    }

    pub fn parse(body: &str) -> Result<Wire, String> {
        let t = tokenize(body);
        if t.len() < 4 {
            return Err(format!("malformed wire line: <{body}>"));
        }
        let int = |s: &str| s.parse::<f64>().map(|v| v.round() as i32).unwrap_or(0);
        let mut w = Wire::new(int(&t[0]), int(&t[1]), int(&t[2]), int(&t[3]));
        let swapped = (w.x1, w.y1) != (int(&t[0]), int(&t[1]));
        let _ = swapped;
        if t.len() > 4 {
            w.label = unquote(&t[4]);
        }
        if t.len() > 7 {
            w.lx = int(&t[5]);
            w.ly = int(&t[6]);
            w.loff = int(&t[7]);
        }
        if t.len() > 8 {
            w.extra = unquote(&t[8]);
        }
        Ok(w)
    }

    pub fn serialize(&self) -> String {
        format!(
            "<{} {} {} {} {} {} {} {} {}>",
            self.x1,
            self.y1,
            self.x2,
            self.y2,
            quote(&self.label),
            self.lx,
            self.ly,
            self.loff,
            quote(&self.extra)
        )
    }

    /// Anchor ("root") of the wire label: `loff` units from the left/upper end.
    pub fn label_anchor(&self) -> (i32, i32) {
        if self.is_point() {
            return (self.x1, self.y1);
        }
        let len = self.len().max(1);
        let d = self.loff.clamp(0, len);
        if self.is_horizontal() {
            (self.x1 + d * (self.x2 - self.x1).signum(), self.y1)
        } else if self.is_vertical() {
            (self.x1, self.y1 + d * (self.y2 - self.y1).signum())
        } else {
            let t = d as f64 / ((((self.x2 - self.x1).pow(2) + (self.y2 - self.y1).pow(2)) as f64).sqrt().max(1.0));
            (self.x1 + ((self.x2 - self.x1) as f64 * t).round() as i32, self.y1 + ((self.y2 - self.y1) as f64 * t).round() as i32)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Schematic {
    pub version: String,
    pub properties: Vec<String>,
    pub symbol: Vec<String>,
    pub components: Vec<Component>,
    pub wires: Vec<Wire>,
    pub diagrams: Vec<String>,
    pub paintings: Vec<String>,
    pub extra_sections: Vec<(String, Vec<String>)>,
}

impl Schematic {
    pub fn new_empty(dataset_stem: &str) -> Schematic {
        let props = [
            "<View=0,0,1200,900,1,0,0>".to_string(),
            "<Grid=10,10,1>".into(),
            format!("<DataSet={dataset_stem}.dat>"),
            format!("<DataDisplay={dataset_stem}.dpl>"),
            "<OpenDisplay=0>".into(),
            "<Script=>".into(),
            "<RunScript=0>".into(),
            "<showFrame=0>".into(),
            "<FrameText0=Title>".into(),
            "<FrameText1=Drawn By:>".into(),
            "<FrameText2=Date:>".into(),
            "<FrameText3=Revision:>".into(),
        ];
        Schematic {
            version: DEFAULT_VERSION.into(),
            properties: props.iter().map(|s| format!("  {s}")).collect(),
            ..Default::default()
        }
    }

    pub fn parse(text: &str) -> Result<Schematic, String> {
        let mut sch = Schematic { version: DEFAULT_VERSION.into(), ..Default::default() };
        let mut section: Option<String> = None;
        let mut saw_header = false;
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if !saw_header {
                if let Some(v) = t.strip_prefix("<Qucs Schematic") {
                    sch.version = v.trim_end_matches('>').trim().to_string();
                    saw_header = true;
                    continue;
                }
                return Err("not a Qucs schematic (missing '<Qucs Schematic ...>' header)".into());
            }
            if let Some(sec) = section.clone() {
                if t == format!("</{sec}>") {
                    section = None;
                    continue;
                }
                let body = || t.trim_start_matches('<').trim_end_matches('>').to_string();
                match sec.as_str() {
                    "Components" => sch.components.push(Component::parse(&body())?),
                    "Wires" => sch.wires.push(Wire::parse(&body())?),
                    "Properties" => sch.properties.push(line.to_string()),
                    "Symbol" => sch.symbol.push(line.to_string()),
                    "Diagrams" => sch.diagrams.push(line.to_string()),
                    "Paintings" => sch.paintings.push(line.to_string()),
                    _ => {
                        if let Some(e) = sch.extra_sections.iter_mut().find(|e| e.0 == sec) {
                            e.1.push(line.to_string());
                        }
                    }
                }
                continue;
            }
            if t.starts_with('<') && t.ends_with('>') && !t.starts_with("</") && !t.contains(' ') {
                let name = t[1..t.len() - 1].to_string();
                if !matches!(
                    name.as_str(),
                    "Components" | "Wires" | "Properties" | "Symbol" | "Diagrams" | "Paintings"
                ) {
                    sch.extra_sections.push((name.clone(), Vec::new()));
                }
                section = Some(name);
            }
        }
        if !saw_header {
            return Err("empty file".into());
        }
        Ok(sch)
    }

    pub fn load(path: &Path) -> Result<Schematic, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        Schematic::parse(&text)
    }

    pub fn serialize(&self) -> String {
        let mut s = format!("<Qucs Schematic {}>\n", self.version);
        let raw = |name: &str, lines: &[String], s: &mut String| {
            let _ = writeln!(s, "<{name}>");
            for l in lines {
                let _ = writeln!(s, "{l}");
            }
            let _ = writeln!(s, "</{name}>");
        };
        raw("Properties", &self.properties, &mut s);
        raw("Symbol", &self.symbol, &mut s);
        s.push_str("<Components>\n");
        for c in &self.components {
            let _ = writeln!(s, "  {}", c.serialize());
        }
        s.push_str("</Components>\n<Wires>\n");
        for w in &self.wires {
            let _ = writeln!(s, "  {}", w.serialize());
        }
        s.push_str("</Wires>\n");
        raw("Diagrams", &self.diagrams, &mut s);
        raw("Paintings", &self.paintings, &mut s);
        for (n, l) in &self.extra_sections {
            raw(n, l, &mut s);
        }
        s
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
        }
        // write to a temp file first so a GUI watching the file never sees a half file
        let tmp = path.with_extension("sch.tmp");
        std::fs::write(&tmp, self.serialize()).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    pub fn component(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }
    pub fn component_mut(&mut self, name: &str) -> Option<&mut Component> {
        self.components.iter_mut().find(|c| c.name == name)
    }
    pub fn component_index(&self, name: &str) -> Option<usize> {
        self.components.iter().position(|c| c.name == name)
    }
}

// ---------------------------------------------------------------------------

pub fn tokenize(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += 1;
            }
            out.push(chars[i..(j + 1).min(chars.len())].iter().collect());
            i = j + 1;
        } else {
            let mut j = i;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            out.push(chars[i..j].iter().collect());
            i = j;
        }
    }
    out
}

pub fn unquote(t: &str) -> String {
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

pub fn quote(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "'"))
}

/// Parse a painting line such as `<Line 10 20 30 0 #000080 2 1>`.
pub fn painting_tokens(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('<').trim_end_matches('>');
    tokenize(t)
}
