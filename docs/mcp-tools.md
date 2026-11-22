# MCP tool reference

Generated from `qucs-s-rs mcp` (0.1.0) by `tools/gen_mcp_docs.py` — do not edit by hand.

Coordinates are Qucs units (x right, **y down**, 10-unit grid). Terminals: `"R1.1"`, `"V1.+"`, `"OP1.OUT"`, points `"[x,y]"`, nets `"net:name"`.

## Recommended workflow

Qucs-S (Rust fork) schematic editor. Coordinates are Qucs units: x to the right, y DOWN, 10-unit grid. Pins are addressed as "R1.1", "V1.+", "OP1.OUT"; points as "[x,y]"; nets as "net:name". Workflow: new_schematic -> add_components (signal left->right, supply at top, ground at bottom, leave >= 60 units between parts) -> connect (one call per net; the router avoids symbols, never overlaps other nets and keeps bends low) -> add_net_label for nodes you want to observe -> snapshot (look at the picture! it is exactly what the editor shows) -> check_layout -> fix with move_components / reroute / tidy -> add_simulation -> run_simulation -> get_results / plot_results (dc_operating_point gives node voltages without any simulation block). Large designs: create_subcircuit moves parts into their own file (open_subcircuit descends). Every edit is saved immediately; open_gui shows the live editor to the user.

## Files

### `new_schematic`

Create a new empty schematic file (overwrites only if overwrite=true).

| parameter | type | required | description |
|---|---|---|---|
| `path` | string | yes |  |
| `overwrite` | boolean |  |  |

### `open_schematic`

Open an existing Qucs-S .sch file.

| parameter | type | required | description |
|---|---|---|---|
| `path` | string | yes |  |

### `save_schematic`

Save (optionally to a new path). Edits are saved automatically anyway.

| parameter | type | required | description |
|---|---|---|---|
| `path` | string |  |  |

### `get_schematic`

Components (with absolute pin positions and nets), wires and nets of the open schematic.

| parameter | type | required | description |
|---|---|---|---|
| `include_wires` | boolean |  |  |

### `export_image`

Write the schematic as PNG or SVG (report style, no overlays) to a file.

| parameter | type | required | description |
|---|---|---|---|
| `path` | string | yes |  |
| `scale` | number |  | pixels per unit, default 2 |

## Components

### `list_component_types`

Search the component library (220 Qucs-S types).

| parameter | type | required | description |
|---|---|---|---|
| `query` | string |  |  |
| `category` | string |  |  |

### `get_component_type`

Properties (name/default/description), pins and symbol size of a component type.

| parameter | type | required | description |
|---|---|---|---|
| `type` | string | yes |  |

### `search_library`

Search the Qucs-S component libraries (real parts: op-amps, transistors, diodes, regulators, transformers, ...). Returns lib/comp names, description, pins and parameters; place with add_components {type:"Lib", lib, comp}.

| parameter | type | required | description |
|---|---|---|---|
| `query` | string |  | words to match, e.g. "lm358", "zener 5v", "mosfet irf" |
| `lib` | string |  | restrict to one library |
| `limit` | integer |  |  |

### `add_components`

Place components. Returns their pins (absolute coordinates and lead direction) so you can plan wiring.

| parameter | type | required | description |
|---|---|---|---|
| `components` | object[] | yes |  |

### `update_component`

Change properties, orientation, position or name of a component (wires are NOT moved; use move_components for that).

| parameter | type | required | description |
|---|---|---|---|
| `name` | string | yes |  |
| `props` | object |  |  |
| `rotate` | integer |  |  |
| `mirror` | boolean |  |  |
| `x` | integer |  |  |
| `y` | integer |  |  |
| `new_name` | string |  |  |
| `mirror_y` | boolean |  | mirror about the Y axis (left-right) |
| `active` | integer |  | 1 active, 0 off, 2 shorted |
| `label_offset` | integer[] |  | [tx, ty] text position relative to the component |

