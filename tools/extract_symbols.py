"""Extract Qucs-S component symbols (drawing primitives) from the Qucs-S C++ sources.

Qucs-S builds every symbol in C++ (``Lines.append(new qucs::Line(...))`` ...).
This script transpiles the constructor / ``createSymbol()`` bodies to Python and
executes them against stub objects, so the geometry is taken 1:1 from upstream.

Usage:
    python tools/extract_symbols.py <path-to-qucs_s-checkout> [data/components.json] [data/symbols.json]

The output is consumed by the Rust crate (``include_str!("../data/symbols.json")``).
"""
from __future__ import annotations

import json
import math
import re
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# C++ source scanning
# ---------------------------------------------------------------------------

def strip_comments(s: str) -> str:
    out, i, n = [], 0, len(s)
    while i < n:
        c = s[i]
        if c == '"':
            j = i + 1
            while j < n and s[j] != '"':
                j += 2 if s[j] == "\\" else 1
            out.append(s[i:j + 1]); i = j + 1
        elif c == "'":
            j = i + 1
            while j < n and s[j] != "'":
                j += 2 if s[j] == "\\" else 1
            out.append(s[i:j + 1]); i = j + 1
        elif s.startswith("//", i):
            j = s.find("\n", i)
            i = n if j < 0 else j
        elif s.startswith("/*", i):
            j = s.find("*/", i + 2)
            i = n if j < 0 else j + 2
        elif s.startswith("#", i) and (i == 0 or s[i - 1] == "\n"):
            j = s.find("\n", i)
            i = n if j < 0 else j
        else:
            out.append(c); i += 1
    return "".join(out)


def match_brace(s: str, i: int) -> int:
    """s[i] == '{' -> index after the matching '}' (strings are skipped)."""
    d, n = 0, len(s)
    while i < n:
        c = s[i]
        if c in "\"'":
            q = c; i += 1
            while i < n and s[i] != q:
                i += 2 if s[i] == "\\" else 1
        elif c == "{":
            d += 1
        elif c == "}":
            d -= 1
            if d == 0:
                return i + 1
        i += 1
    return n


FUNC_RE = re.compile(r"\b(\w+)::(~?\w+)\s*\(([^;{}]*?)\)\s*(?:const\s*)?(?:noexcept\s*)?(?::[^;{]*?)?\{", re.S)


def functions(src: str) -> dict[tuple[str, str], tuple[str, str]]:
    out = {}
    for m in FUNC_RE.finditer(src):
        start = m.end() - 1
        end = match_brace(src, start)
        key = (m.group(1), m.group(2))
        if key not in out:
            out[key] = (m.group(3), src[start + 1:end - 1])
    return out


