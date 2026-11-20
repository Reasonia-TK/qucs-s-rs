"""Monte Carlo check through MCP: 5th-order filter, 60 runs, measure at the corner."""
import base64, json, os, subprocess, sys, time
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
EXE = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "debug" / "qucs-s-rs.exe"
OUT = ROOT / "target" / "e2e"
p = subprocess.Popen([str(EXE), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env={**os.environ, "QUCS_MCP_WORKDIR": str(OUT)})
n = 0
def call(tool_name, **a):
    global n
    n += 1
    p.stdin.write((json.dumps({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": tool_name, "arguments": a}}) + "\n").encode()); p.stdin.flush()
    r = json.loads(p.stdout.readline())["result"]
    t = [c["text"] for c in r["content"] if c["type"] == "text"]
    if r["isError"]: raise SystemExit(f"{tool_name}: {t}")
    return json.loads(t[0]), [c for c in r["content"] if c["type"] == "image"]
call("new_schematic", path="mc_filter", overwrite=True)
call("design_filter", response="chebyshev", ripple_db=0.5, **{"class": "lowpass"}, order=5, f1="10 MHz", z0=50, test_bench=True)
t0 = time.time()
s, imgs = call("run_monte_carlo", runs=60, tolerances={"C": 5, "L": 5, "R": 1}, analysis="ac1", vectors=["v(out)"], at=[5e6, 10e6, 15e6],
               limits=[{"vector": "v(out)", "at": 5e6, "min": -7.0}, {"vector": "v(out)", "at": 15e6, "max": -20}])
print("time %.1fs" % (time.time() - t0))
print(json.dumps({k: s[k] for k in ("runs", "successful", "failed", "distribution", "yield", "first_error")}, ensure_ascii=False))
for m in s["measurements"]:
    print(m["vector"], m["at"], m["unit"], "nominal", round(m["nominal"], 2), {k: m["stats"][k] for k in ("mean", "std", "min", "max")})
print("extremes:", json.dumps(s["extremes"]["lowest"])[:300])
for k, im in enumerate(imgs):
    (OUT / f"mc_{k}.png").write_bytes(base64.b64decode(im["data"]))
p.stdin.close(); p.wait(5)
print("OK", len(imgs), "images")
