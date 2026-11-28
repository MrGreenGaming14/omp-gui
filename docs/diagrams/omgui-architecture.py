"""Draws docs/diagrams/omgui-architecture.svg (run: python3 docs/diagrams/omgui-architecture.py)."""
import pathlib

W, H = 1490, 1150
FLOW = {"ui": "#7e57c2", "ctl": "#c26a00", "data": "#1f8a70", "port": "#3b6fb6", "ext": "#4d8a2f"}
LAYER = {"pres": ("#ede4f5", "#9673a6"), "view": ("#dfeafb", "#3b6fb6"), "ctl": ("#e8edf3", "#6b7c93"),
         "run": ("#fde7cf", "#d38b1f"), "set": ("#d6f1e8", "#2d8a6a"), "dom": ("#eeeeee", "#7a7a7a"),
         "adp": ("#fff3c4", "#c9a227"), "ext": ("#d9ecd0", "#5a8f3c")}
o = []
A = o.append


def container(x, y, w, h, title, layer, hh=28):
    f, s = LAYER[layer]
    A(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="4" fill="#fff" stroke="{s}" stroke-width="1.2"/>')
    A(f'<rect x="{x}" y="{y}" width="{w}" height="{hh}" rx="4" fill="{f}" stroke="{s}" stroke-width="1.2"/>')
    A(f'<text x="{x + 12}" y="{y + hh / 2 + 5}" font-size="13" font-weight="bold" fill="#1d1d1f">{title}</text>')


def box(x, y, w, h, lines, layer, bold=False, fs=12.5, sw=1):
    f, s = LAYER[layer]
    A(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" fill="{f}" stroke="{s}" stroke-width="{sw}"/>')
    n = len(lines)
    cy = y + h / 2 - (n - 1) * 8 + 4
    for i, l in enumerate(lines):
        A(f'<text x="{x + w / 2}" y="{cy + i * 16}" text-anchor="middle" font-size="{fs if i == 0 else fs - 1.5}" '
          f'font-weight="{"bold" if bold and i == 0 else "normal"}" fill="{"#1d1d1f" if i == 0 else "#555"}">{l}</text>')


def ellipse(cx, cy, rx, ry, lines):
    A(f'<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="#f8cecc" stroke="#b85450" stroke-width="1.4"/>')
    for i, l in enumerate(lines):
        A(f'<text x="{cx}" y="{cy - 4 + i * 16}" text-anchor="middle" font-size="{13.5 if i == 0 else 11.5}" '
          f'font-weight="{"bold" if i == 0 else "normal"}" fill="#1d1d1f">{l}</text>')


def arrow(pts, kind, dashed=False, hollow=False):
    d = "M" + " L".join(f"{a},{b}" for a, b in pts)
    dash = 'stroke-dasharray="5 3"' if dashed else ""
    A(f'<path d="{d}" fill="none" stroke="{FLOW[kind]}" stroke-width="1.4" {dash} marker-end="url(#{"h" if hollow else "m"}{kind})"/>')


def line(pts, kind):
    d = "M" + " L".join(f"{a},{b}" for a, b in pts)
    A(f'<path d="{d}" fill="none" stroke="{FLOW[kind]}" stroke-width="1.4"/>')


def lab(x, y, t, kind="ctl", anchor="middle", size=11):
    w = len(t) * 5.9 + 8
    bx = x - w / 2 if anchor == "middle" else (x - 4 if anchor == "start" else x - w + 4)
    A(f'<rect x="{bx}" y="{y - 11}" width="{w}" height="15" rx="3" fill="#fff"/>'
      f'<text x="{x}" y="{y}" text-anchor="{anchor}" font-size="{size}" fill="{FLOW[kind]}" font-weight="bold">{t}</text>')


def vlab(x, y, t, kind):
    w = len(t) * 5.9 + 10
    A(f'<g transform="translate({x},{y}) rotate(-90)"><rect x="{-w / 2}" y="-8" width="{w}" height="15" rx="3" fill="#fff"/>'
      f'<text x="0" y="4" text-anchor="middle" font-size="11" font-weight="bold" fill="{FLOW[kind]}">{t}</text></g>')


A(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="Helvetica, Arial, sans-serif">')
A("<defs>" + "".join(
    f'<marker id="m{k}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="{c}"/></marker>'
    f'<marker id="h{k}" viewBox="0 0 12 12" refX="11" refY="6" markerWidth="10" markerHeight="10" orient="auto-start-reverse"><path d="M1,1 L11,6 L1,11 z" fill="#fff" stroke="{c}" stroke-width="1.3"/></marker>'
    for k, c in FLOW.items()) + "</defs>")
A(f'<rect width="{W}" height="{H}" fill="#fff"/>')
A('<text x="635" y="30" text-anchor="middle" font-size="19" font-weight="bold" fill="#1d1d1f">omgui — target architecture</text>')
box(535, 46, 200, 40, ["User"], "ext", bold=True, fs=14)
A('<rect x="30" y="104" width="1210" height="1030" rx="6" fill="#fff" stroke="#3b6fb6" stroke-width="1.4"/>')
A('<rect x="30" y="104" width="1210" height="30" rx="6" fill="#dfeafb" stroke="#3b6fb6" stroke-width="1.4"/>')
A('<text x="45" y="124" font-size="14" font-weight="bold">omgui desktop app — cpp/app, loa-qt (composition root)</text>')

# Presentation + view API
container(55, 148, 1160, 140, "Presentation — display only  ·  loa-ui-*", "pres")
for i, (t, sub) in enumerate([("Settings &amp; Configs screens", "loa-ui-settings"),
                              ("Session view &amp; composer", "loa-ui-session · loa-ui-panel"),
                              ("Shell · terminal · surfaces", "loa-ui-shell · ui-terminal · ui-surface")]):
    box(75 + i * 382, 186, 362, 46, [t, sub], "pres")
box(75, 244, 1126, 32, ["UI kit — widgets · style · theme  (loa-ui-kit)"], "pres", fs=12)
box(55, 306, 1160, 46, ["View API — loa-view  ·  stable, versioned contract",
                        "view models · Intent enums · transcript reads the session DOM · media via decoder ports"], "view", bold=True, sw=1.8)

# Feature controllers: one row per area, aligned with what it drives
ROWS = ["surfaces", "tools", "sandbox", "peers", "models", "projects", "updater", "agents", "harness", "sessions", "decisions"]
R = lambda i: 482 + i * 40


def ry(n):
    i = ROWS.index(n)
    return R(i) if i <= 1 else R(i) + 40


container(590, 376, 300, 612, "Feature controllers · loa-app/areas", "ctl")
box(605, 414, 270, 54, ["Ctx — shared context", "runtime · services · requests · Area trait"], "ctl", fs=12)
# Secondary dependencies confirmed by the call graph, beyond the row's own adapter/service.
ALSO = {"surfaces": "+ mcp", "tools": "+ trust", "peers": "+ mcp", "updater": "+ peer network",
        "agents": "+ omp agent files, mcp", "sessions": "+ projects, plan", "decisions": "+ plan check"}
for n in ROWS:
    h = 72 if n == "tools" else 32
    box(605, ry(n), 270, h, [n], "ctl", bold=True)
    if n in ALSO:
        A(f'<text x="868" y="{ry(n) + h / 2 + 4}" text-anchor="end" font-size="9.5" fill="#6b7c93">{ALSO[n]}</text>')
A('<text x="740" y="982" text-anchor="middle" font-size="10" fill="#6b7c93">each area: own folder + one AREAS entry — expected to change</text>')

# Central settings
container(55, 376, 505, 200, "Central settings  ·  loa-settings", "set")
box(75, 414, 225, 74, ["Harness config ports", "OmpRpc · account", "files · validator"], "set", bold=True)
box(315, 414, 225, 74, ["App config", "Config · ConfigStore"], "set")
box(75, 496, 225, 66, ["Model &amp; provider settings", "models.yml · omp config · agents · sign-in"], "set")
box(315, 496, 225, 66, ["Project &amp; updater settings", "pins · digests · release status"], "set")
arrow([(603, 441), (542, 441)], "data")
lab(572, 435, "load · save", "data", size=9.5)

# Domain services, each next to the area that drives it
container(55, 594, 505, 206, "Domain services", "dom")
for n, t in [("peers", "Trust  ·  identity · grants · authorizes peer edits"), ("models", "Models  ·  backend"),
             ("projects", "Projects  ·  digests"), ("updater", "Updater  ·  gates · peer standing")]:
    y = ry(n)
    box(75, y, 465, 32, [t], "dom", fs=12)
    arrow([(603, y + 16), (542, y + 16)], "ctl")
arrow([(300, 594), (300, 578)], "data")
lab(350, 590, "persist · calls ports", "data", anchor="start", size=9.5)

# Integration adapters + externals
container(920, 446, 295, 530, "Integration adapters", "adp")
for t, y in [("Remote surfaces · loa-surfaces", ry("surfaces")), ("Remote hosts · loa-remote", R(1)),
             ("MCP client + tool server · loa-mcp", R(1) + 40), ("Sandbox · loa-sandbox", ry("sandbox")),
             ("Peer network · loa-p2p", ry("peers"))]:
    box(935, y, 265, 32, [t], "adp", fs=11.5)
hy0, hy1 = ry("models"), ry("decisions") + 32
A(f'<rect x="935" y="{hy0}" width="265" height="{hy1 - hy0}" rx="6" fill="#fff3c4" stroke="#c9a227"/>')
A(f'<text x="1067" y="{hy0 + 18}" text-anchor="middle" font-size="12.5" font-weight="bold">Harness adapters · loa-harness</text>')
for i, hn in enumerate(["omp", "omp2", "claude code", "codex", "opencode", "acp"]):
    box(948 + (i % 2) * 124, hy0 + 30 + (i // 2) * 28, 116, 22, [hn], "adp", fs=11)
A(f'<text x="1067" y="{hy0 + 130}" text-anchor="middle" font-size="10.5" fill="#6b5b16">accounts · native session codecs</text>')
for n in ["surfaces", "sandbox", "peers"]:
    arrow([(877, ry(n) + 16), (933, ry(n) + 16)], "ctl")
arrow([(877, R(1) + 16), (933, R(1) + 16)], "ctl")
arrow([(877, R(1) + 56), (933, R(1) + 56)], "ctl")
lab(905, R(1) + 11, "remote", size=9)
lab(905, R(1) + 51, "servers", size=9)
for n, t in [("harness", "probe"), ("sessions", "import · summarize")]:
    arrow([(877, ry(n) + 16), (933, ry(n) + 16)], "ctl")
    lab(905, ry(n) + 11, t, size=9)
arrow([(1200, R(1) + 56), (1208, R(1) + 56), (1208, hy0 + 10), (1202, hy0 + 10)], "ctl")
A(f'<text x="1204" y="{(R(1) + 56 + hy0) / 2}" text-anchor="end" font-size="9" font-weight="bold" fill="{FLOW["ctl"]}">host tools</text>')
for t, y, p in [("Browsers &amp; displays", ry("surfaces"), "stream"), ("Remote machines", R(1), "SSH"),
                ("MCP servers", R(1) + 40, "MCP"), ("VMs / containers", ry("sandbox"), "provision"),
                ("Peer omgui nodes", ry("peers"), "QUIC")]:
    box(1262, y, 200, 32, [t], "ext", bold=True, fs=12)
    arrow([(1200, y + 16), (1260, y + 16)], "ext")
    lab(1232, y + 11, p, "ext", size=9)
hc = (hy0 + hy1) / 2
box(1262, hc - 16, 200, 32, ["Agent CLIs"], "ext", bold=True, fs=12)
arrow([(1200, hc), (1260, hc)], "ext")
lab(1232, hc - 5, "stdio / RPC", "ext", size=9)

# Headless runtime
container(55, 818, 505, 200, "Headless runtime  ·  no Qt", "run")
box(75, 856, 200, 46, ["Session library", "+ live writer (loa-session · loa-bus)"], "run", fs=12)
box(75, 916, 200, 32, ["Live feeds"], "run", fs=11.5)
ellipse(420, 900, 125, 40, ["Session lifecycle", "loa-runtime state machine"])
arrow([(295, 890), (277, 890)], "data")
arrow([(175, 902), (175, 914)], "data")
arrow([(275, 932), (320, 932), (335, 920)], "data", dashed=True)
arrow([(603, 905), (547, 905)], "ctl")
lab(575, 918, "Command", size=9)
arrow([(530, 878), (603, 878)], "ctl", dashed=True)
lab(575, 872, "Transition", size=9)
yd = ry("decisions")
arrow([(640, yd + 32), (640, 962), (455, 962), (455, 941)], "ctl")
lab(560, 962, "answer via session", size=9.5)
arrow([(505, 928), (505, 1000), (1000, 1000), (1000, hy1 + 2)], "ctl")
lab(760, 995, "start · send · interrupt", size=10)
arrow([(1100, hy1 + 2), (1100, 1024), (470, 1024), (470, 941)], "ctl", dashed=True)
lab(760, 1036, "session events", size=10)

# Journal store, release feeds, settings ports
box(935, 1050, 265, 32, ["DOM journal store · loa-bus"], "adp", fs=11.5)
box(1262, 1050, 200, 32, ["Local disk"], "ext", bold=True, fs=12)
arrow([(1200, 1066), (1260, 1066)], "ext")
lab(1232, 1061, "append", "ext", size=9)
arrow([(75, 870), (62, 870), (62, 1066), (933, 1066)], "data")
lab(300, 1061, "persist every patch (Persist port)", "data", size=10)
box(1262, 1092, 200, 32, ["Release feeds · GitHub"], "ext", bold=True, fs=12)
arrow([(75, ry("updater") + 16), (48, ry("updater") + 16), (48, 1108), (1260, 1108)], "ext")
lab(700, 1104, "HTTPS", "ext", size=10)
arrow([(1200, hy1 - 12), (1226, hy1 - 12), (1226, 1124), (36, 1124), (36, 451), (73, 451)], "port", dashed=True, hollow=True)
lab(700, 1128, "implements settings ports", "port", size=10)

# UI flow
arrow([(635, 86), (635, 146)], "ui")
lab(643, 98, "input", "ui", anchor="start")
arrow([(55, 209), (20, 209), (20, 66), (533, 66)], "ui", dashed=True)
vlab(20, 140, "renders", "ui")
arrow([(400, 306), (400, 290)], "ui", dashed=True)
lab(340, 299, "view models", "ui")
arrow([(870, 290), (870, 304)], "ui")
lab(920, 299, "intents", "ui")
arrow([(700, 374), (700, 354)], "ui", dashed=True)
lab(640, 366, "project views", "ui")
arrow([(780, 354), (780, 374)], "ui")
lab(835, 366, "map intents", "ui")

# Legend
A('<rect x="1262" y="148" width="200" height="98" rx="6" fill="#fff" stroke="#ccc"/>')
A('<text x="1272" y="164" font-size="11" font-weight="bold" fill="#333">Arrows</text>')
for i, (k, t) in enumerate([("ui", "UI flow"), ("ctl", "control / calls"), ("data", "data / persistence"),
                            ("port", "implements a port"), ("ext", "external protocol")]):
    yy = 178 + i * 14
    A(f'<line x1="1272" y1="{yy}" x2="1302" y2="{yy}" stroke="{FLOW[k]}" stroke-width="2"/>'
      f'<text x="1310" y="{yy + 4}" font-size="10.5" fill="#333">{t}</text>')
A('<text x="1262" y="270" font-size="10" fill="#777">Foundation used by every library (omitted):</text>'
  '<text x="1262" y="284" font-size="10" fill="#777">loa::contracts · loa::core</text>')
A("</svg>")

out = pathlib.Path(__file__).with_suffix(".svg")
out.write_text("\n".join(o))
print(out)
