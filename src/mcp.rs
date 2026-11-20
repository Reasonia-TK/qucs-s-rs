//! MCP server over stdio (JSON-RPC 2.0, newline delimited).

use crate::check;
use crate::doc::{apply_props, Doc};
use crate::library::{self, lib};
use crate::render::{build_scene, fit_view, scene_svg, svg_to_png, Decor, Overlay};
use crate::schematic::Schematic;
use crate::sim::{self, SimResult};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const INSTRUCTIONS: &str = "Qucs-S (Rust fork) schematic editor. Coordinates are Qucs units: x to the right, y DOWN, 10-unit grid. \
Pins are addressed as \"R1.1\", \"V1.+\", \"OP1.OUT\"; points as \"[x,y]\"; nets as \"net:name\". \
Workflow: new_schematic -> add_components (signal left->right, supply at top, ground at bottom, leave >= 60 units between parts) -> \
connect (one call per net; the router avoids symbols, never overlaps other nets and keeps bends low) -> add_net_label for nodes you want to observe -> \
snapshot (look at the picture! it is exactly what the editor shows) -> check_layout -> fix with move_components / reroute / tidy -> \
add_simulation -> run_simulation -> get_results / plot_results. Every edit is saved immediately; open_gui shows the live editor to the user.";

pub struct Server {
    doc: Option<Doc>,
    loaded_mtime: Option<SystemTime>,
    sim: Option<SimResult>,
    snap_counter: usize,
}

fn tool(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": desc, "inputSchema": {"type": "object", "properties": props, "required": required}})
}