def split_statements(body: str):
    """Very small C++ statement parser -> nested list of ('stmt', text) / ('if', cond, then, else)
    / ('for', init, cond, inc, body) / ('while', cond, body) / ('block', [...])."""
    toks = body
    pos = 0
    n = len(toks)

    def skip_ws(p):
        while p < n and toks[p].isspace():
            p += 1
        return p

    def read_paren(p):
        assert toks[p] == "("
        d = 0; q = p
        while q < n:
            c = toks[q]
            if c in "\"'":
                ch = c; q += 1
                while q < n and toks[q] != ch:
                    q += 2 if toks[q] == "\\" else 1
            elif c == "(":
                d += 1
            elif c == ")":
                d -= 1
                if d == 0:
                    return toks[p + 1:q], q + 1
            q += 1
        return toks[p + 1:], n

    def read_simple(p):
        d = 0; q = p
        while q < n:
            c = toks[q]
            if c in "\"'":
                ch = c; q += 1
                while q < n and toks[q] != ch:
                    q += 2 if toks[q] == "\\" else 1
            elif c in "({[":
                d += 1
            elif c in ")}]":
                d -= 1
            elif c == ";" and d <= 0:
                return toks[p:q].strip(), q + 1
            q += 1
        return toks[p:].strip(), n

    def parse_stmt(p):
        p = skip_ws(p)
        if p >= n:
            return None, p
        if toks[p] == "{":
            e = match_brace(toks, p)
            inner = toks[p + 1:e - 1]
            return ("block", split_statements(inner)), e
        m = re.match(r"(if|for|while|switch|else|do)\b", toks[p:])
        if m:
            kw = m.group(1)
            q = skip_ws(p + len(kw))
            if kw == "if":
                cond, q = read_paren(q)
                then, q = parse_stmt(q)
                q2 = skip_ws(q)
                els = None
                if toks.startswith("else", q2) and not (q2 + 4 < n and (toks[q2 + 4].isalnum() or toks[q2 + 4] == "_")):
                    els, q = parse_stmt(q2 + 4)
                return ("if", cond, then, els), q
            if kw == "for":
                hdr, q = read_paren(q)
                parts = hdr.split(";")
                body_, q = parse_stmt(q)
                if len(parts) == 3:
                    return ("for", parts[0], parts[1], parts[2], body_), q
                return ("skip",), q
            if kw == "while":
                cond, q = read_paren(q)
                body_, q = parse_stmt(q)
                return ("while", cond, body_), q
            if kw == "switch":
                _, q = read_paren(q)
                q = skip_ws(q)
                if q < n and toks[q] == "{":
                    q = match_brace(toks, q)
                return ("skip",), q
            if kw == "do":
                body_, q = parse_stmt(q)
                q = skip_ws(q)
                _, q = read_simple(q)
                return ("skip",), q
        text, q = read_simple(p)
        return ("stmt", text), q

    out = []
    while True:
        st, pos = parse_stmt(pos)
        if st is None:
            break
        out.append(st)
    return out


# ---------------------------------------------------------------------------
# Expression conversion C++ -> Python
# ---------------------------------------------------------------------------

TYPE_RE = re.compile(
    r"^(?:static\s+)?(?:const\s+)?(?:unsigned\s+)?(?:int|double|float|bool|auto|qreal|long|short|"
    r"QString|QStringList|QChar|char|size_t|qsizetype)\s*[&*]?\s+(?=[A-Za-z_])")


def conv_expr(e: str) -> str:
    e = e.strip()
    # strings: protect
    strs: list[str] = []

    def keep(m):
        strs.append(m.group(0))
        return f"__S{len(strs) - 1}__"
    e = re.sub(r'"(?:[^"\\]|\\.)*"' + r"|'(?:[^'\\]|\\.)*'", keep, e)
    e = re.sub(r"\bnew\s+", "", e)
    e = re.sub(r"\bstd::vector\s*<\s*QPointF\s*>\s*", "", e)
    e = re.sub(r"\bQList\s*<\s*QPointF\s*>\s*", "", e)
    e = re.sub(r"\bstatic_cast\s*<[^>]*>", "", e)
    e = re.sub(r"\bqucs::", "", e)
    e = re.sub(r"\bstd::", "", e)
    e = re.sub(r"\bstruct\s+", "", e)
    e = re.sub(r"\bQObject::tr\b", "tr", e)
    e = re.sub(r"\bQString::number\b", "qnum", e)
    e = re.sub(r"\b\w+::createSymbol\b", "createSymbol", e)
    e = re.sub(r"\bQStringLiteral\b", "QString", e)
    e = re.sub(r"\bQPen\s*\{", "QPen(", e)
    e = re.sub(r"\bQBrush\s*\{", "QBrush(", e)
    e = re.sub(r"\bQt::(\w+)", r"'\1'", e)
    e = e.replace("->", ".")
    e = e.replace("::", ".")
    e = e.replace("{", "[").replace("}", "]")
    e = e.replace("&&", " and ").replace("||", " or ")
    e = re.sub(r"!(?!=)", " not ", e)
    e = re.sub(r"\btrue\b", "True", e)
    e = re.sub(r"\bfalse\b", "False", e)
    e = re.sub(r"\bnullptr\b|\bNULL\b", "None", e)
    e = re.sub(r"(\d+\.?\d*)[fF]\b", r"\1", e)
    e = re.sub(r"(\d)[uUlL]+\b", r"\1", e)
    # strings back
    def unkeep(m):
        s = strs[int(m.group(1))]
        return s
    e = re.sub(r"__S(\d+)__", unkeep, e)
    return e


