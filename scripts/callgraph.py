#!/usr/bin/env python3
"""Call-graph validation for the refactor.

  callgraph.py build <index.scip> <src_root> <out.json>   # graph + reachability facts
  callgraph.py diff <before.json> <after.json> <out.md>   # what reachable behaviour disappeared

The index comes from `rust-analyzer scip <root>`. Edges are references to functions inside a
function's enclosing range, plus trait-method -> impl-method dispatch edges. Roots are every
`main` in a binary target plus impls of external traits (Drop, serde, iced Widget, ...), which
are called from outside the workspace. Test code is excluded. Facts are what a refactor may
rename around but must not lose: external API symbols reached, and identifier-like string
literals inside reachable functions (tool names, wire keys, CLI flags, config keys, ALPNs).

Typical use: index the pre-refactor commit and the refactored branch, build both graphs, then
diff them and triage every lost external API or literal as an intended removal or a regression.
"""
import json, re, sys, os
from collections import defaultdict, deque

# ---------- minimal protobuf decoding ----------
def varint(b, i):
    r = s = 0
    while True:
        x = b[i]; i += 1
        r |= (x & 0x7F) << s; s += 7
        if x < 0x80: return r, i

def fields(b):
    i, n = 0, len(b)
    while i < n:
        key, i = varint(b, i)
        f, wt = key >> 3, key & 7
        if wt == 0:
            v, i = varint(b, i)
        elif wt == 2:
            l, i = varint(b, i); v = b[i:i + l]; i += l
        elif wt == 1:
            v = b[i:i + 8]; i += 8
        elif wt == 5:
            v = b[i:i + 4]; i += 4
        else:
            raise ValueError(f"wire type {wt}")
        yield f, wt, v

def packed(v):
    out, i = [], 0
    while i < len(v):
        x, i = varint(v, i); out.append(x)
    return out

def norm_range(r):
    return (r[0], r[1], r[2], r[3]) if len(r) == 4 else (r[0], r[1], r[0], r[2])

def parse_occ(b):
    o = {"range": None, "symbol": "", "roles": 0, "enc": None}
    for f, wt, v in fields(b):
        if f == 1: o["range"] = norm_range(packed(v) if wt == 2 else [v])
        elif f == 2: o["symbol"] = v.decode()
        elif f == 3: o["roles"] = v
        elif f == 7: o["enc"] = norm_range(packed(v))
    return o

def parse_info(b):
    s = {"symbol": "", "kind": 0, "impls": [], "enclosing": ""}
    for f, wt, v in fields(b):
        if f == 1: s["symbol"] = v.decode()
        elif f == 5: s["kind"] = v
        elif f == 8: s["enclosing"] = v.decode()
        elif f == 4:
            rel = {"symbol": "", "impl": False}
            for g, _, w in fields(v):
                if g == 1: rel["symbol"] = w.decode()
                elif g == 3: rel["impl"] = bool(w)
            if rel["impl"]: s["impls"].append(rel["symbol"])
    return s

def load(path):
    data = open(path, "rb").read()
    docs = []
    for f, _, v in fields(data):
        if f != 2: continue
        d = {"path": "", "occ": [], "info": []}
        for g, _, w in fields(v):
            if g == 1: d["path"] = w.decode()
            elif g == 2: d["occ"].append(parse_occ(w))
            elif g == 3: d["info"].append(parse_info(w))
        docs.append(d)
    return docs

# ---------- graph ----------
def is_fn(sym):
    return sym.endswith(").") and not sym.startswith("local ")

def pkg(sym):
    p = sym.split(" ")
    return p[2] if len(p) > 3 else "?"

def short(sym):
    p = sym.split(" ", 4)
    return f"{p[2]}::{p[4]}" if len(p) == 5 else sym