fn tools() -> Vec<Value> {
    let comp_item = json!({
        "type": "object",
        "properties": {
            "type": {"type": "string", "description": "component type, e.g. R, C, L, Vdc, Vac, Vpulse, GND, OpAmp, Diode, _BJT ... (list_component_types), or \"Lib\" for a library part"},
            "lib": {"type": "string", "description": "for type Lib: library name, e.g. \"OpAmps\" (search_library)"},
            "comp": {"type": "string", "description": "for type Lib: component name in the library, e.g. \"LM358\""},
            "name": {"type": "string"},
            "x": {"type": "integer"}, "y": {"type": "integer"},
            "rotate": {"type": "integer", "description": "Qucs rotation count 0..3 (each 90° counter-clockwise). Default = the type's default orientation (sources are vertical = 1)."},
            "mirror": {"type": "boolean"},
            "props": {"type": "object", "description": "property values by name, e.g. {\"R\": \"10k\"}; use {\"value\": .., \"show\": true} to toggle display"},
            "align": {"type": "object", "description": "place so that pin `pin` of this part lands at `to` (pin or [x,y]) + (dx,dy). Overrides x/y.",
                "properties": {"pin": {"type": "string", "description": "pin name of this component, e.g. \"1\""}, "to": {"type": "string"}, "dx": {"type": "integer"}, "dy": {"type": "integer"}}}
        },
        "required": ["type"]
    });
    vec![
        tool("new_schematic", "Create a new empty schematic file (overwrites only if overwrite=true).", json!({"path": {"type": "string"}, "overwrite": {"type": "boolean"}}), &["path"]),
        tool("open_schematic", "Open an existing Qucs-S .sch file.", json!({"path": {"type": "string"}}), &["path"]),
        tool("save_schematic", "Save (optionally to a new path). Edits are saved automatically anyway.", json!({"path": {"type": "string"}}), &[]),
        tool("get_schematic", "Components (with absolute pin positions and nets), wires and nets of the open schematic.", json!({"include_wires": {"type": "boolean"}}), &[]),
        tool("list_component_types", "Search the component library (220 Qucs-S types).", json!({"query": {"type": "string"}, "category": {"type": "string"}}), &[]),
        tool("search_library", "Search the Qucs-S component libraries (real parts: op-amps, transistors, diodes, regulators, transformers, ...). Returns lib/comp names, description, pins and parameters; place with add_components {type:\"Lib\", lib, comp}.",
            json!({"query": {"type": "string", "description": "words to match, e.g. \"lm358\", \"zener 5v\", \"mosfet irf\""}, "lib": {"type": "string", "description": "restrict to one library"}, "limit": {"type": "integer"}}), &[]),
        tool("get_component_type", "Properties (name/default/description), pins and symbol size of a component type.", json!({"type": {"type": "string"}}), &["type"]),
        tool("add_components", "Place components. Returns their pins (absolute coordinates and lead direction) so you can plan wiring.", json!({"components": {"type": "array", "items": comp_item}}), &["components"]),
        tool("update_component", "Change properties, orientation, position or name of a component (wires are NOT moved; use move_components for that).",
            json!({"name": {"type": "string"}, "props": {"type": "object"}, "rotate": {"type": "integer"}, "mirror": {"type": "boolean"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "new_name": {"type": "string"}, "mirror_y": {"type": "boolean", "description": "mirror about the Y axis (left-right)"}, "active": {"type": "integer", "description": "1 active, 0 off, 2 shorted"}, "label_offset": {"type": "array", "items": {"type": "integer"}, "description": "[tx, ty] text position relative to the component"}}), &["name"]),
        tool("move_components", "Move components (absolute x/y or relative dx/dy) and re-route the wires attached to them.",
            json!({"moves": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "dx": {"type": "integer"}, "dy": {"type": "integer"}}, "required": ["name"]}}, "reroute": {"type": "boolean", "description": "default true"}}), &["moves"]),
        tool("duplicate_components", "Copy components (and the wires between them) with an offset; names are renumbered. Returns the new names.",
            json!({"names": {"type": "array", "items": {"type": "string"}}, "dx": {"type": "integer"}, "dy": {"type": "integer"}}), &["names"]),
        tool("align_components", "Align or distribute components: mode left|right|top|bottom|hcenter|vcenter|distribute_h|distribute_v. Wires are re-routed.",
            json!({"names": {"type": "array", "items": {"type": "string"}}, "mode": {"type": "string"}}), &["names", "mode"]),
        tool("add_painting", "Add a drawing: kind line|rect|ellipse|arrow|text. line/arrow: (x,y)->(x+w,y+h); rect/ellipse: box; text: text at (x,y).",
            json!({"kind": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "w": {"type": "integer"}, "h": {"type": "integer"}, "text": {"type": "string"}, "size": {"type": "integer"}, "color": {"type": "string", "description": "#rrggbb"}, "width": {"type": "integer"}, "filled": {"type": "boolean"}}), &["kind", "x", "y"]),
        tool("delete_paintings", "Delete drawings by index (get_schematic lists them) or all.", json!({"indices": {"type": "array", "items": {"type": "integer"}}, "all": {"type": "boolean"}}), &[]),
        tool("export_image", "Write the schematic as PNG or SVG (report style, no overlays) to a file.", json!({"path": {"type": "string"}, "scale": {"type": "number", "description": "pixels per unit, default 2"}}), &["path"]),
        tool("delete_components", "Delete components (and optionally the wires of nets left without pins).", json!({"names": {"type": "array", "items": {"type": "string"}}, "delete_dangling_wires": {"type": "boolean"}}), &["names"]),
        tool("connect", "Auto-route wires. Each entry of `nets` is a list of terminals that become one net (\"R1.2\", \"[x,y]\", \"net:name\"). Optional labels give the net a name.",
            json!({"nets": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}}, "labels": {"type": "array", "items": {"type": "string"}, "description": "net label per entry of nets (\"\" for none)"},
                   "flip_passives": {"type": "boolean", "description": "turn unwired R/C/L by 180° when that shortens wires (default true)"},
                   "improve": {"type": "boolean", "description": "rip-up-and-retry pass to reduce crossings/bends (default true)"}}), &["nets"]),
        tool("add_wire", "Draw a wire through explicit points [[x,y],...] (diagonal steps are split horizontal-first).", json!({"points": {"type": "array", "items": {"type": "array", "items": {"type": "integer"}}}}), &["points"]),
        tool("delete_wires", "Delete wires by index, by net name, or all.", json!({"indices": {"type": "array", "items": {"type": "integer"}}, "nets": {"type": "array", "items": {"type": "string"}}, "all": {"type": "boolean"}}), &[]),
        tool("add_net_label", "Name a net (label placed automatically in free space). Same names connect nets; results are v(<label>).", json!({"target": {"type": "string", "description": "pin, [x,y] or net:name"}, "label": {"type": "string"}}), &["target", "label"]),
        tool("reroute", "Rip up and re-route the given nets (by name) or all nets. Use after rearranging parts or when wiring looks messy.", json!({"nets": {"type": "array", "items": {"type": "string"}}}), &[]),
        tool("tidy", "Clean up: snap to grid, merge/remove redundant wires, re-place labels, optionally re-route everything.", json!({"reroute": {"type": "boolean"}, "relabel": {"type": "boolean"}}), &[]),
        tool("check_layout", "Find problems: unconnected pins, pins/wire ends on the middle of wires (look connected but are not), overlapping parts or wires, wires through symbols, overlapping text, off-grid items. Also returns wiring metrics.", json!({}), &[]),
        tool("snapshot", "Render the schematic exactly as the editor draws it (PNG). Overlays help reasoning: coordinate grid+rulers, net colours and names, pin names, numbered issue markers.",
            json!({
                "region": {"type": "array", "items": {"type": "number"}, "description": "[x0,y0,x1,y1] in schematic units"},
                "around": {"type": "array", "items": {"type": "string"}, "description": "zoom to these components"},
                "grid": {"type": "boolean"}, "rulers": {"type": "boolean"}, "nets": {"type": "boolean"}, "pins": {"type": "boolean"}, "issues": {"type": "boolean"},
                "highlight": {"type": "array", "items": {"type": "string"}, "description": "component names or net:name to highlight"},
                "scale": {"type": "number", "description": "pixels per unit (default: fit into max_size)"},
                "max_size": {"type": "integer", "description": "default 1400"},
                "clean": {"type": "boolean", "description": "report style: no grid, rulers or overlays"}
            }), &[]),
        tool("add_simulation", "Add a simulation block: kind tr|ac|dc|sweep|noise|fourier|... with properties, e.g. {\"kind\":\"tr\",\"props\":{\"Stop\":\"3 ms\",\"Points\":\"601\"}} or ac {\"Type\":\"log\",\"Start\":\"10 Hz\",\"Stop\":\"10 MHz\",\"Points\":\"301\"}.",
            json!({"kind": {"type": "string"}, "props": {"type": "object"}, "x": {"type": "integer"}, "y": {"type": "integer"}}), &["kind"]),
        tool("add_parameter_sweep", "Sweep a component value (param = component name, e.g. \"R1\", \"C2\", \"V1\") or a variable (created in a .PARAM block if missing; use {name} in component values) over an existing analysis (sim = \"TR1\", \"AC1\", \"DC1\"...). Results come back as one analysis per step (plot_results overlays them).",
            json!({"sim": {"type": "string"}, "param": {"type": "string"}, "start": {"type": "string"}, "stop": {"type": "string"}, "points": {"type": "string"}, "type": {"type": "string", "description": "lin (default) | log | list"}, "default": {"type": "string", "description": "value for a newly created variable"}}), &["sim", "param", "start", "stop"]),
        tool("run_monte_carlo", "Monte Carlo / tolerance analysis: runs the existing simulation N times (in parallel) with R/C/L values randomly varied. Returns statistics of the vectors at the given x points (mean, std, min/max, 1/50/99 percentiles; complex values in dB), the yield against limits, an overlay of all runs and a histogram.",
            json!({"runs": {"type": "integer", "description": "default 50 (run 0 = nominal)"},
                   "tolerances": {"type": "object", "description": "percent: by element letter {\"R\":1,\"C\":5,\"L\":10} and/or per component {\"C3\":20, \"R1\":0}"},
                   "distribution": {"type": "string", "description": "gauss (tolerance = 3 sigma, default) | uniform"}, "seed": {"type": "integer"},
                   "analysis": {"type": "string"}, "vectors": {"type": "array", "items": {"type": "string"}},
                   "at": {"type": "array", "items": {"type": "number"}, "description": "x values to measure at (frequency / time)"},
                   "limits": {"type": "array", "items": {"type": "object", "properties": {"vector": {"type": "string"}, "at": {"type": "number"}, "min": {"type": "number"}, "max": {"type": "number"}}}, "description": "pass/fail limits for the yield (dB for complex vectors)"}}), &[]),
        tool("get_netlist", "The ngspice netlist generated by Qucs-S for the current schematic.", json!({}), &[]),
        tool("run_simulation", "Run ngspice (netlist by Qucs-S). Returns a summary of every analysis.", json!({"timeout_s": {"type": "integer"}}), &[]),
        tool("get_results", "Values of result vectors: summary, values at given x positions, or a decimated table.", json!({"analysis": {"type": "string"}, "vectors": {"type": "array", "items": {"type": "string"}}, "at": {"type": "array", "items": {"type": "number"}}, "points": {"type": "integer", "description": "table rows (decimated)"}}), &[]),
        tool("plot_results", "Result diagram like Qucs-S: kind rect (default; AC shows dB + phase), polar, smith (reflection coefficients) or table. markers = x values (time / frequency) to read off: the values are returned as numbers too. Sweeps overlay all steps.",
            json!({"analysis": {"type": "string"}, "vectors": {"type": "array", "items": {"type": "string"}}, "kind": {"type": "string", "description": "rect | polar | smith | table"},
                   "markers": {"type": "array", "items": {"type": "number"}}, "db": {"type": "boolean"}, "phase": {"type": "boolean"}, "x_log": {"type": "boolean"}, "y_log": {"type": "boolean"},
                   "x_range": {"type": "array", "items": {"type": "number"}}, "y_range": {"type": "array", "items": {"type": "number"}}, "z0": {"type": "number"}, "title": {"type": "string"}}), &[]),
        tool("design_filter", "LC ladder filter synthesis (like qucs-sfilter) placed into the schematic and wired. response butterworth|chebyshev|bessel, class lowpass|highpass|bandpass|bandstop, order, f1 (corner / lower edge), f2 (upper edge), z0. test_bench=true adds source, load and an AC sweep.",
            json!({"response": {"type": "string"}, "ripple_db": {"type": "number"}, "class": {"type": "string"}, "order": {"type": "integer"}, "f1": {"type": "string", "description": "e.g. \"10 MHz\""}, "f2": {"type": "string"}, "z0": {"type": "number"},
                   "first_series": {"type": "boolean", "description": "T start (series element first) instead of PI"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "place": {"type": "boolean", "description": "default true; false = only compute values"}, "test_bench": {"type": "boolean"}}), &["f1"]),
        tool("design_attenuator", "Resistive attenuator (like qucs-sattenuator): topology pi|tee|bridged_tee, atten_db, z_in, z_out. Placed and wired unless place=false.",
            json!({"topology": {"type": "string"}, "atten_db": {"type": "number"}, "z_in": {"type": "number"}, "z_out": {"type": "number"}, "x": {"type": "integer"}, "y": {"type": "integer"}, "place": {"type": "boolean"}, "test_bench": {"type": "boolean"}}), &["atten_db"]),
        tool("calc_transmission_line", "Transmission line calculator (like qucs-strans). microstrip: er, h_mm, t_um, and w_mm (analyse) or z0 (synthesise width); freq for dispersion and lengths (electrical_deg -> length_mm, length_mm -> degrees). coax: er, d_inner_mm, d_outer_mm.",
            json!({"kind": {"type": "string"}, "er": {"type": "number"}, "h_mm": {"type": "number"}, "t_um": {"type": "number"}, "w_mm": {"type": "number"}, "z0": {"type": "number"}, "freq": {"type": "string"},
                   "length_mm": {"type": "number"}, "electrical_deg": {"type": "number"}, "d_inner_mm": {"type": "number"}, "d_outer_mm": {"type": "number"}}), &[]),
        tool("open_gui", "Open the live editor window for the current schematic (it follows every change made here).", json!({}), &[]),
        tool("get_gui_state", "What the user has selected / is looking at in the editor window.", json!({}), &[]),
    ]
}