def conv_stmt(s: str) -> list[str]:
    s = s.strip()
    if not s:
        return []
    if s.startswith("return") or s.startswith("break") or s.startswith("continue"):
        w = s.split()[0]
        return [w if w != "return" else "return"]
    m = TYPE_RE.match(s)
    if m:
        rest = s[m.end():]
        # split declarators on top-level commas
        parts, d, cur = [], 0, ""
        for c in rest:
            if c in "([{":
                d += 1
            elif c in ")]}":
                d -= 1
            if c == "," and d == 0:
                parts.append(cur); cur = ""
            else:
                cur += c
        parts.append(cur)
        out = []
        for p in parts:
            p = p.strip()
            if "=" in p:
                out.append(conv_expr(p))
            elif re.match(r"^\w+$", p):
                out.append(f"{p} = 0")
            elif re.match(r"^\w+\s*\(.*\)$", p, re.S):   # QString s("x")
                nm, arg = p.split("(", 1)
                out.append(f"{nm.strip()} = ({conv_expr(arg[:-1])})")
        return out
    m2 = re.match(r"^(?:const\s+)?[A-Z]\w*(?:<[^>]*>)?(?:::\w+)*\s*[&*]?\s+(\w+)\s*=\s*(.+)$", s, re.S)
    if m2:
        return [f"{m2.group(1)} = {conv_expr(m2.group(2))}"]
    # Class declarations like "Property::Builder b;" / "QFontMetrics metrics(...)" -> skip
    if re.match(r"^[A-Z]\w*(?:::\w+)*\s*[&*]?\s+\w+\s*(\(.*\))?$", s, re.S):
        return []
    s2 = re.sub(r"^(\+\+|--)\s*(\w+)$", lambda m: f"{m.group(2)} {m.group(1)[0]}= 1", s)
    s2 = re.sub(r"^(\w+)\s*(\+\+|--)$", lambda m: f"{m.group(1)} {m.group(2)[0]}= 1", s2)
    return [conv_expr(s2)]


def to_python(stmts, ind=1) -> list[str]:
    pad = "    " * ind
    out = []
    for st in stmts:
        k = st[0]
        if k == "stmt":
            for line in conv_stmt(st[1]):
                if line in ("return", "break", "continue"):
                    out.append(pad + line)
                    continue
                line = line.replace("\n", " ")
                try:
                    compile(line, "<stmt>", "exec")
                except SyntaxError:
                    continue
                out.append(pad + "try:")
                out.append(pad + "    " + line)
                out.append(pad + "except Exception: pass")
        elif k == "block":
            out += to_python(st[1], ind) or [pad + "pass"]
        elif k == "if":
            out.append(pad + f"if _cond(lambda: ({conv_expr(st[1])})):".replace("\n", " "))
            out += to_python([st[2]], ind + 1) or [pad + "    pass"]
            if st[3] is not None:
                out.append(pad + "else:")
                out += to_python([st[3]], ind + 1) or [pad + "    pass"]
        elif k == "for":
            init = conv_stmt(st[1]) if st[1].strip() else []
            for line in init:
                out.append(pad + "try:")
                out.append(pad + "    " + line)
                out.append(pad + "except Exception: pass")
            cond = conv_expr(st[2]) if st[2].strip() else "True"
            inc = conv_stmt(st[3]) if st[3].strip() else []
            g, f = f"_g{ind}", f"_f{ind}"
            out.append(pad + f"{g} = 0")
            out.append(pad + f"{f} = True")
            out.append(pad + "while True:")
            out.append(pad + f"    {g} += 1")
            out.append(pad + f"    if {g} > 200: break")
            out.append(pad + f"    if not {f}:")
            for line in inc or ["pass"]:
                out.append(pad + "        " + line)
            out.append(pad + f"    {f} = False")
            out.append(pad + f"    if not _cond(lambda: ({cond}), False): break".replace("\n", " "))
            body = to_python([st[4]], ind + 1)
            out += body or [pad + "    pass"]
        elif k == "while":
            out.append(pad + "_guard = 0")
            out.append(pad + f"while _cond(lambda: ({conv_expr(st[1])}), False):")
            out.append(pad + "    _guard += 1")
            out.append(pad + "    if _guard > 200: break")
            out += to_python([st[2]], ind + 1) or [pad + "    pass"]
    return out


