"""Read the resource tables of docs/ui-api.md: every path, its methods, and the fields it documents.

The notes column writes shapes in a loose JSON-like way (`{ id, name, role: "admin" \\| "watch",
cars: [id] }`, `{ ac, dc } each { kwh, ms }`, `Drive plus prev, next`). This module turns them into
trees of field names: {"keys": {name: tree | None}} for an object, {"list": tree} for a list, None
for a plain value. Optional fields end in "?" in the document and are left out.
"""

from __future__ import annotations

import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
DOC = os.path.normpath(os.path.join(HERE, "..", "..", "..", "docs", "ui-api.md"))
METHODS = ("GET", "POST", "PUT", "PATCH", "DELETE")
WILDCARDS = {"field", "name", "reason", "column"}  # `values: { field: [] }`: any name


class Row:
    def __init__(self, section, paths, methods, notes, shape, columns):
        self.section = section
        self.paths = paths  # concrete patterns, e.g. /api/v1/cars/{car}/drives
        self.methods = methods
        self.notes = notes
        self.shape = shape  # what GET answers, or None when the row documents no shape
        self.columns = columns  # the shape is a list of columns (t_ms plus values)
        self.writes = {}  # {method: shape} of write answers the notes spell out

    def __repr__(self):
        return f"Row({self.paths}, {self.methods})"


# ------------------------------------------------------------------ tokens and shapes

TOKEN = re.compile(r'\s*(\.\.\.|"[^"]*"|\\\||[{}\[\](),:+|]|[A-Za-z_][A-Za-z0-9_]*\??|[^\s{}\[\](),:+|"]+)')


def tokens(text):
    out = []
    pos = 0
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m or m.end() == pos:
            break
        out.append(m.group(1))
        pos = m.end()
    return out


def is_ident(t):
    return re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*\??", t or "") is not None


class Parser:
    def __init__(self, text, types):
        self.t = tokens(text)
        self.i = 0
        self.types = types

    def peek(self, k=0):
        j = self.i + k
        return self.t[j] if j < len(self.t) else None

    def take(self):
        tok = self.peek()
        self.i += 1
        return tok

    def skip_group(self):
        """Skip a bracketed group starting at the current token."""
        pairs = {"(": ")", "{": "}", "[": "]"}
        depth = []
        while self.peek() is not None:
            tok = self.take()
            if tok in pairs:
                depth.append(pairs[tok])
            elif depth and tok == depth[-1]:
                depth.pop()
                if not depth:
                    return

    def value(self):
        tok = self.peek()
        if tok == "{":
            obj = self.obj()
            if self.peek() == "each" and self.peek(1) == "{":
                self.take()
                each = self.obj()
                obj = {"keys": {k: each for k in obj["keys"]}}
            return obj
        if tok == "[":
            self.take()
            inner = None
            if self.peek() == "{":
                inner = self.value()
            elif self.peek() in self.types:
                inner = self.types[self.take()]
            while self.peek() not in (None, "]"):
                if self.peek() in ("(", "{", "["):
                    self.skip_group()
                else:
                    self.take()
            self.take()
            return {"list": inner}
        if tok in self.types:
            self.take()
            return self.types[tok]
        return None

    def members(self, end):
        keys = {}
        while self.peek() is not None and self.peek() != end:
            tok = self.peek()
            if tok in (",", "+"):
                self.take()
                continue
            if tok == "(":
                self.skip_group()
                continue
            if tok == "...":
                self.take()
                continue
            if is_ident(tok):
                name = self.take()
                sub = None
                if self.peek() == ":":
                    self.take()
                    sub = self.value()
                if not name.endswith("?"):
                    keys[name] = sub
                # anything else up to the next member (unions, notes) is skipped
                while self.peek() not in (None, ",", "+", end):
                    if self.peek() in ("(", "{", "["):
                        self.skip_group()
                    else:
                        self.take()
                continue
            self.take()
        return keys

    def obj(self):
        self.take()  # {
        keys = self.members("}")
        self.take()  # }
        if len(keys) == 1 and next(iter(keys)) in WILDCARDS:
            return {"keys": {}, "wildcard": True}
        return {"keys": keys}

    def top(self):
        """A whole code span: an object (and more members after it), a list, or a bare member list."""
        if self.peek() == "{":
            out = self.value()
            if self.peek() == ",":
                more = self.members(None)
                out = {"keys": dict(out["keys"], **more)}
            return out
        if self.peek() == "[":
            return self.value()
        return {"keys": self.members(None)}


