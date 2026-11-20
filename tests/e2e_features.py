"""End-to-end check of library parts, filter synthesis, diagrams/markers, sweeps,
editing tools and export through the MCP server.

    python tests/e2e_features.py [path-to-qucs-s-rs]
"""
import base64
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "debug" / "qucs-s-rs.exe"
OUT = ROOT / "target" / "e2e"
OUT.mkdir(parents=True, exist_ok=True)

p = subprocess.Popen([str(EXE), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     env={**os.environ, "QUCS_MCP_WORKDIR": str(OUT)})
n = 0


def call(tool_name, **a):
    global n
    n += 1
    p.stdin.write((json.dumps({"jsonrpc": "2.0", "id": n, "method": "tools/call",
                               "params": {"name": tool_name, "arguments": a}}) + "\n").encode())
    p.stdin.flush()
    r = json.loads(p.stdout.readline())["result"]
    texts = [c["text"] for c in r["content"] if c["type"] == "text"]
    imgs = [c for c in r["content"] if c["type"] == "image"]
    if r["isError"]:
        raise SystemExit(f"{tool_name}: {texts}")
    data = json.loads(texts[0]) if texts and texts[0][:1] in "{[" else (texts[0] if texts else None)
    return data, imgs


def save(imgs, fname):
    (OUT / fname).write_bytes(base64.b64decode(imgs[0]["data"]))


# ---------------------------------------------------------------- library part
call("new_schematic", path="lib_amp", overwrite=True)
hits, _ = call("search_library", query="lm358")
print("search lm358:", [(h["lib"], h["comp"], h["ngspice"], len(h["pins_at_origin"])) for h in hits["parts"]][:5])
part = next(h for h in hits["parts"] if h["ngspice"])
print("pins:", part["pins_at_origin"])
added, _ = call("add_components", components=[
    {"type": "Lib", "lib": part["lib"], "comp": part["comp"], "name": "U1", "x": 400, "y": 300},
    {"type": "Vac", "name": "VIN", "x": 100, "y": 360, "props": {"U": "0.1 V"}},
    {"type": "Vdc", "name": "VP", "x": 600, "y": 160, "props": {"U": "12 V"}},
    {"type": "Vdc", "name": "VN", "x": 700, "y": 160, "props": {"U": "12 V"}},
    {"type": "R", "name": "R1", "x": 230, "y": 260, "props": {"R": "10k"}},
    {"type": "R", "name": "R2", "x": 400, "y": 160, "props": {"R": "100k"}},
    {"type": "GND", "x": 100, "y": 460}, {"type": "GND", "x": 300, "y": 460},
    {"type": "GND", "x": 600, "y": 260}, {"type": "GND", "x": 700, "y": 260},
])
u1 = next(a for a in added["added"] if a["name"] == "U1")
print("U1 pins:", [(q["pin"], q["x"], q["y"]) for q in u1["pins"]])
r, _ = call("connect", nets=[
    ["VIN.+", "R1.1"], ["R1.2", "U1.2", "R2.1"], ["R2.2", "U1.5"], ["U1.1", "GND2.1"],
    ["U1.VCC", "VP.+"], ["VP.-", "GND3.1"], ["U1.VEE", "VN.-"], ["VN.+", "GND4.1"], ["VIN.-", "GND1.1"]],
    labels=["in", "", "out", "", "", "", "", "", ""])
print("lib amp metrics:", r["metrics"])
call("add_simulation", kind="ac", props={"Type": "log", "Start": "10 Hz", "Stop": "10 MHz", "Points": "201"})
sim, _ = call("run_simulation")
print("lib amp sim ok:", sim["ok"], sim["errors"][:3])
if sim["ok"]:
    m, img = call("plot_results", analysis="ac1", vectors=["v(out)"], markers=[1000, 100000])
    save(img, "lib_amp_ac.png")
    print("gain markers:", [(x["x"], round(x["value"]["db"], 2)) for x in m["markers"]])
_, img = call("snapshot", clean=True)
save(img, "lib_amp.png")

# ---------------------------------------------------------------- filter synthesis
call("new_schematic", path="filter", overwrite=True)
d, _ = call("design_filter", response="chebyshev", ripple_db=0.5, **{"class": "lowpass"}, order=5, f1="10 MHz", z0=50, test_bench=True)
print("filter values:", d["design"]["values"].get("check"), d["metrics"])
sim, _ = call("run_simulation")
print("filter sim ok:", sim["ok"], sim["errors"][:3])
m, img = call("plot_results", analysis="ac1", vectors=["v(out)"], markers=[5e6, 10e6, 20e6])
save(img, "filter_ac.png")
print("filter markers dB:", [(x["x"], round(x["value"]["db"], 2) if isinstance(x["value"], dict) else x["value"]) for x in m["markers"]])
_, img = call("plot_results", analysis="ac1", vectors=["v(out)"], kind="polar")
save(img, "filter_polar.png")
_, img = call("plot_results", analysis="ac1", vectors=["v(out)", "v(in)"], kind="table", markers=[1e6, 10e6, 30e6])
save(img, "filter_table.png")
_, img = call("snapshot", clean=True)
save(img, "filter.png")

# ---------------------------------------------------------------- sweep of the load
sw, _ = call("add_parameter_sweep", sim="AC1", param="RL", start="25", stop="100", points="3")
print("sweep:", sw["component"])
sim, _ = call("run_simulation")
print("sweep sim ok:", sim["ok"], [a["analysis"] for a in sim["analyses"]][:6])
m, img = call("plot_results", analysis="ac1", vectors=["v(out)"], markers=[10e6])
save(img, "filter_sweep.png")
print("sweep markers:", [(x.get("label"), round(x["value"]["db"], 2)) for x in m["markers"]])

# ---------------------------------------------------------------- attenuator + tline
a, _ = call("design_attenuator", topology="pi", atten_db=10, z_in=50, place=False)
print("attenuator:", a["values"])
t, _ = call("calc_transmission_line", er=4.4, h_mm=1.6, t_um=35, z0=50, freq="2.4 GHz", electrical_deg=90)
print("microstrip:", t)

# ---------------------------------------------------------------- editing tools
call("new_schematic", path="edit", overwrite=True)
call("add_components", components=[{"type": "R", "name": "R1", "x": 100, "y": 100}, {"type": "C", "name": "C1", "x": 230, "y": 130}])
call("connect", nets=[["R1.2", "C1.1"]])
dup, _ = call("duplicate_components", names=["R1", "C1"], dy=120)
print("duplicate:", dup)
al, _ = call("align_components", names=["R1", "C1"], mode="hcenter")
print("align moves:", al["moves"])
call("add_painting", kind="text", x=100, y=20, text="Editing test (two lines)", size=14)
call("add_painting", kind="rect", x=40, y=40, w=280, h=260, color="#808080")
call("update_component", name="C2", mirror_y=True)
ex, _ = call("export_image", path="edit_export.svg")
print("export:", ex)
_, img = call("snapshot", clean=True)
save(img, "edit.png")
p.stdin.close()
p.wait(5)
print("OK")