# ---------------------------------------------------------------------------
# Runtime stubs
# ---------------------------------------------------------------------------

QT_COLORS = {
    "black": "#000000", "white": "#ffffff", "darkBlue": "#000080", "blue": "#0000ff",
    "red": "#ff0000", "darkRed": "#800000", "green": "#00ff00", "darkGreen": "#008000",
    "gray": "#a0a0a4", "darkGray": "#808080", "lightGray": "#c0c0c0", "cyan": "#00ffff",
    "darkCyan": "#008080", "magenta": "#ff00ff", "darkMagenta": "#800080", "yellow": "#ffff00",
    "darkYellow": "#808000", "transparent": "none", "NoBrush": "none", "NoPen": "none",
}


def color_of(c) -> str:
    if isinstance(c, QColor):
        return c.hex
    if isinstance(c, str):
        if c.startswith("#"):
            return c
        return QT_COLORS.get(c, "#000080")
    return "#000080"


class QColor:
    def __init__(self, *a):
        if len(a) >= 3:
            self.hex = "#%02x%02x%02x" % tuple(int(v) for v in a[:3])
        elif len(a) == 1:
            self.hex = color_of(a[0])
        else:
            self.hex = "#000000"


class QStr(str):
    def toInt(self, *a):
        try:
            return int(float(self.strip().split()[0]))
        except Exception:
            return 0
    toUInt = toInt
    def toDouble(self, *a):
        try:
            return float(self.strip().split()[0])
        except Exception:
            return 0.0
    def at(self, i):
        return QStr(self[i]) if i < len(self) else QStr("")
    def isEmpty(self):
        return len(self) == 0
    def length(self):
        return len(self)
    size = length
    count = length
    def left(self, n):
        return QStr(self[:n])
    def right(self, n):
        return QStr(self[-n:]) if n else QStr("")
    def mid(self, a, n=-1):
        return QStr(self[a:] if n < 0 else self[a:a + n])
    def toLower(self):
        return QStr(self.lower())
    def toUpper(self):
        return QStr(self.upper())
    def section(self, sep, a, b=None):
        parts = self.split(sep)
        b = a if b is None else b
        return QStr(sep.join(parts[a:b + 1]))
    def __add__(self, o):
        return QStr(str(self) + str(o))
    def __radd__(self, o):
        return QStr(str(o) + str(self))
    def arg(self, *a):
        s = str(self)
        for i, v in enumerate(a):
            s = s.replace(f"%{i + 1}", str(v))
        return QStr(s)


class Prop:
    def __init__(self, name, value, display=False, desc=""):
        self.Name = QStr(name); self.Value = QStr(value); self.display = display; self.Description = desc


class QList(list):
    def __init__(self, *a, frozen=False):
        super().__init__(*a)
        self.frozen = frozen
    def append(self, x):
        if not self.frozen:
            super().append(x)
    def at(self, i):
        return self[i]
    def back(self):
        return self[-1]
    last = back
    def front(self):
        return self[0]
    first = front
    def count(self, *a):
        return len(self)
    size = count
    length = count
    def isEmpty(self):
        return len(self) == 0
    def clear(self):
        del self[:]