def is_test(sym, path):
    d = sym.split(" ", 4)[-1]
    return ("/tests/" in path or path.startswith("tests/") or path.endswith("tests.rs")
            or "/tests/" in "/" + d or "testkit" in path or "/benches/" in path or "/examples/" in path)

def inside(r, enc):
    return (enc[0], enc[1]) <= (r[0], r[1]) and (r[2], r[3]) <= (enc[2], enc[3])

LIT = re.compile(r'"((?:[^"\\\n]|\\.){3,60})"')
IDENT_LIT = re.compile(r'^[A-Za-z0-9_][A-Za-z0-9_./:@-]*$')

def build(scip_path, root, out):
    docs = load(scip_path)
    defs = {}                # fn symbol -> (path, enclosing range)
    impls = defaultdict(set) # trait method -> impl methods
    workspace = set()
    ext_impl = set()
    for d in docs:
        for s in d["info"]:
            workspace.add(pkg(s["symbol"]))
            for t in s["impls"]:
                if is_fn(t) and is_fn(s["symbol"]):
                    impls[t].add(s["symbol"])
                    ext_impl.add((s["symbol"], t))
        for o in d["occ"]:
            if o["roles"] & 1 and is_fn(o["symbol"]):
                defs[o["symbol"]] = (d["path"], o["enc"] or o["range"])
    if not any(v[1] != None for v in defs.values()):
        sys.exit("index has no enclosing ranges")
    have_enc = sum(1 for d in docs for o in d["occ"] if o["roles"] & 1 and o["enc"])
    edges = defaultdict(set)
    ext = defaultdict(set)
    by_file = defaultdict(list)
    for sym, (p, enc) in defs.items():
        by_file[p].append((sym, enc))
    for d in docs:
        fns = sorted(by_file.get(d["path"], []), key=lambda x: (x[1][0], x[1][1]))
        for o in d["occ"]:
            if o["roles"] & 1 or not o["symbol"] or o["symbol"].startswith("local "):
                continue
            # innermost enclosing fn
            owner = None
            for sym, enc in fns:
                if inside(o["range"], enc):
                    if owner is None or inside(enc, defs[owner][1]):
                        owner = sym
            if owner is None:
                continue
            if pkg(o["symbol"]) in workspace:
                if is_fn(o["symbol"]): edges[owner].add(o["symbol"])
                else: edges[owner].add(o["symbol"])  # types: lets impls of external traits become reachable
            else:
                ext[owner].add(short(o["symbol"]))
    for t, ims in impls.items():
        edges[t] |= ims
    # type -> its methods (methods of external-trait impls are invoked by libraries: iced, serde, std)
    methods_of = defaultdict(set)
    for sym in defs:
        m = re.match(r"^(.*?)#[^#]*\)\.$", sym)
        if m: methods_of[m.group(1) + "#"].add(sym)
        m = re.search(r"impl#\[([^\]]+)\]", sym)
        if m:
            methods_of[m.group(1)].add(sym)
    roots = [s for s, (p, _) in defs.items()
             if re.search(r"(^|/)main\(\)\.$", s.split(" ", 4)[-1]) or s.endswith(" main().")]
    # methods implementing a trait from outside the workspace are called by that library
    roots += [m for m, t in ext_impl if pkg(t) not in workspace and m in defs and not is_test(m, defs[m][0])]
    seen, q = set(roots), deque(roots)
    while q:
        f = q.popleft()
        nxt = set(edges.get(f, ()))
        if not is_fn(f):
            key = f.split(" ", 4)[-1].rstrip("#")
            for k, ms in methods_of.items():
                if k.rstrip("#").endswith(key.split("/")[-1]) and key.split("/")[-1]:
                    nxt |= ms
        for n in nxt:
            if n not in seen:
                seen.add(n); q.append(n)
    reach = {s for s in seen if is_fn(s) and s in defs}
    # literals inside reachable fns
    lits = defaultdict(set)
    src_cache = {}
    for sym in reach:
        p, enc = defs[sym]
        if p not in src_cache:
            try: src_cache[p] = open(os.path.join(root, p), errors="ignore").read().split("\n")
            except OSError: src_cache[p] = []
        lines = src_cache[p][enc[0]:enc[2] + 1]
        for m in LIT.finditer("\n".join(lines)):
            s = m.group(1)
            if IDENT_LIT.match(s): lits[s].add(short(sym))
    crate_of = lambda s: pkg(s)
    crate_edges = defaultdict(set)
    for a, bs in edges.items():
        for b in bs:
            if is_fn(a) and crate_of(a) != crate_of(b) and crate_of(b) in workspace:
                crate_edges[crate_of(a)].add(crate_of(b))
    all_fns = [s for s, (p, _) in defs.items() if not is_test(s, p)]
    dead = sorted(short(s) for s in all_fns if s not in reach)
    ext_reach = defaultdict(set)
    for f in reach:
        for e in ext.get(f, ()): ext_reach[e].add(short(f))
    res = {
        "roots": sorted(short(r) for r in roots if r.endswith("main().")),
        "functions": len(all_fns), "reachable": len(reach), "enclosing_defs": have_enc,
        "unreachable": dead,
        "external_reached": {k: sorted(v)[:3] for k, v in ext_reach.items()},
        "literals": {k: sorted(v)[:3] for k, v in lits.items()},
        "crate_edges": {k: sorted(v) for k, v in crate_edges.items()},
    }
    json.dump(res, open(out, "w"), indent=1)
    print(f"{len(all_fns)} non-test fns, {len(reach)} reachable from {len(roots)} roots, "
          f"{len(dead)} unreachable, {len(ext_reach)} external symbols, {len(lits)} literals")