### `move_components`

Move components (absolute x/y or relative dx/dy) and re-route the wires attached to them.

| parameter | type | required | description |
|---|---|---|---|
| `moves` | object[] | yes |  |
| `reroute` | boolean |  | default true |

### `duplicate_components`

Copy components (and the wires between them) with an offset; names are renumbered. Returns the new names.

| parameter | type | required | description |
|---|---|---|---|
| `names` | string[] | yes |  |
| `dx` | integer |  |  |
| `dy` | integer |  |  |

### `align_components`

Align or distribute components: mode left|right|top|bottom|hcenter|vcenter|distribute_h|distribute_v. Wires are re-routed.

| parameter | type | required | description |
|---|---|---|---|
| `names` | string[] | yes |  |
| `mode` | string | yes |  |

### `delete_components`

Delete components (and optionally the wires of nets left without pins).

| parameter | type | required | description |
|---|---|---|---|
| `names` | string[] | yes |  |
| `delete_dangling_wires` | boolean |  |  |

## Wiring

### `connect`

Auto-route wires. Each entry of `nets` is a list of terminals that become one net ("R1.2", "[x,y]", "net:name"). Optional labels give the net a name.

| parameter | type | required | description |
|---|---|---|---|
| `nets` | string[][] | yes |  |
| `labels` | string[] |  | net label per entry of nets ("" for none) |
| `flip_passives` | boolean |  | turn unwired R/C/L by 180° when that shortens wires (default true) |
| `improve` | boolean |  | rip-up-and-retry pass to reduce crossings/bends (default true) |

### `add_wire`

Draw a wire through explicit points [[x,y],...] (diagonal steps are split horizontal-first).

| parameter | type | required | description |
|---|---|---|---|
| `points` | integer[][] | yes |  |

### `delete_wires`

Delete wires by index, by net name, or all.

| parameter | type | required | description |
|---|---|---|---|
| `indices` | integer[] |  |  |
| `nets` | string[] |  |  |
| `all` | boolean |  |  |

### `add_net_label`

Name a net (label placed automatically in free space). Same names connect nets; results are v(<label>).

| parameter | type | required | description |
|---|---|---|---|
| `target` | string | yes | pin, [x,y] or net:name |
| `label` | string | yes |  |

### `reroute`

Rip up and re-route the given nets (by name) or all nets. Use after rearranging parts or when wiring looks messy.

| parameter | type | required | description |
|---|---|---|---|
| `nets` | string[] |  |  |

### `tidy`

Clean up: snap to grid, merge/remove redundant wires, re-place labels, optionally re-route everything.

| parameter | type | required | description |
|---|---|---|---|
| `reroute` | boolean |  |  |
| `relabel` | boolean |  |  |

## Drawings

### `add_painting`

Add a drawing: kind line|rect|ellipse|arrow|text. line/arrow: (x,y)->(x+w,y+h); rect/ellipse: box; text: text at (x,y).

| parameter | type | required | description |
|---|---|---|---|
| `kind` | string | yes |  |
| `x` | integer | yes |  |
| `y` | integer | yes |  |
| `w` | integer |  |  |
| `h` | integer |  |  |
| `text` | string |  |  |
| `size` | integer |  |  |
| `color` | string |  | #rrggbb |
| `width` | integer |  |  |
| `filled` | boolean |  |  |

### `delete_paintings`

Delete drawings by index (get_schematic lists them) or all.

| parameter | type | required | description |
|---|---|---|---|
| `indices` | integer[] |  |  |
| `all` | boolean |  |  |

## Inspection

### `snapshot`

Render the schematic exactly as the editor draws it (PNG). Overlays help reasoning: coordinate grid+rulers, net colours and names, pin names, numbered issue markers.