def pen(c="darkBlue", w=1, style="SolidLine", cap=None, join=None):
    return {"color": color_of(c), "width": float(w) if not isinstance(w, str) else 1.0,
            "style": style if isinstance(style, str) else "SolidLine"}


def brush(c="NoBrush", *a):
    if isinstance(c, str) and c in ("NoBrush",):
        return None
    if a and a[0] == "NoBrush":
        return None
    return color_of(c)


def make_env(model: str, props: list[dict], symbol: dict):
    env: dict = {}

    def Line(x1, y1, x2, y2, p=None):
        return {"k": "line", "x1": x1, "y1": y1, "x2": x2, "y2": y2, "pen": p or pen()}

    def Arc(x, y, w, h, a, l, p=None):
        return {"k": "arc", "x": x, "y": y, "w": w, "h": h, "angle": int(a), "arclen": int(l), "pen": p or pen()}

    def Rect(x, y, w, h, p=None, b=None):
        return {"k": "rect", "x": x, "y": y, "w": w, "h": h, "pen": p or pen(), "brush": b}

    def Ellips(x, y, w, h, p=None, b=None):
        return {"k": "ellipse", "x": x, "y": y, "w": w, "h": h, "pen": p or pen(), "brush": b}

    def Polyline(pts, p=None, b=None):
        return {"k": "polyline", "points": [list(map(float, q)) for q in pts], "pen": p or {"color": "none", "width": 0, "style": "NoPen"}, "brush": b}

    def Text(x, y, s, c=None, size=10.0, mcos=1.0, msin=0.0):
        return {"k": "text", "x": x, "y": y, "s": str(s), "color": color_of(c) if c is not None else "#000000",
                "size": float(size), "mcos": float(mcos), "msin": float(msin)}

    def Port(x, y, *a):
        return [x, y]

    def QPointF(x, y):
        return [x, y]

    def tr(s, *a):
        return QStr(s)

    def QString(*a):
        return QStr(a[0]) if a else QStr("")

    def qnum(v, *a):
        if isinstance(v, float) and v.is_integer():
            v = int(v)
        return QStr(str(v))

    def _cond(f, default=True):
        try:
            return bool(f())
        except Exception:
            return default

    Lines, Arcs, Rects, Ellipses, Polylines, Texts, Ports = (QList() for _ in range(7))

    def rotate(*a):
        if len(Ports) < 1:
            return False
        for p in Lines:
            p["x1"], p["y1"] = p["y1"], -p["x1"]
            p["x2"], p["y2"] = p["y2"], -p["x2"]
        for p in Ports:
            p[0], p[1] = p[1], -p[0]
        for p in Arcs:
            x, y, w, h = p["x"], p["y"], p["w"], p["h"]
            p["x"], p["y"], p["w"], p["h"] = y, -x - w, h, w
            p["angle"] += 16 * 90
            if p["angle"] >= 16 * 360:
                p["angle"] -= 16 * 360
        for lst in (Rects, Ellipses):
            for p in lst:
                x, y, w, h = p["x"], p["y"], p["w"], p["h"]
                p["x"], p["y"], p["w"], p["h"] = y, -x - w, h, w
        for t in Texts:
            t["x"], t["y"] = t["y"], -t["x"]
            t["msin"], t["mcos"] = t["mcos"], -t["msin"]
        for pl in Polylines:
            pl["points"] = [[q[1], -q[0]] for q in pl["points"]]
        g = env
        x1, y1, x2, y2 = g.get("x1", 0), g.get("y1", 0), g.get("x2", 0), g.get("y2", 0)
        g["x1"], g["y1"], g["x2"], g["y2"] = y1, -x2, y2, -x1
        g["tx"], g["ty"] = g.get("ty", 0), -g.get("tx", 0)
        g["rotated"] = (g.get("rotated", 0) + 1) & 3
        return True

    def mirrorX(*a):
        g = env
        g["mirroredX"] = not g.get("mirroredX", False)
        g["_mirror_calls"] = g.get("_mirror_calls", 0) + 1
        return True

    def createSymbol(*a):
        body = symbol.get("createSymbol")
        if body:
            exec(body, env)
            env["_createSymbol"]()

    plist = QList([Prop(p["name"], p["default"], p.get("display", False), p.get("description", "")) for p in props],
                  frozen=True)

    def getProperty(name):
        for p in plist:
            if p.Name == name:
                return p
        return Prop(name, "")

    class _Settings:
        def __getattr__(self, k):
            raise AttributeError(k)

    env.update(dict(
        Line=Line, Arc=Arc, Rect=Rect, Ellips=Ellips, Polyline=Polyline, Text=Text, Port=Port,
        QPointF=QPointF, QPoint=QPointF, QPen=pen, QBrush=brush, QColor=QColor, tr=tr, QString=QString, qnum=qnum,
        _cond=_cond, Lines=Lines, Arcs=Arcs, Rects=Rects, Ellipses=Ellipses, Polylines=Polylines,
        Texts=Texts, Ports=Ports, Props=plist, getProperty=getProperty, rotate=rotate, mirrorX=mirrorX,
        createSymbol=createSymbol, Model=QStr(model), Name=QStr(""), x1=0, y1=0, x2=0, y2=0, tx=0, ty=0,
        rotated=0, mirroredX=False, Description=QStr(""), Type=0, SpiceModel=QStr(""), isEquation=False,
        Simulator=0, showName=True, int=lambda v=0: int(v), double=float, float=float, abs=abs,
        max=max, min=min, round=round, sqrt=math.sqrt, qMax=max, qMin=min, QucsSettings=_Settings(),
        recreate=lambda *a: None, setNodePortCenter=lambda *a: None, Ellipses_append=None,
    ))
    return env


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------

