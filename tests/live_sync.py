"""Live-sync check: start the GUI on a file, edit it through the MCP server, the GUI must follow."""
import json, subprocess, sys, time, os
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "debug" / "qucs-s-rs.exe"
OUT = ROOT / "target" / "e2e"
p = subprocess.Popen([str(EXE), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env={**os.environ, "QUCS_MCP_WORKDIR": str(OUT)})
n = 0
def call(name, **a):
    global n
    n += 1
    p.stdin.write((json.dumps({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": name, "arguments": a}}) + "\n").encode()); p.stdin.flush()
    r = json.loads(p.stdout.readline())["result"]
    t = [c["text"] for c in r["content"] if c["type"] == "text"]
    if r["isError"]: raise SystemExit(t)
    return t[0] if t else None
call("new_schematic", path="live", overwrite=True)
call("open_gui")
time.sleep(float(sys.argv[1]) if len(sys.argv) > 1 else 4)
call("add_components", components=[{"type": "Vdc", "name": "V1", "x": 100, "y": 200, "props": {"U": "5 V"}},
                                   {"type": "R", "name": "R1", "x": 220, "y": 140},
                                   {"type": "R", "name": "R2", "x": 320, "y": 200, "rotate": 1},
                                   {"type": "LED", "name": "D1", "x": 320, "y": 300} if False else {"type": "Diode", "name": "D1", "x": 420, "y": 200, "rotate": 1},
                                   {"type": "GND", "x": 100, "y": 320}])
call("connect", nets=[["V1.+", "R1.1"], ["R1.2", "R2.1", "D1.A"], ["V1.-", "R2.2", "D1.K", "GND1.1"]], labels=["vin", "mid", ""])
time.sleep(2)
print(call("get_gui_state"))
p.stdin.close(); p.wait(5)
