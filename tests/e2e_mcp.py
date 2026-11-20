"""End-to-end test: drive `qucs-s-rs mcp` over stdio like an MCP client.

Builds a two-stage BJT amplifier from scratch, wires it with `connect`,
checks the layout, takes snapshots, simulates and plots.

    python tests/e2e_mcp.py [path-to-qucs-s-rs]
"""
import base64
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "debug" / "qucs-s-rs.exe"
OUT = ROOT / "target" / "e2e"
OUT.mkdir(parents=True, exist_ok=True)


class Client:
    def __init__(self):
        self.p = subprocess.Popen([str(EXE), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  env={**__import__("os").environ, "QUCS_MCP_WORKDIR": str(OUT)})
        self.n = 0

    def rpc(self, method, params=None):
        self.n += 1
        msg = {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}
        self.p.stdin.write((json.dumps(msg) + "\n").encode())
        self.p.stdin.flush()
        line = self.p.stdout.readline()
        r = json.loads(line)
        if "error" in r:
            raise RuntimeError(r["error"])
        return r["result"]

    def call(self, name, **args):
        r = self.rpc("tools/call", {"name": name, "arguments": args})
        texts = [c["text"] for c in r["content"] if c["type"] == "text"]
        imgs = [c for c in r["content"] if c["type"] == "image"]
        if r.get("isError"):
            raise RuntimeError(f"{name}: {texts}")
        data = json.loads(texts[0]) if texts and texts[0][:1] in "{[" else (texts[0] if texts else None)
        return data, imgs


def main():
    c = Client()
    init = c.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}})
    assert init["serverInfo"]["name"] == "qucs-s-rs"
    tools = c.rpc("tools/list")["tools"]
    print("tools:", len(tools))

    c.call("new_schematic", path="two_stage", overwrite=True)
    parts = [
        {"type": "Vdc", "name": "VCC", "x": 60, "y": 200, "props": {"U": "12 V"}},
        {"type": "Vac", "name": "VIN", "x": 60, "y": 420, "props": {"U": "10 mV"}},
        {"type": "C", "name": "C1", "x": 160, "y": 360, "props": {"C": "10u"}},
        {"type": "R", "name": "RB1", "x": 260, "y": 220, "rotate": 1, "props": {"R": "47k"}},
        {"type": "R", "name": "RB2", "x": 260, "y": 480, "rotate": 1, "props": {"R": "10k"}},
        {"type": "_BJT", "name": "Q1", "x": 360, "y": 360},
        {"type": "R", "name": "RC1", "x": 390, "y": 220, "rotate": 1, "props": {"R": "4.7k"}},
        {"type": "R", "name": "RE1", "x": 390, "y": 480, "rotate": 1, "props": {"R": "1k"}},
        {"type": "C", "name": "C2", "x": 500, "y": 300, "props": {"C": "10u"}},
        {"type": "R", "name": "RB3", "x": 600, "y": 220, "rotate": 1, "props": {"R": "47k"}},
        {"type": "R", "name": "RB4", "x": 600, "y": 480, "rotate": 1, "props": {"R": "10k"}},
        {"type": "_BJT", "name": "Q2", "x": 700, "y": 360},
        {"type": "R", "name": "RC2", "x": 730, "y": 220, "rotate": 1, "props": {"R": "4.7k"}},
        {"type": "R", "name": "RE2", "x": 730, "y": 480, "rotate": 1, "props": {"R": "1k"}},
        {"type": "GND", "x": 60, "y": 560},
    ]
    added, _ = c.call("add_components", components=parts)
    print("added:", [a["name"] for a in added["added"]], "overlaps:", added["overlaps"])
    pins = {p["pin"]: (p["x"], p["y"]) for a in added["added"] for p in a["pins"]}
    print("Q1 pins:", {k: v for k, v in pins.items() if k.startswith("Q1")})

    nets = [
        ["VCC.+", "RB1.1", "RC1.1", "RB3.1", "RC2.1"],
        ["VCC.-", "VIN.-", "RB2.2", "RE1.2", "RB4.2", "RE2.2", "GND1.1"],
        ["VIN.+", "C1.1"],
        ["C1.2", "RB1.2", "RB2.1", "Q1.B"],
        ["Q1.C", "RC1.2", "C2.1"],
        ["Q1.E", "RE1.1"],
        ["C2.2", "RB3.2", "RB4.1", "Q2.B"],
        ["Q2.C", "RC2.2"],
        ["Q2.E", "RE2.1"],
    ]
    r, _ = c.call("connect", nets=nets, labels=["vcc", "", "in", "b1", "c1", "", "b2", "out", ""])
    for res in r["results"]:
        if "failed" in res:
            print("FAILED:", res)
    print("flipped:", r.get("flipped_for_shorter_wires"), "improvement:", r.get("improvement"))
    print("metrics after connect:", r["metrics"])

    chk, _ = c.call("check_layout")
    print("issues:", [(i["kind"], i["message"]) for i in chk["issues"]])

    meta, img = c.call("snapshot")
    (OUT / "two_stage_snapshot.png").write_bytes(base64.b64decode(img[0]["data"]))
    meta, img = c.call("snapshot", clean=True)
    (OUT / "two_stage_clean.png").write_bytes(base64.b64decode(img[0]["data"]))
    print("snapshot:", meta["size_px"], meta["coordinate_mapping"])

    c.call("add_simulation", kind="tr", props={"Stop": "2 ms", "Points": "401"})
    c.call("add_simulation", kind="ac", props={"Type": "log", "Start": "10 Hz", "Stop": "100 MHz", "Points": "201"})
    sim, _ = c.call("run_simulation")
    print("sim ok:", sim["ok"], "errors:", sim["errors"][:5])
    for a in sim["analyses"]:
        print("  ", a["analysis"], a["kind"], a["points"], list(a["vectors"])[:8])
    if sim["ok"]:
        res, _ = c.call("get_results", analysis="ac1", vectors=["v(out)"], at=[1000])
        print("gain @1kHz:", res[0]["at"])
        _, img = c.call("plot_results", analysis="ac1", vectors=["v(out)", "v(c1)"])
        (OUT / "two_stage_ac.png").write_bytes(base64.b64decode(img[0]["data"]))
        _, img = c.call("plot_results", analysis="tr1", vectors=["v(in)", "v(out)"])
        (OUT / "two_stage_tr.png").write_bytes(base64.b64decode(img[0]["data"]))

    # move a part: wires follow
    mv, _ = c.call("move_components", moves=[{"name": "Q2", "dx": 40}, {"name": "RC2", "dx": 40}, {"name": "RE2", "dx": 40}])
    meta, img = c.call("snapshot")
    (OUT / "two_stage_moved.png").write_bytes(base64.b64decode(img[0]["data"]))
    print("after move:", meta["metrics"])
    sch, _ = c.call("get_schematic", include_wires=False)
    print("nets:", [(n["name"], len(n["pins"])) for n in sch["nets"]])
    c.p.stdin.close()
    c.p.wait(5)
    print("OK, outputs in", OUT)


if __name__ == "__main__":
    main()