def py_func(name: str, params: str, body: str) -> str:
    stmts = split_statements(body)
    lines = to_python(stmts, 1)
    # declare globals so assignments to x1/ty/... land in the shared namespace
    gl = "    global x1, y1, x2, y2, tx, ty, Model, Name, SpiceModel, Description, Type, rotated, mirroredX, isEquation, showName, Simulator"
    return f"def {name}():\n{gl}\n" + "\n".join(lines or ["    pass"]) + "\n"


def collect_sources(root: Path):
    dirs = [root / "qucs" / "components", root / "qucs" / "spicecomponents", root / "qucs" / "magnetics"]
    classes: dict[str, dict] = {}
    bases: dict[str, str] = {}
    for d in dirs:
        if not d.is_dir():
            continue
        for h in d.glob("*.h"):
            src = strip_comments(h.read_text(encoding="utf-8", errors="replace"))
            for m in re.finditer(r"class\s+(\w+)\s*:\s*public\s+(?:qucs::component::)?(\w+)", src):
                bases[m.group(1)] = m.group(2)
            # default args of constructors
            for m in re.finditer(r"\b(\w+)\s*\(([^()]*=[^()]*)\)\s*;", src):
                classes.setdefault(m.group(1), {}).setdefault("defaults", m.group(2))
        for f in d.glob("*.cpp"):
            src = strip_comments(f.read_text(encoding="utf-8", errors="replace"))
            for (cls, meth), (params, body) in functions(src).items():
                c = classes.setdefault(cls, {})
                c.setdefault("file", f.name)
                c.setdefault("methods", {})[meth] = (params, body)
    return classes, bases