| parameter | type | required | description |
|---|---|---|---|
| `region` | number[] |  | [x0,y0,x1,y1] in schematic units |
| `around` | string[] |  | zoom to these components |
| `grid` | boolean |  |  |
| `rulers` | boolean |  |  |
| `nets` | boolean |  |  |
| `pins` | boolean |  |  |
| `issues` | boolean |  |  |
| `highlight` | string[] |  | component names or net:name to highlight |
| `scale` | number |  | pixels per unit (default: fit into max_size) |
| `max_size` | integer |  | default 1400 |
| `clean` | boolean |  | report style: no grid, rulers or overlays |

### `check_layout`

Find problems: unconnected pins, pins/wire ends on the middle of wires (look connected but are not), overlapping parts or wires, wires through symbols, overlapping text, off-grid items. Also returns wiring metrics.

## Hierarchy

### `create_subcircuit`

Move the given components into a new subcircuit schematic (Qucs Sub with numbered Ports for every net that leaves the selection) and replace them with a Sub component wired to the same nets. Returns the ports (number -> net).

| parameter | type | required | description |
|---|---|---|---|
| `components` | string[] | yes |  |
| `file` | string | yes | subcircuit file, relative to the current schematic, e.g. "amp_stage.sch" |
| `overwrite` | boolean |  |  |

### `open_subcircuit`

Open the schematic behind a Sub component (descend into the hierarchy). open_schematic the parent again to go back up.

| parameter | type | required | description |
|---|---|---|---|
| `name` | string | yes | Sub component name |

## Simulation

### `add_simulation`

Add a simulation block: kind tr|ac|dc|sweep|noise|fourier|... with properties, e.g. {"kind":"tr","props":{"Stop":"3 ms","Points":"601"}} or ac {"Type":"log","Start":"10 Hz","Stop":"10 MHz","Points":"301"}.

| parameter | type | required | description |
|---|---|---|---|
| `kind` | string | yes |  |
| `props` | object |  |  |
| `x` | integer |  |  |
| `y` | integer |  |  |

### `add_parameter_sweep`

Sweep a component value (param = component name, e.g. "R1", "C2", "V1") or a variable (created in a .PARAM block if missing; use {name} in component values) over an existing analysis (sim = "TR1", "AC1", "DC1"...). Results come back as one analysis per step (plot_results overlays them).

| parameter | type | required | description |
|---|---|---|---|
| `sim` | string | yes |  |
| `param` | string | yes |  |
| `start` | string | yes |  |
| `stop` | string | yes |  |
| `points` | string |  |  |
| `type` | string |  | lin (default) \| log \| list |
| `default` | string |  | value for a newly created variable |

### `get_netlist`

The ngspice netlist of the current schematic (built-in netlister; Qucs-S is used for parts it cannot translate). netlister = auto | builtin | qucs-s.

| parameter | type | required | description |
|---|---|---|---|
| `netlister` | string |  |  |

### `run_simulation`

Run ngspice. Returns a summary of every analysis. The netlist comes from the built-in netlister (TR, AC, DC operating point; R/L/C, sources, diodes, BJT, MOSFET, JFET, op-amp, controlled sources, probes, B-sources, subcircuits); other analyses or parts are netlisted by Qucs-S when it is installed.

| parameter | type | required | description |
|---|---|---|---|
| `timeout_s` | integer |  |  |

### `dc_operating_point`

DC operating point: the voltage of every net and the current through every voltage source / probe. Needs no simulation block. The editor can show the voltages on the schematic (button "動作点").

| parameter | type | required | description |
|---|---|---|---|
| `timeout_s` | integer |  |  |

### `get_results`

Values of result vectors: summary, values at given x positions, or a decimated table.

| parameter | type | required | description |
|---|---|---|---|
| `analysis` | string |  |  |
| `vectors` | string[] |  |  |
| `at` | number[] |  |  |
| `points` | integer |  | table rows (decimated) |

### `plot_results`