def ext_key(s):
    # compare external APIs by crate + last path segments; versions differ between runs
    p = s.split("::", 1)
    return (p[0], re.sub(r"\[[^\]]*\]", "", p[1]) if len(p) > 1 else "")

def diff(a_path, b_path, out):
    a, b = json.load(open(a_path)), json.load(open(b_path))
    ae = {ext_key(k): k for k in a["external_reached"]}
    be = {ext_key(k) for k in b["external_reached"]}
    lost_ext = sorted((ae[k], a["external_reached"][ae[k]]) for k in ae if k not in be)
    lost_lit = sorted((k, v) for k, v in a["literals"].items() if k not in b["literals"])
    w = open(out, "w")
    w.write(f"# Call-graph diff\n\n| | before | after |\n|---|---:|---:|\n")
    for k in ("functions", "reachable"):
        w.write(f"| {k} | {a[k]} | {b[k]} |\n")
    w.write(f"| unreachable (non-test) | {len(a['unreachable'])} | {len(b['unreachable'])} |\n\n")
    w.write(f"## External APIs no longer reachable from main ({len(lost_ext)})\n\n")
    for k, v in lost_ext: w.write(f"- `{k}` — was used by {', '.join(v)}\n")
    w.write(f"\n## Identifier literals no longer in reachable code ({len(lost_lit)})\n\n")
    for k, v in lost_lit: w.write(f"- `{k}` — was in {', '.join(v)}\n")
    w.write(f"\n## Unreachable functions after ({len(b['unreachable'])})\n\n")
    for s in b["unreachable"]: w.write(f"- `{s}`\n")
    w.write("\n## Crate edges after\n\n")
    for k, v in sorted(b["crate_edges"].items()): w.write(f"- {k} -> {', '.join(v)}\n")
    print(f"lost external APIs: {len(lost_ext)}, lost literals: {len(lost_lit)}, "
          f"unreachable after: {len(b['unreachable'])}")

if __name__ == "__main__":
    cmds = {"build": build, "diff": diff}
    if len(sys.argv) != 5 or sys.argv[1] not in cmds:
        sys.exit(__doc__)
    cmds[sys.argv[1]](*sys.argv[2:5])