fn text(v: &Value) -> Value {
    json!({"type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default()})
}

fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(|v| v.as_str())
}
fn i(a: &Value, k: &str) -> Option<i32> {
    a.get(k).and_then(|v| v.as_f64()).map(|f| f.round() as i32)
}
fn b(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(|v| v.as_bool())
}
fn strs(a: &Value, k: &str) -> Vec<String> {
    a.get(k).and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default()
}

pub fn workdir() -> PathBuf {
    std::env::var_os("QUCS_MCP_WORKDIR").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join("qucs_work"))
}

pub fn gui_state_path() -> PathBuf {
    std::env::temp_dir().join("qucs-s-rs").join("gui_state.json")
}

pub fn gui_heartbeat_path() -> PathBuf {
    std::env::temp_dir().join("qucs-s-rs").join("gui_alive")
}

pub fn gui_open_request_path() -> PathBuf {
    std::env::temp_dir().join("qucs-s-rs").join("open_request.txt")
}

/// True when an editor window updated its heartbeat within the last 3 seconds.
fn gui_running() -> bool {
    std::fs::metadata(gui_heartbeat_path()).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs_f64() < 3.0).unwrap_or(false)
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Server {
    pub fn new() -> Server {
        Server { doc: None, loaded_mtime: None, sim: None, snap_counter: 0 }
    }

    fn doc(&mut self) -> Result<&mut Doc, String> {
        // pick up edits saved from the GUI
        if let Some(d) = &self.doc {
            if let Some(p) = d.path.clone() {
                let m = mtime(&p);
                if m.is_some() && m != self.loaded_mtime {
                    if let Ok(nd) = Doc::open(&p) {
                        self.doc = Some(nd);
                        self.loaded_mtime = m;
                    }
                }
            }
        }
        self.doc.as_mut().ok_or_else(|| "no schematic open (new_schematic / open_schematic)".to_string())
    }

    fn commit(&mut self) -> Result<(), String> {
        let d = self.doc.as_mut().ok_or("no schematic")?;
        if d.path.is_some() {
            let p = d.save()?;
            self.loaded_mtime = mtime(&p);
        }
        Ok(())
    }

    fn resolve_path(p: &str) -> PathBuf {
        let mut pb = PathBuf::from(p);
        if pb.extension().is_none() {
            pb.set_extension("sch");
        }
        if pb.is_relative() {
            pb = workdir().join(pb);
        }
        pb
    }

    pub fn call(&mut self, name: &str, a: &Value) -> Result<Vec<Value>, String> {
        match name {
            "new_schematic" => {
                let p = Self::resolve_path(s(a, "path").ok_or("path required")?);
                if p.exists() && !b(a, "overwrite").unwrap_or(false) {
                    return Err(format!("{} exists (pass overwrite=true or use open_schematic)", p.display()));
                }
                let stem = p.file_stem().unwrap().to_string_lossy().to_string();
                self.doc = Some(Doc::new(Schematic::new_empty(&stem), Some(p.clone())));
                self.sim = None;
                self.commit()?;
                Ok(vec![text(&json!({"path": p.display().to_string()}))])
            }
            "open_schematic" => {
                let p = Self::resolve_path(s(a, "path").ok_or("path required")?);
                let d = Doc::open(&p)?;
                self.loaded_mtime = mtime(&p);
                self.doc = Some(d);
                self.sim = None;
                let info = self.schematic_json(false)?;
                Ok(vec![text(&info)])
            }
            "save_schematic" => {
                let d = self.doc()?;
                if let Some(p) = s(a, "path") {
                    d.path = Some(Self::resolve_path(p));
                }
                self.commit()?;
                let p = self.doc()?.path.clone().unwrap();
                Ok(vec![text(&json!({"saved": p.display().to_string()}))])
            }
            "get_schematic" => Ok(vec![text(&self.schematic_json(b(a, "include_wires").unwrap_or(true))?)]),
            "list_component_types" => {
                let q = s(a, "query").unwrap_or("").to_ascii_lowercase();
                let cat = s(a, "category").unwrap_or("").to_ascii_lowercase();
                let mut out = Vec::new();
                let mut cats = std::collections::BTreeMap::<String, usize>::new();
                for c in &lib().comps {
                    *cats.entry(c.category.clone()).or_default() += 1;
                    let hay = format!("{} {} {} {}", c.typ, c.caption, c.description, c.category).to_ascii_lowercase();
                    if (!q.is_empty() && !q.split_whitespace().all(|w| hay.contains(w))) || (!cat.is_empty() && !c.category.to_ascii_lowercase().contains(&cat)) {
                        continue;
                    }
                    out.push(json!({"type": c.typ, "caption": c.caption, "category": c.category, "pins": c.pin_names, "ngspice": c.ngspice}));
                }
                let show_cats = q.is_empty() && cat.is_empty();
                if show_cats {
                    out.truncate(60);
                }
                Ok(vec![text(&json!({"count": out.len(), "types": out, "categories": if show_cats { json!(cats) } else { Value::Null }}))])
            }
            "search_library" => {
                let q = s(a, "query").unwrap_or("");
                let lim = i(a, "limit").unwrap_or(40).clamp(1, 300) as usize;
                let only = s(a, "lib");
                let mut hits = crate::qlib::search(q, if only.is_some() { 100_000 } else { lim });
                if let Some(l) = only {
                    hits.retain(|h| h.0.name.eq_ignore_ascii_case(l));
                    hits.truncate(lim);
                }
                let out: Vec<Value> = hits
                    .iter()
                    .map(|(l, c)| {
                        let tmp = crate::qlib::new_lib_component(&l.name, &c.name, "X", 0, 0).ok();
                        let pins = tmp.map(|t| library::place(&t, None).pins.iter().map(|p| json!({"pin": p.name, "x": p.x, "y": p.y, "dir": [p.dir.0, p.dir.1]})).collect::<Vec<_>>()).unwrap_or_default();
                        json!({"lib": l.name, "comp": c.name, "description": c.description, "ngspice": l.ngspice_ok && (c.has_spice || c.has_qucs_model),
                               "pins_at_origin": pins,
                               "params": c.symbol.params.iter().map(|p| json!({"name": p.name, "default": p.default, "description": p.description})).collect::<Vec<_>>()})
                    })
                    .collect();
                let libs: Vec<String> = if q.is_empty() && only.is_none() { crate::qlib::libraries().iter().map(|l| format!("{} ({})", l.name, l.comps.len())).collect() } else { vec![] };
                Ok(vec![text(&json!({"count": out.len(), "parts": out, "libraries": libs, "library_dirs": crate::qlib::library_dirs().iter().map(|d| d.display().to_string()).collect::<Vec<_>>()}))])
            }
            "get_component_type" => {
                let t = s(a, "type").ok_or("type required")?;
                let info = lib().info(t).ok_or_else(|| format!("unknown type '{t}'"))?;
                let c = library::new_component(t, "X1", 0, 0)?;
                let p = library::place(&c, None);
                Ok(vec![text(&json!({
                    "type": info.typ, "caption": info.caption, "description": info.description, "category": info.category,
                    "props": info.props.iter().map(|p| json!({"name": p.name, "default": p.default, "shown": p.display, "description": p.description})).collect::<Vec<_>>(),
                    "default_rotate": c.rotate,
                    "pins_at_origin": p.pins.iter().map(|q| json!({"name": q.name, "x": q.x, "y": q.y, "dir": [q.dir.0, q.dir.1]})).collect::<Vec<_>>(),
                    "body": p.bbox, "ngspice": info.ngspice,
                }))])
            }
            "add_components" => {
                let items = a.get("components").and_then(|v| v.as_array()).cloned().ok_or("components required")?;
                let mut added = Vec::new();
                let d = self.doc()?;
                d.checkpoint();
                for it in &items {
                    let typ = s(it, "type").ok_or("each component needs a type")?;
                    let mut props = it.get("props").and_then(|p| p.as_object()).cloned().unwrap_or_default();
                    if typ == "Lib" {
                        for (k, key) in [("lib", "Lib"), ("comp", "Comp")] {
                            if let Some(v) = s(it, k) {
                                props.insert(key.into(), json!(v));
                            }
                        }
                    }
                    let (mut x, mut y) = (i(it, "x").unwrap_or(0), i(it, "y").unwrap_or(0));
                    let name = d.add_component(typ, s(it, "name"), x, y, i(it, "rotate"), b(it, "mirror"), &props)?;
                    if let Some(al) = it.get("align") {
                        let conn = d.conn();
                        let pin = s(al, "pin").ok_or("align.pin required")?;
                        let to = s(al, "to").ok_or("align.to required")?;
                        let own = conn.find_pin(&format!("{name}.{pin}")).ok_or_else(|| format!("{name} has no pin '{pin}'"))?.clone();
                        let tgt = d.resolve_terminal(&conn, to)?;
                        x += tgt.x + i(al, "dx").unwrap_or(0) - own.x;
                        y += tgt.y + i(al, "dy").unwrap_or(0) - own.y;
                        let c = d.sch.component_mut(&name).unwrap();
                        c.x = crate::doc::snap(x);
                        c.y = crate::doc::snap(y);
                    }
                    added.push(name);
                }
                let conn = self.doc()?.conn();
                let d = self.doc()?;
                let info: Vec<Value> = added
                    .iter()
                    .map(|n| {
                        let ci = d.sch.component_index(n).unwrap();
                        let c = &d.sch.components[ci];
                        json!({"name": n, "type": c.typ, "x": c.x, "y": c.y, "rotate": c.rotate, "body": conn.placed[ci].bbox.map(|v| v.round()),
                               "pins": conn.pins.iter().filter(|p| p.comp == ci).map(|p| json!({"pin": p.label, "x": p.x, "y": p.y, "dir": [p.dir.0, p.dir.1]})).collect::<Vec<_>>()})
                    })
                    .collect();
                let overlaps: Vec<String> = check::check(&d.sch, &conn).issues.iter().filter(|i| i.kind == "component_overlap").map(|i| i.message.clone()).collect();
                self.commit()?;
                Ok(vec![text(&json!({"added": info, "overlaps": overlaps}))])
            }
            "update_component" => {
                let n = s(a, "name").ok_or("name required")?.to_string();
                let d = self.doc()?;
                d.checkpoint();
                let ci = d.sch.component_index(&n).ok_or_else(|| format!("no component '{n}'"))?;
                if let Some(p) = a.get("props").and_then(|v| v.as_object()) {
                    apply_props(&mut d.sch.components[ci], p)?;
                }
                let c = &mut d.sch.components[ci];
                if let Some(r) = i(a, "rotate") {
                    c.rotate = r.rem_euclid(4);
                }
                if let Some(m) = b(a, "mirror") {
                    c.mirror = m as i32;
                }
                if b(a, "mirror_y").unwrap_or(false) {
                    crate::edit::mirror_y(c);
                }
                if let Some(x) = i(a, "x") {
                    c.x = crate::doc::snap(x);
                }
                if let Some(y) = i(a, "y") {
                    c.y = crate::doc::snap(y);
                }
                if let Some(act) = i(a, "active") {
                    c.active = (c.active & !3) | (act & 3);
                }
                if let Some(nn) = s(a, "new_name") {
                    c.name = nn.to_string();
                }
                if let Some(lo) = a.get("label_offset").and_then(|v| v.as_array()) {
                    if lo.len() == 2 {
                        c.tx = lo[0].as_i64().unwrap_or(0) as i32;
                        c.ty = lo[1].as_i64().unwrap_or(0) as i32;
                    }
                } else if a.get("rotate").is_some() || a.get("mirror").is_some() {
                    d.default_label_pos(ci);
                }
                let conn = d.conn();
                let pins: Vec<Value> = conn.pins.iter().filter(|p| p.comp == ci).map(|p| json!({"pin": p.label, "x": p.x, "y": p.y, "net": conn.nets[p.net].name})).collect();
                let c = d.sch.components[ci].serialize();
                self.commit()?;
                Ok(vec![text(&json!({"component": c, "pins": pins}))])
            }
            "move_components" => {
                let moves = a.get("moves").and_then(|v| v.as_array()).cloned().ok_or("moves required")?;
                let d = self.doc()?;
                d.checkpoint();
                let mut mv = Vec::new();
                for m in &moves {
                    let n = s(m, "name").ok_or("move needs name")?;
                    let c = d.sch.component(n).ok_or_else(|| format!("no component '{n}'"))?;
                    let x = i(m, "x").unwrap_or(c.x) + i(m, "dx").unwrap_or(0);
                    let y = i(m, "y").unwrap_or(c.y) + i(m, "dy").unwrap_or(0);
                    mv.push((n.to_string(), x, y));
                }
                let r = d.move_components(&mv, b(a, "reroute").unwrap_or(true))?;
                self.commit()?;
                Ok(vec![text(&r)])
            }
            "duplicate_components" => {
                let names = strs(a, "names");
                let d = self.doc()?;
                let idx: Result<Vec<usize>, String> = names.iter().map(|n| d.sch.component_index(n).ok_or_else(|| format!("no component '{n}'"))).collect();
                let idx = idx?;
                d.checkpoint();
                let (c, w, _) = crate::edit::duplicate(d, &idx, &[], &[], i(a, "dx").unwrap_or(0), i(a, "dy").unwrap_or(120))?;
                let new: Vec<String> = c.iter().map(|&k| d.sch.components[k].name.clone()).collect();
                let overlaps: Vec<String> = check::check(&d.sch, &d.conn()).issues.iter().filter(|i| i.kind == "component_overlap").map(|i| i.message.clone()).collect();
                self.commit()?;
                Ok(vec![text(&json!({"new_components": new, "wires_copied": w.len(), "overlaps": overlaps}))])
            }
            "align_components" => {
                let names = strs(a, "names");
                let mode = s(a, "mode").ok_or("mode required")?.to_string();
                let d = self.doc()?;
                let idx: Result<Vec<usize>, String> = names.iter().map(|n| d.sch.component_index(n).ok_or_else(|| format!("no component '{n}'"))).collect();
                let moves = crate::edit::align(d, &idx?, &mode)?;
                d.checkpoint();
                let r = d.move_components(&moves, true)?;
                self.commit()?;
                Ok(vec![text(&json!({"moves": moves.iter().map(|m| json!({"name": m.0, "x": m.1, "y": m.2})).collect::<Vec<_>>(), "rerouted": r}))])
            }
            "add_painting" => {
                let line = crate::edit::make_painting(
                    s(a, "kind").ok_or("kind required")?,
                    i(a, "x").unwrap_or(0),
                    i(a, "y").unwrap_or(0),
                    i(a, "w").unwrap_or(0),
                    i(a, "h").unwrap_or(0),
                    s(a, "color").unwrap_or(""),
                    i(a, "width").unwrap_or(if s(a, "kind") == Some("text") { 1 } else { 2 }),
                    s(a, "text").unwrap_or(""),
                    i(a, "size").unwrap_or(12),
                    b(a, "filled").unwrap_or(false),
                )?;
                let d = self.doc()?;
                d.checkpoint();
                d.sch.paintings.push(format!("  {line}"));
                let k = d.sch.paintings.len() - 1;
                self.commit()?;
                Ok(vec![text(&json!({"index": k, "painting": line}))])
            }
            "delete_paintings" => {
                let d = self.doc()?;
                d.checkpoint();
                let n0 = d.sch.paintings.len();
                if b(a, "all").unwrap_or(false) {
                    d.sch.paintings.clear();
                } else {
                    let del: std::collections::HashSet<usize> = a.get("indices").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_u64().map(|u| u as usize)).collect()).unwrap_or_default();
                    let mut k = 0;
                    d.sch.paintings.retain(|_| {
                        let keep = !del.contains(&k);
                        k += 1;
                        keep
                    });
                }
                let removed = n0 - d.sch.paintings.len();
                self.commit()?;
                Ok(vec![text(&json!({"deleted": removed}))])
            }
            "export_image" => {
                let mut path = PathBuf::from(s(a, "path").ok_or("path required")?);
                if path.is_relative() {
                    path = workdir().join(path);
                }
                let d = self.doc()?;
                let conn = d.conn();
                let sc = build_scene(&d.sch, &conn, &Overlay::default());
                let view = fit_view(sc.bounds, 30.0, Some(a.get("scale").and_then(|v| v.as_f64()).unwrap_or(2.0)), 8000.0);
                let (svg, _) = scene_svg(&sc, &view, &Decor::default());
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).ok();
                }
                if path.extension().map(|e| e.eq_ignore_ascii_case("svg")).unwrap_or(false) {
                    std::fs::write(&path, svg).map_err(|e| e.to_string())?;
                } else {
                    std::fs::write(&path, svg_to_png(&svg)?).map_err(|e| e.to_string())?;
                }
                Ok(vec![text(&json!({"saved": path.display().to_string(), "size_px": [view.width_px(), view.height_px()]}))])
            }
            "delete_components" => {
                let names = strs(a, "names");
                let d = self.doc()?;
                d.checkpoint();
                let removed = d.delete_components(&names)?;
                let mut dropped = 0;
                if b(a, "delete_dangling_wires").unwrap_or(true) {
                    let conn = d.conn();
                    let dead: std::collections::HashSet<usize> = conn.islands.iter().filter(|isl| isl.pins.is_empty() && isl.labels.is_empty()).flat_map(|isl| isl.wires.clone()).collect();
                    dropped = dead.len();
                    let mut k = 0;
                    d.sch.wires.retain(|_| {
                        let keep = !dead.contains(&k);
                        k += 1;
                        keep
                    });
                }
                self.commit()?;
                Ok(vec![text(&json!({"deleted": removed, "wires_removed": dropped}))])
            }
            "connect" => {
                let nets: Vec<Vec<String>> = a
                    .get("nets")
                    .and_then(|v| v.as_array())
                    .ok_or("nets required")?
                    .iter()
                    .map(|n| n.as_array().map(|x| x.iter().filter_map(|t| t.as_str().map(|s| s.to_string())).collect()).unwrap_or_default())
                    .collect();
                let labels = strs(a, "labels");
                let d = self.doc()?;
                d.checkpoint();
                let flipped = if b(a, "flip_passives").unwrap_or(true) { d.optimize_flips(&nets) } else { vec![] };
                // short nets first: they have the fewest alternatives
                let conn = d.conn();
                let mut order: Vec<usize> = (0..nets.len()).collect();
                let hp = |n: &Vec<String>| -> i32 {
                    let pts: Vec<(i32, i32)> = n.iter().filter_map(|t| d.resolve_terminal(&conn, t).ok()).map(|t| (t.x, t.y)).collect();
                    if pts.is_empty() {
                        return 0;
                    }
                    let (x0, x1) = (pts.iter().map(|p| p.0).min().unwrap(), pts.iter().map(|p| p.0).max().unwrap());
                    let (y0, y1) = (pts.iter().map(|p| p.1).min().unwrap(), pts.iter().map(|p| p.1).max().unwrap());
                    x1 - x0 + y1 - y0
                };
                order.sort_by_key(|&k| hp(&nets[k]));
                let mut out = vec![Value::Null; nets.len()];
                for k in order {
                    let l = labels.get(k).filter(|l| !l.is_empty()).map(|s| s.as_str());
                    out[k] = d.connect(&nets[k], l)?;
                }
                let gain = if b(a, "improve").unwrap_or(true) && nets.len() > 1 { d.improve_routes(2) } else { 0 };
                let relabeled = d.fix_label_collisions();
                let conn = d.conn();
                let rep = check::check(&d.sch, &conn);
                self.commit()?;
                Ok(vec![text(&json!({"results": out, "flipped_for_shorter_wires": flipped, "improvement": gain, "labels_moved": relabeled, "metrics": rep.metrics}))])
            }
            "add_wire" => {
                let pts: Vec<(i32, i32)> = a
                    .get("points")
                    .and_then(|v| v.as_array())
                    .ok_or("points required")?
                    .iter()
                    .filter_map(|p| {
                        let p = p.as_array()?;
                        Some((p.first()?.as_f64()? as i32, p.get(1)?.as_f64()? as i32))
                    })
                    .collect();
                let d = self.doc()?;
                d.checkpoint();
                let n = d.add_wire_path(&pts);
                self.commit()?;
                Ok(vec![text(&json!({"wires_added": n}))])
            }
            "delete_wires" => {
                let d = self.doc()?;
                d.checkpoint();
                let conn = d.conn();
                let mut del: std::collections::HashSet<usize> = a.get("indices").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_u64().map(|u| u as usize)).collect()).unwrap_or_default();
                for n in strs(a, "nets") {
                    let ni = conn.net_by_name(&n).ok_or_else(|| format!("no net '{n}'"))?;
                    del.extend(conn.nets[ni].wires.iter().copied());
                }
                if b(a, "all").unwrap_or(false) {
                    del.extend(0..d.sch.wires.len());
                }
                let mut k = 0;
                d.sch.wires.retain(|_| {
                    let keep = !del.contains(&k);
                    k += 1;
                    keep
                });
                self.commit()?;
                Ok(vec![text(&json!({"deleted": del.len()}))])
            }
            "add_net_label" => {
                let d = self.doc()?;
                d.checkpoint();
                let r = d.add_net_label(s(a, "target").ok_or("target required")?, s(a, "label").ok_or("label required")?)?;
                self.commit()?;
                Ok(vec![text(&r)])
            }
            "reroute" => {
                let d = self.doc()?;
                d.checkpoint();
                let r = d.reroute(&strs(a, "nets"))?;
                let rep = check::check(&d.sch, &d.conn());
                self.commit()?;
                Ok(vec![text(&json!({"result": r, "metrics": rep.metrics}))])
            }
            "tidy" => {
                let d = self.doc()?;
                d.checkpoint();
                let r = d.tidy(b(a, "relabel").unwrap_or(true), b(a, "reroute").unwrap_or(false))?;
                let rep = check::check(&d.sch, &d.conn());
                self.commit()?;
                Ok(vec![text(&json!({"result": r, "metrics": rep.metrics}))])
            }
            "check_layout" => {
                let d = self.doc()?;
                let conn = d.conn();
                let rep = check::check(&d.sch, &conn);
                Ok(vec![text(&json!({"metrics": rep.metrics, "issues": rep.issues.iter().enumerate().map(|(k, i)| i.json(k + 1)).collect::<Vec<_>>()}))])
            }
            "snapshot" => self.snapshot(a),
            "add_simulation" => {
                let kind = s(a, "kind").ok_or("kind required")?;
                let props = a.get("props").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let d = self.doc()?;
                d.checkpoint();
                let n = d.add_simulation(kind, &props, i(a, "x"), i(a, "y"))?;
                let c = d.sch.component(&n).unwrap().serialize();
                self.commit()?;
                Ok(vec![text(&json!({"name": n, "component": c}))])
            }
            "add_parameter_sweep" => {
                let d = self.doc()?;
                d.checkpoint();
                let n = d.add_parameter_sweep(
                    s(a, "sim").ok_or("sim required")?,
                    s(a, "param").ok_or("param required")?,
                    s(a, "start").ok_or("start required")?,
                    s(a, "stop").ok_or("stop required")?,
                    s(a, "points").unwrap_or("5"),
                    s(a, "type").unwrap_or("lin"),
                    s(a, "default"),
                )?;
                let c = d.sch.component(&n).unwrap().serialize();
                self.commit()?;
                Ok(vec![text(&json!({"name": n, "component": c}))])
            }
            "run_monte_carlo" => {
                self.commit()?;
                let p = self.doc()?.path.clone().ok_or("save the schematic first")?;
                let spec = crate::montecarlo::spec_from_json(a);
                let res = crate::montecarlo::run(&p, &spec)?;
                let mut out = vec![text(&res.summary)];
                if let Ok(svg) = crate::montecarlo::overlay_svg(&res, &spec, 200) {
                    let png = svg_to_png(&svg)?;
                    let path = self.snap_path("mc")?;
                    std::fs::write(&path, &png).map_err(|e| e.to_string())?;
                    out.push(json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&png), "mimeType": "image/png"}));
                }
                let first = res.summary["measurements"].as_array().and_then(|m| m.first()).cloned();
                if let Some(m) = first {
                    let v = m["vector"].as_str().unwrap_or("").to_string();
                    if let Some(svg) = crate::montecarlo::histogram_svg(&res, &spec, &v, m["at"].as_f64()) {
                        let png = svg_to_png(&svg)?;
                        out.push(json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&png), "mimeType": "image/png"}));
                    }
                }
                Ok(out)
            }
            "get_netlist" => {
                self.commit()?;
                let p = self.doc()?.path.clone().ok_or("save the schematic first")?;
                let out = std::env::temp_dir().join("qucs-s-rs").join(format!("{}.cir", p.file_stem().unwrap().to_string_lossy()));
                std::fs::create_dir_all(out.parent().unwrap()).ok();
                let net = sim::netlist(&p, &out)?;
                Ok(vec![json!({"type": "text", "text": net})])
            }
            "run_simulation" => {
                self.commit()?;
                let p = self.doc()?.path.clone().ok_or("save the schematic first")?;
                let r = sim::simulate(&p, i(a, "timeout_s").unwrap_or(300) as u64)?;
                let mut sum = r.summary();
                if !r.ok {
                    sum["log_tail"] = json!(r.log.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
                }
                self.sim = Some(r);
                Ok(vec![text(&sum)])
            }
            "get_results" => {
                let r = self.sim.as_ref().ok_or("no results yet (run_simulation)")?;
                let plots = r.find_all(s(a, "analysis"))?;
                let names = strs(a, "vectors");
                let at: Vec<f64> = a.get("at").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
                let npts = i(a, "points").unwrap_or(0).max(0) as usize;
                let mut out = Vec::new();
                for p in plots {
                    let mut o = p.summary();
                    if !names.is_empty() {
                        let mut m = Map::new();
                        for n in &names {
                            let v = p.vector(n).ok_or_else(|| format!("no vector '{n}' in {}", p.name))?;
                            m.insert(v.name.clone(), sim::describe(v));
                        }
                        o["vectors"] = Value::Object(m);
                    }
                    let sel: Vec<&sim::Vector> = if names.is_empty() { p.vectors.iter().skip(if p.kind == "op" { 0 } else { 1 }).take(12).collect() } else { names.iter().filter_map(|n| p.vector(n)).collect() };
                    if let (Some(sc), false) = (p.scale(), at.is_empty()) {
                        let mut rows = Vec::new();
                        for &x in &at {
                            let mut row = Map::new();
                            row.insert(sc.name.clone(), json!(x));
                            for v in &sel {
                                if v.is_complex() {
                                    let re = sim::interp(&sc.re, &v.re, x);
                                    let im = sim::interp(&sc.re, v.im.as_ref().unwrap(), x);
                                    if let (Some(re), Some(im)) = (re, im) {
                                        let mag = (re * re + im * im).sqrt();
                                        row.insert(v.name.clone(), json!({"re": re, "im": im, "mag": mag, "db": 20.0 * mag.max(1e-300).log10(), "phase_deg": im.atan2(re).to_degrees()}));
                                    }
                                } else {
                                    row.insert(v.name.clone(), json!(sim::interp(&sc.re, &v.re, x)));
                                }
                            }
                            rows.push(Value::Object(row));
                        }
                        o["at"] = json!(rows);
                    }
                    if let (Some(sc), true) = (p.scale(), npts > 0) {
                        let n = sc.re.len();
                        let step = (n as f64 / npts as f64).max(1.0);
                        let mut rows = Vec::new();
                        let mut k = 0.0;
                        while (k as usize) < n {
                            let idx = k as usize;
                            let mut row = vec![json!(sc.re[idx])];
                            for v in &sel {
                                row.push(if v.is_complex() { json!([v.re[idx], v.im.as_ref().unwrap()[idx]]) } else { json!(v.re[idx]) });
                            }
                            rows.push(json!(row));
                            k += step;
                        }
                        o["table"] = json!({"columns": std::iter::once(sc.name.clone()).chain(sel.iter().map(|v| v.name.clone())).collect::<Vec<_>>(), "rows": rows});
                    }
                    out.push(o);
                }
                Ok(vec![text(&json!(out))])
            }
            "plot_results" => {
                let r = self.sim.as_ref().ok_or("no results yet (run_simulation)")?;
                let plots = r.find_all(s(a, "analysis"))?;
                let mut spec = crate::diagram::DiagramSpec::default();
                if let Some(k) = s(a, "kind") {
                    spec.kind = crate::diagram::DiagramSpec::parse_kind(k).ok_or_else(|| format!("unknown kind '{k}' (rect, polar, smith, table)"))?;
                }
                spec.vectors = strs(a, "vectors");
                spec.db = b(a, "db");
                spec.phase = b(a, "phase");
                spec.x_log = b(a, "x_log");
                spec.y_log = b(a, "y_log").unwrap_or(false);
                spec.markers = a.get("markers").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
                let range = |k: &str| a.get(k).and_then(|v| v.as_array()).and_then(|v| Some((v.first()?.as_f64()?, v.get(1)?.as_f64()?)));
                spec.x_range = range("x_range");
                spec.y_range = range("y_range");
                if let Some(z) = a.get("z0").and_then(|v| v.as_f64()) {
                    spec.z0 = z;
                }
                spec.title = s(a, "title").map(|t| t.to_string());
                let (w, h) = if spec.kind == crate::diagram::DiagramKind::Rect { (900.0, 560.0) } else { (720.0, 720.0) };
                let out = crate::diagram::diagram_svg(&plots, &spec, w, h)?;
                let png = svg_to_png(&out.svg)?;
                let path = self.snap_path("plot")?;
                std::fs::write(&path, &png).map_err(|e| e.to_string())?;
                Ok(vec![
                    json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&png), "mimeType": "image/png"}),
                    text(&json!({"saved": path.display().to_string(), "markers": out.markers})),
                ])
            }
            "design_filter" => {
                let spec = crate::synth::filter_spec(a)?;
                let mut d = crate::rftools::lc_filter(&spec)?;
                // analytic check of the synthesized values (ideal terminations)
                let zl = d.values.get("z_load").and_then(|v| v.as_f64()).unwrap_or(spec.z0);
                let mut chk = serde_json::Map::new();
                for (k, f) in [("f1", Some(spec.f1)), ("f2", spec.f2)] {
                    if let Some(f) = f {
                        if let Ok((s11, s21)) = crate::rftools::two_port_s_db(&d, f, spec.z0, zl) {
                            chk.insert(format!("s21_db_at_{k}"), json!((s21 * 100.0).round() / 100.0));
                            chk.insert(format!("s11_db_at_{k}"), json!((s11 * 100.0).round() / 100.0));
                        }
                    }
                }
                d.values["check"] = Value::Object(chk);
                self.place_design_tool(a, d, Some(crate::synth::bench_range(&spec)))
            }
            "design_attenuator" => {
                let (t, db, zi, zo) = crate::synth::attenuator_args(a)?;
                let d = crate::rftools::attenuator(t, db, zi, zo)?;
                self.place_design_tool(a, d, Some((1e3, 1e9)))
            }
            "calc_transmission_line" => Ok(vec![text(&crate::synth::tline(a)?)]),
            "open_gui" => {
                self.commit()?;
                let p = self.doc()?.path.clone().ok_or("save the schematic first")?;
                if gui_running() {
                    // one editor window: open the file in a new tab there
                    use std::io::Write as _;
                    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(gui_open_request_path()).map_err(|e| e.to_string())?;
                    writeln!(f, "{}", p.display()).map_err(|e| e.to_string())?;
                    return Ok(vec![text(&json!({"opened": p.display().to_string(), "note": "opened as a tab in the running editor window; it follows every change made here"}))]);
                }
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("gui").arg(&p).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000 | 0x0000_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
                }
                cmd.spawn().map_err(|e| format!("cannot start GUI: {e}"))?;
                Ok(vec![text(&json!({"opened": p.display().to_string(), "note": "the window reloads automatically whenever this server edits the file"}))])
            }
            "get_gui_state" => {
                let t = std::fs::read_to_string(gui_state_path()).map_err(|_| "the editor window is not running (open_gui)".to_string())?;
                let v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                Ok(vec![text(&v)])
            }
            other => Err(format!("unknown tool '{other}'")),
        }
    }

    fn place_design_tool(&mut self, a: &Value, d: crate::rftools::Design, range: Option<(f64, f64)>) -> Result<Vec<Value>, String> {
        let summary = json!({"title": d.title, "values": d.values, "notes": d.notes,
            "parts": d.parts.iter().map(|p| json!({"type": p.typ, "name": p.name, "props": p.props, "nets": p.nets})).collect::<Vec<_>>()});
        if !b(a, "place").unwrap_or(true) {
            return Ok(vec![text(&summary)]);
        }
        let doc = self.doc()?;
        doc.checkpoint();
        let (x, y) = match (i(a, "x"), i(a, "y")) {
            (Some(x), Some(y)) => (x, y),
            _ => {
                let conn = doc.conn();
                let maxy = conn.placed.iter().map(|p| p.bbox[3]).fold(0.0f64, f64::max);
                (300, (maxy as i32 + 120).max(120))
            }
        };
        let z_load = d.values.get("z_load").and_then(|v| v.as_f64()).or_else(|| a.get("z_out").and_then(|v| v.as_f64())).unwrap_or(50.0);
        let z_src = a.get("z0").and_then(|v| v.as_f64()).or_else(|| a.get("z_in").and_then(|v| v.as_f64())).unwrap_or(50.0);
        let bench = if b(a, "test_bench").unwrap_or(false) { range.map(|(lo, hi)| (z_src, z_load, lo, hi)) } else { None };
        let placed = crate::synth::place_design(doc, &d, x, y, bench)?;
        let rep = check::check(&doc.sch, &doc.conn());
        self.commit()?;
        Ok(vec![text(&json!({"design": summary, "placed": placed, "metrics": rep.metrics}))])
    }

    fn snap_path(&mut self, kind: &str) -> Result<PathBuf, String> {
        let d = self.doc()?;
        let p = d.path.clone().unwrap_or_else(|| workdir().join("untitled.sch"));
        let dir = p.parent().unwrap().join("snapshots");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        self.snap_counter += 1;
        let ts = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Ok(dir.join(format!("{}_{kind}_{}_{:03}.png", p.file_stem().unwrap().to_string_lossy(), ts % 1_000_000, self.snap_counter)))
    }

    fn snapshot(&mut self, a: &Value) -> Result<Vec<Value>, String> {
        let clean = b(a, "clean").unwrap_or(false);
        let d = self.doc()?;
        let conn = d.conn();
        let rep = check::check(&d.sch, &conn);
        let mut ov = Overlay {
            color_nets: b(a, "nets").unwrap_or(!clean),
            net_tags: b(a, "nets").unwrap_or(!clean),
            pin_names: b(a, "pins").unwrap_or(false),
            ..Default::default()
        };
        let show_issues = b(a, "issues").unwrap_or(!clean);
        let shown_issues: Vec<&check::Issue> = if show_issues { rep.issues.iter().filter(|i| i.severity == "error" || i.kind == "unconnected_pin").collect() } else { vec![] };
        ov.markers = shown_issues.iter().map(|i| (i.at.0, i.at.1, i.message.clone())).collect();
        for h in strs(a, "highlight") {
            if let Some(n) = h.strip_prefix("net:") {
                if let Some(ni) = conn.net_by_name(n) {
                    ov.highlight_nets.push(ni);
                }
            } else if let Some(ci) = d.sch.component_index(&h) {
                ov.highlight.push(ci);
            }
        }
        let scene = build_scene(&d.sch, &conn, &ov);
        let bounds = if let Some(r) = a.get("region").and_then(|v| v.as_array()) {
            let v: Vec<f64> = r.iter().filter_map(|x| x.as_f64()).collect();
            if v.len() != 4 {
                return Err("region must be [x0,y0,x1,y1]".into());
            }
            [v[0], v[1], v[2], v[3]]
        } else if let Some(names) = a.get("around").and_then(|v| v.as_array()) {
            let mut bb = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for n in names.iter().filter_map(|x| x.as_str()) {
                let ci = d.sch.component_index(n).ok_or_else(|| format!("no component '{n}'"))?;
                let p = &conn.placed[ci];
                bb = [bb[0].min(p.bbox[0]), bb[1].min(p.bbox[1]), bb[2].max(p.bbox[2]), bb[3].max(p.bbox[3])];
            }
            [bb[0] - 60.0, bb[1] - 60.0, bb[2] + 60.0, bb[3] + 60.0]
        } else {
            scene.bounds
        };
        let max = i(a, "max_size").unwrap_or(1400).clamp(200, 4000) as f64;
        let view = fit_view(bounds, if a.get("region").is_some() { 0.0 } else { 30.0 }, a.get("scale").and_then(|v| v.as_f64()), max);
        let decor = Decor { grid: b(a, "grid").unwrap_or(!clean), rulers: b(a, "rulers").unwrap_or(!clean), title: None };
        let (svg, (ox, oy)) = scene_svg(&scene, &view, &decor);
        let png = svg_to_png(&svg)?;
        let path = self.snap_path("snap")?;
        std::fs::write(&path, &png).map_err(|e| e.to_string())?;
        let meta = json!({
            "saved": path.display().to_string(),
            "size_px": [view.width_px() as f64 + ox, view.height_px() as f64 + oy],
            "coordinate_mapping": {"px": format!("(x - {}) * {:.4} + {}", view.x0, view.scale, ox), "py": format!("(y - {}) * {:.4} + {}", view.y0, view.scale, oy), "region": [view.x0, view.y0, view.x1, view.y1]},
            "metrics": rep.metrics,
            "markers": shown_issues.iter().enumerate().map(|(k, i)| json!({"n": k + 1, "kind": i.kind, "message": i.message, "at": [i.at.0.round(), i.at.1.round()]})).collect::<Vec<_>>(),
            "legend": "wire colours = nets (names in boxes); red circles = open pins; numbered red rings = issues; blue dots = junctions",
        });
        Ok(vec![json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&png), "mimeType": "image/png"}), text(&meta)])
    }

    fn schematic_json(&mut self, wires: bool) -> Result<Value, String> {
        let d = self.doc()?;
        let conn = d.conn();
        let comps: Vec<Value> = d
            .sch
            .components
            .iter()
            .enumerate()
            .map(|(ci, c)| {
                let props: Map<String, Value> = c.props.iter().enumerate().map(|(k, p)| (library::prop_name(c, k) + if library::is_text_block(&c.typ) { format!("#{k}") } else { String::new() }.as_str(), json!(p.value))).collect();
                json!({"name": c.name, "type": c.typ, "x": c.x, "y": c.y, "rotate": c.rotate, "mirror": c.mirror != 0, "active": c.active & 3,
                       "props": props, "body": conn.placed[ci].bbox.map(|v| v.round()),
                       "pins": conn.pins.iter().filter(|p| p.comp == ci).map(|p| json!({"pin": p.label, "x": p.x, "y": p.y, "net": conn.nets[p.net].name})).collect::<Vec<_>>()})
            })
            .collect();
        let nets: Vec<Value> = conn.nets.iter().map(|n| json!({"name": n.name, "pins": n.pins.iter().map(|&p| conn.pins[p].label.clone()).collect::<Vec<_>>(), "wires": n.wires.len()})).collect();
        let mut o = json!({"path": d.path.as_ref().map(|p| p.display().to_string()), "components": comps, "nets": nets,
            "paintings": d.sch.paintings.iter().enumerate().map(|(k, p)| json!({"i": k, "painting": p.trim()})).collect::<Vec<_>>()});
        if wires {
            o["wires"] = json!(d.sch.wires.iter().enumerate().map(|(k, w)| json!({"i": k, "p": [w.x1, w.y1, w.x2, w.y2], "net": conn.nets[conn.wire_net[k]].name, "label": if w.label.is_empty() { Value::Null } else { json!(w.label) }})).collect::<Vec<_>>());
        }
        Ok(o)
    }
}

pub fn serve() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut server = Server::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let r = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}});
                let _ = writeln!(stdout, "{r}");
                let _ = stdout.flush();
                continue;
            }
        };
        let batch = if let Value::Array(a) = msg { a } else { vec![msg] };
        for m in batch {
            if let Some(resp) = handle(&mut server, &m) {
                let _ = writeln!(stdout, "{resp}");
                let _ = stdout.flush();
            }
        }
    }
}

pub fn handle(server: &mut Server, m: &Value) -> Option<Value> {
    let id = m.get("id").cloned();
    let method = m.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = m.get("params").cloned().unwrap_or(json!({}));
    let id = id?; // notifications get no response
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            let ver = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18").to_string();
            Ok(json!({
                "protocolVersion": ver,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "qucs-s-rs", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.call(&name, &args)));
            match r {
                Ok(Ok(content)) => Ok(json!({"content": content, "isError": false})),
                Ok(Err(e)) => Ok(json!({"content": [{"type": "text", "text": format!("Error: {e}")}], "isError": true})),
                Err(_) => Ok(json!({"content": [{"type": "text", "text": "Error: internal panic while running the tool"}], "isError": true})),
            }
        }
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((c, msg)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": c, "message": msg}}),
    })
}
