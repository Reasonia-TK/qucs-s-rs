# Changelog

## Unreleased

### Added
- **Built-in ngspice netlister** (`src/spice.rs`): a port of Qucs-S' translation rules for TR / AC / DC-OP, Nutmeg
  equations, `.PARAM`, passives, sources, diode, BJT, MOSFET, JFET, op-amp, controlled sources, probes, B-sources and
  nested subcircuits. Qucs-S is now optional and only used for what the built-in netlister cannot translate
  (`QUCS_RS_NETLISTER=auto|builtin|qucs-s`). `tests/netlist_parity.py` checks the results against qucs-s.
- **DC operating point**: editor button *⚡ 動作点* (voltage tags on every net + current list), MCP `dc_operating_point`,
  CLI `qucs-s-rs op`.
- **Hierarchy**: *Create subcircuit* from a selection (ports generated for every net that leaves it, the parent is
  re-wired to a `Sub`), open a `Sub` by double-click; MCP `create_subcircuit` / `open_subcircuit`.
- **Save-changes prompt** when closing a tab or the window with unsaved edits.
- **Persistent settings** (view toggles, auto-save, window geometry, panel widths) and a **recent files** menu.
- Find (`Ctrl+F`), keyboard zoom (`Ctrl +/-/0`), `Ctrl+Shift+S`, `F5`, `F1`; PDF export and printing via PDF.
- About window (version, ngspice / Qucs-S paths, settings folder); application icon and Windows version info.
- CLI: `netlist`, `simulate`, `op`, `--version`.
- Per-user installer / uninstaller and release packaging scripts (`tools/install.ps1`, `uninstall.ps1`, `package.ps1`).
- CI on Windows, Linux and macOS (clippy, tests, release build, ngspice simulation with the built-in netlister).
- Unit tests for the parser, connectivity rules, editing / undo, router guarantees, netlister, hierarchy, PDF, settings.
- English README, generated MCP tool reference (`docs/mcp-tools.md`, `tools/gen_mcp_docs.py`).
- `QUCS_RS_SHOT` self-screenshot hook for headless GUI checks.

### Changed
- `get_netlist` takes `netlister`; `get_component_type` reports `builtin_netlist`.
- Simulation work folders no longer carry the `\\?\` prefix on Windows.
- More CJK font locations on Linux / macOS.

### Fixed
- Untitled tabs with edits were closed without warning, even with auto-save on.
- Possible panics on NaN values (sorting), poisoned locks in Monte Carlo, and paths without a parent / file name.
- Float literals that will stop compiling in future Rust releases.

## 0.1.0

First version: Rust editor with Qucs-S symbols, net-aware router, MCP server, simulation via Qucs-S + ngspice,
result diagrams, Qucs-S libraries, RF design tools, Monte Carlo, tabs, light / dark themes.