def param_values(params: str, defaults: str | None) -> dict:
    vals = {}
    dmap = {}
    if defaults:
        for part in defaults.split(","):
            if "=" in part:
                lhs, rhs = part.split("=", 1)
                nm = lhs.strip().split()[-1].lstrip("&*")
                dmap[nm] = rhs.strip()
    for part in params.split(","):
        part = part.strip()
        if not part:
            continue
        nm = part.split("=")[0].strip().split()[-1].lstrip("&*")
        dv = dmap.get(nm)
        if "=" in part:
            dv = part.split("=", 1)[1].strip()
        if dv is None:
            dv = "true" if part.startswith("bool") else "0"
        try:
            vals[nm] = eval(conv_expr(dv), {"QString": str, "tr": str})
        except Exception:
            vals[nm] = 0
    return vals


def find_method(classes, bases, cls, meth):
    seen = set()
    while cls and cls not in seen:
        seen.add(cls)
        m = classes.get(cls, {}).get("methods", {}).get(meth)
        if m:
            return m
        cls = bases.get(cls)
    return None


def run_variant(classes, bases, cls, model, props, overrides):
    ctor = classes[cls]["methods"][cls]
    csym = find_method(classes, bases, cls, "createSymbol")
    symbol = {}
    if csym:
        symbol["createSymbol"] = py_func("_createSymbol", "", csym[1])
    props2 = [dict(p) for p in props]
    for p in props2:
        if p["name"] in overrides:
            p["default"] = overrides[p["name"]]
    env = make_env(model, props2, symbol)
    env.update(param_values(ctor[0], classes[cls].get("defaults")))
    # base constructor (e.g. GateComponent) first
    base = bases.get(cls)
    if base and base in classes and base in classes[base].get("methods", {}) and base not in ("Component", "MultiViewComponent"):
        exec(py_func("_basector", "", classes[base]["methods"][base][1]), env)
        try:
            env["_basector"]()
        except Exception:
            pass
    exec(py_func("_ctor", "", ctor[1]), env)
    env["_ctor"]()
    return env


def num(v):
    v = float(v)
    return int(v) if v.is_integer() else round(v, 3)


def clean(prim):
    out = {}
    for k, v in prim.items():
        if isinstance(v, (int, float)) and not isinstance(v, bool):
            out[k] = num(v)
        else:
            out[k] = v
    return out


def symbol_from_env(env):
    prims = []
    for lst in ("Lines", "Polylines", "Arcs", "Rects", "Ellipses", "Texts"):
        for p in env[lst]:
            if p["k"] == "polyline":
                p = dict(p); p["points"] = [[num(a), num(b)] for a, b in p["points"]]
            prims.append(clean(p))
    return {
        "prims": prims,
        "ports": [[num(x), num(y)] for x, y in env["Ports"]],
        "bbox": [num(env["x1"]), num(env["y1"]), num(env["x2"]), num(env["y2"])],
        "tx": num(env["tx"]), "ty": num(env["ty"]),
        "rotated": int(env["rotated"]) & 3,
    }