def merge(a, b):
    if a is None:
        return b
    if b is None:
        return a
    out = {"keys": dict(a.get("keys", {}))}
    for k, v in b.get("keys", {}).items():
        out["keys"][k] = merge(out["keys"].get(k), v) if k in out["keys"] else v
    return out


# ------------------------------------------------------------------ rows


def _cells(line):
    return [c.strip() for c in re.split(r"(?<!\\)\|", line.strip())[1:-1]]


def _paths(cell):
    out = []
    for p in re.findall(r"`([^`]+)`", cell):
        if not p.startswith("/api/") and p != "/health":
            p = "/api/v1" + p
        m = re.fullmatch(r"(.*)\[(/\{\w+\})\]", p)
        if m:
            out += [m.group(1), m.group(1) + m.group(2)]
        else:
            out.append(p)
    return out


def _last_method(text):
    found = re.findall(r"\b(GET|POST|PUT|PATCH|DELETE)\b", text)
    return found[-1] if found else None


def _response(notes, types, has_get):
    """(shape, columns flag, alias paths) of what GET answers, from the notes."""
    shape, columns, aliases = None, False, []
    last_tree = None
    base = None
    m = re.match(r"\s*(\w+) plus\b", notes)
    if m and m.group(1) in types:
        base = types[m.group(1)]
    for sm in re.finditer(r"`([^`]+)`", notes):
        span = sm.group(1)
        before = notes[:sm.start()]
        prev = before.rstrip()
        if span.startswith("/api/v1/"):
            aliases.append(span)
            continue
        if span.startswith("?") or span.startswith("/") or span.endswith("="):
            continue
        method = _last_method(before)
        if method not in (None, "GET") and not prev.endswith("->"):
            continue
        if not has_get:
            continue
        tree = Parser(span, types).top()
        if prev.endswith("every value"):
            continue
        if prev.endswith("each") and last_tree is not None:
            # `config: {...}`, each `{ value, source }`: the shape of each member of the last object
            for k, v in reversed(list(last_tree["keys"].items())):
                if isinstance(v, dict) and v.get("keys"):
                    last_tree["keys"][k] = {"keys": {c: tree for c in v["keys"]}}
                    break
            continue
        is_cols = bool(re.search(r"columns\s*$", prev))
        is_item = bool(re.search(r"\b(Item|Row):\s*$", prev))
        explicit = prev.endswith("->") or is_cols or prev.endswith(":") or prev.endswith("plus") or \
            prev.endswith("and") or is_item or prev.endswith("GET") or re.search(r"-> rows\s*$", prev)
        bare_keys = not span.startswith(("{", "[")) and span.count(",") >= 3
        if not explicit and not span.startswith(("{", "[")) and not bare_keys:
            continue
        if is_item:
            tree = {"keys": {"items": {"list": tree}}}
        if is_cols:
            columns = True
        if shape is not None and not (prev.endswith("plus") or prev.endswith("and")):
            if not explicit:
                continue
        shape = merge(shape, tree) if tree.get("keys") is not None or "list" not in tree else tree
        if "list" in tree and "keys" not in tree:
            shape = tree
        last_tree = shape if "keys" in shape else last_tree
    if base is not None:
        shape = merge(base, shape)
    return shape, columns, aliases