Result diagram like Qucs-S: kind rect (default; AC shows dB + phase), polar, smith (reflection coefficients) or table. markers = x values (time / frequency) to read off: the values are returned as numbers too. Sweeps overlay all steps.

| parameter | type | required | description |
|---|---|---|---|
| `analysis` | string |  |  |
| `vectors` | string[] |  |  |
| `kind` | string |  | rect \| polar \| smith \| table |
| `markers` | number[] |  |  |
| `db` | boolean |  |  |
| `phase` | boolean |  |  |
| `x_log` | boolean |  |  |
| `y_log` | boolean |  |  |
| `x_range` | number[] |  |  |
| `y_range` | number[] |  |  |
| `z0` | number |  |  |
| `title` | string |  |  |

### `run_monte_carlo`

Monte Carlo / tolerance analysis: runs the existing simulation N times (in parallel) with R/C/L values randomly varied. Returns statistics of the vectors at the given x points (mean, std, min/max, 1/50/99 percentiles; complex values in dB), the yield against limits, an overlay of all runs and a histogram.

| parameter | type | required | description |
|---|---|---|---|
| `runs` | integer |  | default 50 (run 0 = nominal) |
| `tolerances` | object |  | percent: by element letter {"R":1,"C":5,"L":10} and/or per component {"C3":20, "R1":0} |
| `distribution` | string |  | gauss (tolerance = 3 sigma, default) \| uniform |
| `seed` | integer |  |  |
| `analysis` | string |  |  |
| `vectors` | string[] |  |  |
| `at` | number[] |  | x values to measure at (frequency / time) |
| `limits` | object[] |  | pass/fail limits for the yield (dB for complex vectors) |

## Design tools

### `design_filter`

LC ladder filter synthesis (like qucs-sfilter) placed into the schematic and wired. response butterworth|chebyshev|bessel, class lowpass|highpass|bandpass|bandstop, order, f1 (corner / lower edge), f2 (upper edge), z0. test_bench=true adds source, load and an AC sweep.

| parameter | type | required | description |
|---|---|---|---|
| `response` | string |  |  |
| `ripple_db` | number |  |  |
| `class` | string |  |  |
| `order` | integer |  |  |
| `f1` | string | yes | e.g. "10 MHz" |
| `f2` | string |  |  |
| `z0` | number |  |  |
| `first_series` | boolean |  | T start (series element first) instead of PI |
| `x` | integer |  |  |
| `y` | integer |  |  |
| `place` | boolean |  | default true; false = only compute values |
| `test_bench` | boolean |  |  |

### `design_attenuator`

Resistive attenuator (like qucs-sattenuator): topology pi|tee|bridged_tee, atten_db, z_in, z_out. Placed and wired unless place=false.

| parameter | type | required | description |
|---|---|---|---|
| `topology` | string |  |  |
| `atten_db` | number | yes |  |
| `z_in` | number |  |  |
| `z_out` | number |  |  |
| `x` | integer |  |  |
| `y` | integer |  |  |
| `place` | boolean |  |  |
| `test_bench` | boolean |  |  |

### `calc_transmission_line`

Transmission line calculator (like qucs-strans). microstrip: er, h_mm, t_um, and w_mm (analyse) or z0 (synthesise width); freq for dispersion and lengths (electrical_deg -> length_mm, length_mm -> degrees). coax: er, d_inner_mm, d_outer_mm.

| parameter | type | required | description |
|---|---|---|---|
| `kind` | string |  |  |
| `er` | number |  |  |
| `h_mm` | number |  |  |
| `t_um` | number |  |  |
| `w_mm` | number |  |  |
| `z0` | number |  |  |
| `freq` | string |  |  |
| `length_mm` | number |  |  |
| `electrical_deg` | number |  |  |
| `d_inner_mm` | number |  |  |
| `d_outer_mm` | number |  |  |

## Editor

### `open_gui`

Open the live editor window for the current schematic (it follows every change made here).

### `get_gui_state`

What the user has selected / is looking at in the editor window.
