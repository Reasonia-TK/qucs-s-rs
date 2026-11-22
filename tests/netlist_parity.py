"""Built-in netlister vs. qucs-s: simulate the same schematics with both and
compare every output vector (needs Qucs-S and ngspice).

    python tests/netlist_parity.py [path-to-qucs-s-rs]

Builds a device zoo (diode, BJT, MOSFET, JFET, controlled sources, probes,
pulse / rect sources, op-amp, Nutmeg equations) with the MCP server and adds
the example schematics.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from e2e_mcp import Client, EXE, OUT, ROOT  # noqa: E402


def build_zoo():
    c = Client()
    c.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "parity", "version": "0"}})
    c.call("new_schematic", path="parity_zoo", overwrite=True)
    parts = [
        {"type": "Vdc", "name": "V1", "x": 60, "y": 200, "props": {"U": "5 V"}},
        {"type": "R", "name": "R1", "x": 180, "y": 100, "props": {"R": "1k"}},
        {"type": "Diode", "name": "D1", "x": 300, "y": 200, "rotate": 1},
        {"type": "R", "name": "RB", "x": 420, "y": 100, "props": {"R": "220k"}},
        {"type": "R", "name": "RC", "x": 560, "y": 100, "rotate": 1, "props": {"R": "2k"}},
        {"type": "_BJT", "name": "Q1", "x": 560, "y": 260},
        {"type": "R", "name": "RD", "x": 760, "y": 100, "rotate": 1, "props": {"R": "1k"}},
        {"type": "_MOSFET", "name": "M1", "x": 760, "y": 260, "props": {"Vt0": "1 V", "Kp": "2e-3"}},
        {"type": "Vpulse", "name": "VP", "x": 660, "y": 420, "props": {"U1": "0 V", "U2": "3 V", "T1": "10 us", "T2": "60 us", "Tr": "1 us", "Tf": "1 us"}},
        {"type": "IProbe", "name": "Pr1", "x": 900, "y": 100},
        {"type": "R", "name": "RJ", "x": 1040, "y": 160, "rotate": 1, "props": {"R": "1k"}},
        {"type": "JFET", "name": "J1", "x": 1040, "y": 300},
        {"type": "VCVS", "name": "E1", "x": 300, "y": 520, "props": {"G": "2"}},
        {"type": "R", "name": "RE", "x": 440, "y": 520, "rotate": 1, "props": {"R": "1k"}},
        {"type": "CCCS", "name": "F1", "x": 300, "y": 720, "props": {"G": "3"}},
        {"type": "R", "name": "RF", "x": 440, "y": 720, "rotate": 1, "props": {"R": "100"}},
        {"type": "R", "name": "RS", "x": 200, "y": 640, "rotate": 1, "props": {"R": "50"}},
        {"type": "Vrect", "name": "VR", "x": 100, "y": 720, "props": {"U": "1 V", "TH": "20 us", "TL": "30 us"}},
        {"type": "VProbe", "name": "VPr", "x": 200, "y": 860},
        {"type": "OpAmp", "name": "OP1", "x": 860, "y": 560},
        {"type": "R", "name": "RO", "x": 1000, "y": 620, "rotate": 1, "props": {"R": "10k"}},
        {"type": "GND", "x": 60, "y": 300},
        {"type": ".TR", "name": "TR1", "x": 60, "y": 1000, "props": {"Stop": "100 us", "Points": "201"}},
        {"type": ".AC", "name": "AC1", "x": 300, "y": 1000, "props": {"Start": "1 kHz", "Stop": "1 MHz", "Points": "61", "Type": "log"}},
        {"type": ".DC", "name": "DC1", "x": 540, "y": 1000},
    ]
    c.call("add_components", components=parts)
    nets = [
        {"terminals": ["V1.+", "R1.1", "RB.1", "RC.2", "RD.2", "Pr1.1"], "label": "vcc"},
        {"terminals": ["R1.2", "D1.A"], "label": "dio"},
        {"terminals": ["RB.2", "Q1.B"], "label": "base"},
        {"terminals": ["RC.1", "Q1.C"], "label": "coll"},
        {"terminals": ["RD.1", "M1.D"], "label": "drain"},
        {"terminals": ["VP.+", "M1.G", "E1.3", "OP1.IN+"], "label": "gate"},
        {"terminals": ["Pr1.2", "RJ.2"], "label": "jtop"},
        {"terminals": ["RJ.1", "J1.D"], "label": "jd"},
        {"terminals": ["E1.1", "RE.2"], "label": "eout"},
        {"terminals": ["VR.+", "F1.3"], "label": "rect"},
        {"terminals": ["F1.4", "RS.2"], "label": "sense"},
        {"terminals": ["F1.1", "RF.2", "VPr.1"], "label": "fout"},
        {"terminals": ["OP1.OUT", "RO.2", "OP1.IN-"], "label": "opout"},
        {"terminals": ["V1.-", "D1.K", "Q1.E", "M1.S", "VP.-", "J1.G", "J1.S", "E1.2", "E1.4", "RE.1", "F1.2", "RS.1", "RF.1", "VR.-", "VPr.2", "RO.1", "GND1.1"]},
    ]
    r, _ = c.call("connect", nets=[n["terminals"] for n in nets], labels=[n.get("label", "") for n in nets])
    for res in r["results"]:
        if "failed" in res:
            print("connect FAILED:", res)
    c.call("save_schematic")
    c.p.terminate()
    return OUT / "parity_zoo.sch"


def simulate(sch, netlister):
    env = {**os.environ, "QUCS_RS_NETLISTER": netlister}
    r = subprocess.run([str(EXE), "simulate", str(sch)], capture_output=True, text=True, env=env)
    if r.returncode != 0:
        raise RuntimeError(f"{netlister}: {r.stderr.strip()}")
    return json.loads(r.stdout)


def close(a, b, rel=2e-3, abs_=1e-9):
    if isinstance(a, list):
        return all(close(x, y, rel, abs_) for x, y in zip(a, b))
    if not isinstance(a, (int, float)) or not isinstance(b, (int, float)):
        return a == b
    return abs(a - b) <= abs_ + rel * max(abs(a), abs(b))


def compare(sch):
    a = simulate(sch, "builtin")
    b = simulate(sch, "qucs-s")
    bad = []
    an = {x["analysis"]: x for x in a["analyses"]}
    bn = {x["analysis"]: x for x in b["analyses"]}
    if set(an) != set(bn):
        bad.append(f"analyses differ: {sorted(an)} vs {sorted(bn)}")
    for name in sorted(set(an) & set(bn)):
        va, vb = an[name]["vectors"], bn[name]["vectors"]
        la = {k.lower(): v for k, v in va.items()}
        lb = {k.lower(): v for k, v in vb.items()}
        if set(la) != set(lb):
            bad.append(f"{name}: vectors {sorted(la)} vs {sorted(lb)}")
        for k in sorted(set(la) & set(lb)):
            for f in set(la[k]) & set(lb[k]):
                if f in ("n", "unit", "complex"):
                    continue
                if not close(la[k][f], lb[k][f]):
                    bad.append(f"{name}.{k}.{f}: {la[k][f]} vs {lb[k][f]}")
    return a, bad


def main():
    files = [build_zoo(), ROOT / "examples" / "inverting_amp.sch", ROOT / "examples" / "arxiv1902_02074_rf_plasma.sch"]
    e2e = OUT / "two_stage.sch"
    if e2e.exists():
        files.append(e2e)
    failed = 0
    for f in files:
        a, bad = compare(f)
        names = [x["analysis"] for x in a["analyses"]]
        if bad:
            failed += 1
            print(f"FAIL {f.name} ({', '.join(names)})")
            for b in bad[:20]:
                print("   ", b)
        else:
            nv = sum(len(x["vectors"]) for x in a["analyses"])
            print(f"ok   {f.name}: {', '.join(names)} - {nv} vectors identical")
    failed += hierarchy_check(files[0])
    sys.exit(1 if failed else 0)


def op(sch):
    r = subprocess.run([str(EXE), "op", str(sch)], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip())
    return json.loads(r.stdout)["node_voltages_V"]


def hierarchy_check(zoo):
    """Moving the BJT stage into a subcircuit must not change the operating point."""
    flat = op(zoo)
    hier = OUT / "parity_hier.sch"
    hier.write_bytes(zoo.read_bytes())
    c = Client()
    c.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "parity", "version": "0"}})
    c.call("open_schematic", path=str(hier))
    r, _ = c.call("create_subcircuit", components=["Q1", "RB", "RC", "M1", "RD"], file="parity_stage.sch", overwrite=True)
    c.p.terminate()
    h = op(hier)
    common = [k for k in flat if k in h and not k.startswith("_net")]
    bad = [f"{k}: {flat[k]} vs {h[k]}" for k in common if not close(flat[k], h[k], 1e-6, 1e-9)]
    if bad or len(common) < 5:
        print(f"FAIL hierarchy ({len(common)} nets compared)")
        for b in bad:
            print("   ", b)
        return 1
    print(f"ok   hierarchy: {len(r['ports'])} ports, {len(common)} node voltages unchanged after create_subcircuit")
    return 0


if __name__ == "__main__":
    main()