def write_shapes(notes, types, methods):
    """{method: shape} for the answers of writes the notes spell out (`POST {...} -> {...}`)."""
    out = {}
    writes = [m for m in methods if m != "GET"]
    for sm in re.finditer(r"`([^`]+)`", notes):
        before = notes[:sm.start()].rstrip()
        if not before.endswith("->") or sm.group(1).startswith(("/", "?")):
            continue
        method = _last_method(before)
        if method is None and len(writes) == 1 and "GET" not in methods:
            method = writes[0]
        if method in writes:
            out[method] = Parser(sm.group(1), types).top()
    return out


def read(path=DOC):
    """Every resource row of the document, in order, plus the named types (Drive, Charge)."""
    with open(path, encoding="utf-8") as f:
        lines = f.read().splitlines()
    rows, types = [], {}
    # named types first: a list row may come before the row that defines its items (Drive, Charge)
    for line in lines:
        if line.startswith("## Deliberately absent"):
            break
        if line.startswith("| ") and not line.startswith("| `") and not line.startswith("| Resource"):
            cells = _cells(line)
            spans = re.findall(r"`([^`]+)`", cells[2]) if len(cells) >= 3 else []
            if spans:
                types[cells[0].strip()] = Parser(spans[0], types).top()
    section = None
    for line in lines:
        if line.startswith("## "):
            section = line[3:].strip()
            if section in ("Deliberately absent", "Decisions"):
                break
            continue
        if not line.startswith("| ") or line.startswith("| Resource") or line.startswith("|---"):
            continue
        cells = _cells(line)
        if len(cells) < 3:
            continue
        res, meth, notes = cells[0], cells[1], cells[2]
        if not res.startswith("`"):  # a named type, e.g. Drive (read above)
            continue
        methods = [m for m in METHODS if re.search(rf"\b{m}\b", meth)]
        shape, columns, aliases = _response(notes, types, "GET" in methods)
        paths = _paths(res)
        row = Row(section, paths, methods, notes, shape, columns)
        row.writes = write_shapes(notes, types, methods)
        rows.append(row)
        for a in aliases:
            alias = Row(section, [a], ["GET"] if "GET" in methods else methods, notes, shape, columns)
            alias.writes = {}
            rows.append(alias)
    # "GET lists the imports (... same fields as one import)": the list's rows have the shape of the next row
    for i, row in enumerate(rows):
        if row.shape is None and "GET" in row.methods and re.search(r"same fields as one \w+", row.notes) \
                and i + 1 < len(rows) and rows[i + 1].shape is not None:
            row.shape = {"keys": {"items": {"list": rows[i + 1].shape}, "next": None}}
    return rows, types


def missing_fields(shape, value, path="", columns=False):
    """Field paths the shape names that `value` lacks (null values are not looked into)."""
    out = []
    if shape is None or value is None:
        return out
    if "list" in shape and "keys" not in shape:
        if isinstance(value, dict) and isinstance(value.get("items"), list):
            value = value["items"]
        if isinstance(value, list):
            for v in value[:3]:
                out += missing_fields(shape["list"], v, path + "[]")
        return out
    if shape.get("wildcard") or not isinstance(value, dict):
        return out
    keys = shape.get("keys", {})
    items = value.get("items") if isinstance(value.get("items"), list) else None
    for k, sub in keys.items():
        if k in value:
            v = value[k]
            if isinstance(sub, dict) and "list" in sub and "keys" not in sub:
                if isinstance(v, list):
                    for x in v[:3]:
                        out += missing_fields(sub["list"], x, f"{path}{k}[].")
            else:
                out += missing_fields(sub, v, f"{path}{k}.")
        elif columns and isinstance(value.get("values"), dict) and k in value["values"]:
            continue
        elif items is not None and path == "":
            # a list resource: the fields are its rows' fields
            if not items:
                continue
            for x in items[:3]:
                if not isinstance(x, dict) or k not in x:
                    out.append(f"items[].{k}")
                    break
                out += missing_fields(sub, x[k], f"items[].{k}.")
        else:
            out.append(path + k)
    return out