def main():
    root = Path(sys.argv[1])
    comp_db = Path(sys.argv[2] if len(sys.argv) > 2 else "data/components.json")
    out = Path(sys.argv[3] if len(sys.argv) > 3 else "data/symbols.json")
    db = json.loads(comp_db.read_text(encoding="utf-8"))
    classes, bases = collect_sources(root)

    # Model -> class
    model_cls: dict[str, str] = {}
    for cls, c in classes.items():
        ctor = c.get("methods", {}).get(cls)
        if not ctor:
            continue
        for m in re.finditer(r'\bModel\s*=\s*"([^"]+)"', ctor[1]):
            model_cls.setdefault(m.group(1), cls)
    result = {}
    failed = []
    for model, info in db.items():
        cls = model_cls.get(model)
        if not cls:
            failed.append((model, "no class"))
            continue
        props = info.get("props", [])
        variants_spec = [{}]
        for p in props:
            mm = re.search(r"\[([^\]]+)\]", p.get("description", ""))
            if p["name"] == "Symbol" and mm:
                opts = [o.strip() for o in mm.group(1).split(",")]
                variants_spec = [{"Symbol": o} for o in opts]
        if props and props[0]["name"] == "in" and "input" in props[0].get("description", "") + "input":
            variants_spec = [dict(v, **{"in": str(k)}) for v in variants_spec for k in range(2, 9)]
        sym = None
        variants = []
        for spec in variants_spec:
            try:
                env = run_variant(classes, bases, cls, model, props, spec)
                s = symbol_from_env(env)
            except Exception as e:  # noqa: BLE001
                failed.append((model, f"{spec}: {type(e).__name__}: {e}"))
                continue
            if sym is None and all(info_default(props, k) == v for k, v in spec.items()):
                sym = s
            variants.append({"when": spec, **s})
        if sym is None and variants:
            sym = {k: v for k, v in variants[0].items() if k != "when"}
        if sym is None:
            continue
        sym["class"] = cls
        if len(variants) > 1:
            sym["variants"] = variants
        # pin names aligned with the Qucs port order (DB pins are in SPICE order, rotation 0)
        dbp = [tuple(p) for p in info.get("pins") or []]
        names = info.get("pin_names") or []
        r0 = [tuple(p) for p in transform_ports(sym, 0, 0)]
        sym["pin_names"] = [names[dbp.index(p)] if p in dbp and dbp.index(p) < len(names) else str(i + 1)
                            for i, p in enumerate(r0)]
        # text-like blocks (simulations, equations, SPICE statements) are drawn from their title
        ctor_body = classes[cls]["methods"][cls][1]
        if not sym["prims"]:
            env = run_variant(classes, bases, cls, model, props, {})
            sym["description"] = str(env.get("Description", ""))
            mt = re.search(r'Texts\.append\(\s*new\s+Text\([^;]*?(?:QObject::tr\()?"([^"]+)"', ctor_body)
            if mt:
                sym["title"] = mt.group(1)
            mc = re.search(r"Lines\.append\(new qucs::Line\([^;]*?QPen\(Qt::(\w+)", ctor_body)
            if mc:
                sym["color"] = QT_COLORS.get(mc.group(1), "#000080")
            hdr = None
            for d in ("components", "spicecomponents"):
                hp = root / "qucs" / d / classes[cls]["file"].replace(".cpp", ".h")
                if hp.exists():
                    hdr = hp.read_text(encoding="utf-8", errors="replace")
            if hdr and model.startswith("."):
                mcol = re.search(r"color\(\)\s*const\s*override\s*\{\s*return\s+Qt::(\w+)", hdr)
                sym["color"] = QT_COLORS.get(mcol.group(1), "#000080") if mcol else "#000080"
        result[model] = sym
    out.write_text(json.dumps(result, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")

    # report
    empty = [m for m, s in result.items() if not s["prims"]]
    mism = []
    for m, s in result.items():
        pins = db[m].get("pins")
        if not pins:
            continue
        # DB pins are measured in the template orientation
        mir, rot = 0, 0
        pts = transform_ports(s, mir, rot)
        if sorted(map(tuple, pts)) != sorted(map(tuple, pins)):
            mism.append((m, pts, pins, mir, rot))
    print(f"extracted {len(result)}/{len(db)} symbols; empty={len(empty)} {empty}")
    print(f"failed: {failed}")
    print(f"pin mismatches ({len(mism)}):")
    for x in mism:
        print("  ", x)


def info_default(props, name):
    for p in props:
        if p["name"] == name:
            return p["default"]
    return None


def transform_ports(sym, mirror, rotate):
    """Apply Qucs load semantics (mirror first, then rotate up to the requested count)."""
    pts = [list(p) for p in sym["ports"]]
    k = sym.get("rotated", 0)
    if mirror:
        pts = [[x, -y] for x, y in pts]
    t = rotate
    if k > t:
        t += 4
    for _ in range(t - k):
        pts = [[y, -x] for x, y in pts]
    return pts


if __name__ == "__main__":
    main()
