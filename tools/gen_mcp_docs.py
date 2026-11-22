"""Generate docs/mcp-tools.md from the server's own tool list.

    python tools/gen_mcp_docs.py [path-to-qucs-s-rs]
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "release" / "qucs-s-rs.exe"

GROUPS = [
    ("Files", ["new_schematic", "open_schematic", "save_schematic", "get_schematic", "export_image"]),
    ("Components", ["list_component_types", "get_component_type", "search_library", "add_components", "update_component",
                    "move_components", "duplicate_components", "align_components", "delete_components"]),
    ("Wiring", ["connect", "add_wire", "delete_wires", "add_net_label", "reroute", "tidy"]),
    ("Drawings", ["add_painting", "delete_paintings"]),
    ("Inspection", ["snapshot", "check_layout"]),
    ("Hierarchy", ["create_subcircuit", "open_subcircuit"]),
    ("Simulation", ["add_simulation", "add_parameter_sweep", "get_netlist", "run_simulation", "dc_operating_point",
                    "get_results", "plot_results", "run_monte_carlo"]),
    ("Design tools", ["design_filter", "design_attenuator", "calc_transmission_line"]),
    ("Editor", ["open_gui", "get_gui_state"]),
]


def rpc(p, n, method, params=None):
    p.stdin.write((json.dumps({"jsonrpc": "2.0", "id": n, "method": method, "params": params or {}}) + "\n").encode())
    p.stdin.flush()
    return json.loads(p.stdout.readline())["result"]


def schema_type(s):
    t = s.get("type", "")
    if t == "array":
        return f"{schema_type(s.get('items', {}))}[]"
    return t or "any"


def main():
    p = subprocess.Popen([str(EXE), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    init = rpc(p, 1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "docs", "version": "0"}})
    tools = {t["name"]: t for t in rpc(p, 2, "tools/list")["tools"]}
    p.terminate()

    out = ["# MCP tool reference", "",
           f"Generated from `qucs-s-rs mcp` ({init['serverInfo'].get('version', '')}) by `tools/gen_mcp_docs.py` — do not edit by hand.", "",
           "Coordinates are Qucs units (x right, **y down**, 10-unit grid). Terminals: `\"R1.1\"`, `\"V1.+\"`, `\"OP1.OUT\"`, "
           "points `\"[x,y]\"`, nets `\"net:name\"`.", ""]
    if init.get("instructions"):
        out += ["## Recommended workflow", "", init["instructions"], ""]
    seen = set()
    for title, names in GROUPS:
        names = [n for n in names if n in tools]
        if not names:
            continue
        out += [f"## {title}", ""]
        for n in names:
            seen.add(n)
            out += doc_tool(tools[n])
    rest = [n for n in tools if n not in seen]
    if rest:
        out += ["## Other", ""]
        for n in rest:
            out += doc_tool(tools[n])
    path = ROOT / "docs" / "mcp-tools.md"
    path.write_text("\n".join(out).rstrip() + "\n", encoding="utf-8")
    print(f"wrote {path} ({len(tools)} tools)")


def doc_tool(t):
    lines = [f"### `{t['name']}`", "", t["description"], ""]
    props = t.get("inputSchema", {}).get("properties", {})
    req = set(t.get("inputSchema", {}).get("required", []))
    if props:
        lines += ["| parameter | type | required | description |", "|---|---|---|---|"]
        for k, v in props.items():
            desc = (v.get("description", "") or "").replace("|", "\\|").replace("\n", " ")
            lines.append(f"| `{k}` | {schema_type(v)} | {'yes' if k in req else ''} | {desc} |")
        lines.append("")
    return lines


if __name__ == "__main__":
    main()
