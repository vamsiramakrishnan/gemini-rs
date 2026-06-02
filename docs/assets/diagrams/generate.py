#!/usr/bin/env python3
"""Generate the documentation diagrams as clean, consistent SVGs.

One generator, one palette, one set of primitives -> every diagram in the
docs shares the same visual language. Edit here and re-run to regenerate:

    python3 docs/assets/diagrams/generate.py

Output: docs/assets/diagrams/*.svg
"""
import os

OUT = os.path.dirname(os.path.abspath(__file__))

# ---------------------------------------------------------------------------
# Design tokens
# ---------------------------------------------------------------------------
MONO = "ui-monospace, 'SF Mono', SFMono-Regular, Menlo, Consolas, monospace"
SANS = "system-ui, -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif"

INK = "#0f172a"
MUTED = "#64748b"
ARROW = "#94a3b8"
CARD = "#ffffff"
CARD_BORDER = "#e6eaf0"

# (fill, stroke, text) per semantic kind
PAL = {
    "l2":      ("#ede9fe", "#7c3aed", "#5b21b6"),
    "l1":      ("#dbeafe", "#2563eb", "#1e40af"),
    "l0":      ("#e2e8f0", "#64748b", "#334155"),
    "api":     ("#f1f5f9", "#94a3b8", "#475569"),
    "state":   ("#fef3c7", "#d97706", "#92400e"),
    "fast":    ("#dcfce7", "#16a34a", "#166534"),
    "control": ("#dbeafe", "#2563eb", "#1e40af"),
    "tele":    ("#f3e8ff", "#9333ea", "#6b21a8"),
    "neutral": ("#f8fafc", "#cbd5e1", "#334155"),
    "bad":     ("#fee2e2", "#dc2626", "#991b1b"),
    "ok":      ("#dcfce7", "#16a34a", "#166534"),
    "tool":    ("#e0f2fe", "#0284c7", "#075985"),
}

MARKERS = {  # name -> color
    "arrow": ARROW,
    "arrowd": "#475569",
    "arrowr": "#dc2626",
    "arrowg": "#16a34a",
    "arrowv": "#7c3aed",
}


def esc(s):
    return (s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;"))


def charw(fs):
    return fs * 0.602  # monospace advance approximation


# ---------------------------------------------------------------------------
# Primitives
# ---------------------------------------------------------------------------
def text(x, y, s, fs=12.5, col=MUTED, anchor="middle", weight="400",
         font=MONO, baseline="central", italic=False):
    st = ' font-style="italic"' if italic else ""
    return (f'<text x="{x:.1f}" y="{y:.1f}" font-family="{font}" '
            f'font-size="{fs}" fill="{col}" text-anchor="{anchor}" '
            f'font-weight="{weight}" dominant-baseline="{baseline}"{st}>'
            f'{esc(s)}</text>')


def mtext(cx, cy, lines, fs=12.5, col=INK, weight="400", anchor="middle",
          font=MONO, lh=1.42, x=None):
    """Multi-line text block, vertically centered on cy (anchor x = cx)."""
    if isinstance(lines, str):
        lines = lines.split("\n")
    xx = cx if x is None else x
    n = len(lines)
    line_h = fs * lh
    start = cy - (n - 1) / 2 * line_h
    out = []
    for i, ln in enumerate(lines):
        out.append(text(xx, start + i * line_h, ln, fs=fs, col=col,
                        anchor=anchor, weight=weight, font=font))
    return "".join(out)


def box(x, y, w, h, lines=None, kind="neutral", rx=11, fs=13, title=None,
        title_fs=None, body_fs=None, stripe=False, dashed=False, x_text=None,
        align="middle", pad=14):
    """Rounded card with optional bold title line + muted body lines."""
    fill, stroke, tcol = PAL[kind]
    dash = ' stroke-dasharray="5 4"' if dashed else ""
    parts = [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" '
             f'fill="{fill}" stroke="{stroke}" stroke-width="1.6"{dash}/>']
    if stripe:
        parts.append(f'<path d="M{x},{y+rx} a{rx},{rx} 0 0 1 {rx},-{rx} '
                     f'L{x+6},{y} L{x+6},{y+h} L{x+rx},{y+h} '
                     f'a{rx},{rx} 0 0 1 -{rx},-{rx} Z" fill="{stroke}"/>')
    cx = x + w / 2
    if align == "start":
        tx = x + pad if x_text is None else x_text
        anc = "start"
    else:
        tx = cx if x_text is None else x_text
        anc = "middle"
    if title is not None:
        tfs = title_fs or (fs + 1.5)
        bfs = body_fs or (fs - 0.5)
        body = lines or []
        if isinstance(body, str):
            body = body.split("\n")
        line_h = bfs * 1.42
        total_h = (1 * tfs * 1.5) + len(body) * line_h
        ty = y + h / 2 - total_h / 2 + tfs * 0.75
        parts.append(text(tx, ty, title, fs=tfs, col=tcol, anchor=anc,
                          weight="700", font=SANS))
        by = ty + tfs * 1.2
        for i, ln in enumerate(body):
            parts.append(text(tx, by + i * line_h, ln, fs=bfs, col=tcol,
                              anchor=anc))
    elif lines is not None:
        parts.append(mtext(cx, y + h / 2, lines, fs=fs, col=tcol,
                           anchor=anc, x=(tx if align == "start" else None)))
    return "".join(parts)


def line(x1, y1, x2, y2, color=ARROW, w=2.0, dashed=False):
    dash = ' stroke-dasharray="5 4"' if dashed else ""
    return (f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{color}" stroke-width="{w}"{dash}/>')


def arrow(x1, y1, x2, y2, marker="arrow", w=2.0, dashed=False):
    color = MARKERS[marker]
    dash = ' stroke-dasharray="5 4"' if dashed else ""
    return (f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{color}" stroke-width="{w}" marker-end="url(#{marker})"{dash}/>')


def elbow(pts, marker="arrow", w=2.0, dashed=False):
    color = MARKERS[marker]
    dash = ' stroke-dasharray="5 4"' if dashed else ""
    d = "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in pts)
    return (f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{w}" '
            f'marker-end="url(#{marker})" stroke-linejoin="round"{dash}/>')


def chip(cx, cy, label, kind, fs=12, padx=11, h=24):
    fill, stroke, tcol = PAL[kind]
    w = charw(fs) * len(label) + padx * 2
    return (f'<rect x="{cx-w/2:.1f}" y="{cy-h/2:.1f}" width="{w:.1f}" '
            f'height="{h}" rx="{h/2:.0f}" fill="{fill}" stroke="{stroke}" '
            f'stroke-width="1.3"/>' + text(cx, cy, label, fs=fs, col=tcol,
                                           weight="600")), w


def caption(cx, y, s, fs=12, col=MUTED, italic=True, anchor="middle"):
    return text(cx, y, s, fs=fs, col=col, anchor=anchor, italic=italic,
                font=SANS)


def caption_bg(cx, y, s, fs=11, col=MUTED, italic=True):
    """Centered caption with a white pill behind it (for labels over lines)."""
    w = charw(fs) * len(s) * 0.95 + 14
    return (f'<rect x="{cx-w/2:.1f}" y="{y-9:.1f}" width="{w:.1f}" height="18" '
            f'rx="7" fill="{CARD}"/>' + caption(cx, y, s, fs=fs, col=col,
                                                italic=italic))


def svg(w, h, body, pad_card=True):
    defs = ['<defs>']
    for name, color in MARKERS.items():
        defs.append(f'<marker id="{name}" markerWidth="9" markerHeight="9" '
                    f'refX="7.5" refY="3" orient="auto" '
                    f'markerUnits="userSpaceOnUse">'
                    f'<path d="M0,0 L7.5,3 L0,6 Z" fill="{color}"/></marker>')
    defs.append('</defs>')
    bg = (f'<rect x="1" y="1" width="{w-2}" height="{h-2}" rx="16" '
          f'fill="{CARD}" stroke="{CARD_BORDER}" stroke-width="1.5"/>'
          if pad_card else "")
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
            f'viewBox="0 0 {w} {h}" font-family="{MONO}" role="img">'
            + "".join(defs) + bg + body + "</svg>\n")


def write(name, content):
    path = os.path.join(OUT, name)
    with open(path, "w") as f:
        f.write(content)
    print(f"wrote {name}  ({len(content)} bytes)")


# ---------------------------------------------------------------------------
# Diagrams
# ---------------------------------------------------------------------------
def d_architecture_stack(fname, with_api=False):
    W = 780
    M = 26
    bw = W - 2 * M
    bands = [
        ("l2", "gemini-adk-fluent-rs   ·   L2 — Fluent DX", [
            "Live::builder()  ·  AgentBuilder  ·  S·C·T·P·M·A operators",
            "PhaseBuilder  ·  WatchBuilder  ·  Temporal patterns",
            ".govern(flow)  ·  .extract_record()  ·  .on_enter()"]),
        ("l1", "gemini-adk-rs   ·   L1 — Agent Runtime", [
            "LiveSessionBuilder  ·  LiveHandle  ·  Three-lane processor",
            "State (prefix-scoped)  ·  PhaseMachine  ·  ToolDispatcher",
            "Governed: Flow · Extract · Resolver  ·  TextAgent combinators",
            "LlmAgent · Runner · SessionService · MCP · A2A"]),
        ("l0", "gemini-genai-rs   ·   L0 — Wire Protocol", [
            "Transport (WebSocket + Mock)  ·  Codec (JSON)  ·  Auth providers",
            "SessionHandle  ·  Protocol types  ·  VAD  ·  Jitter buffer",
            "Telemetry (OTel + Prometheus)  ·  REST APIs (feature-gated)"]),
    ]
    heights = [108, 132, 108]
    if with_api:
        bands.append(("api", "Gemini Multimodal Live API", [
            "WebSocket · full-duplex audio + text · server-side VAD"]))
        heights.append(64)
    gap = 30
    y = 26
    body = []
    ys = []
    for (kind, t, ln), hh in zip(bands, heights):
        ys.append((y, hh))
        body.append(box(M, y, bw, hh, ln, kind=kind, title=t, stripe=True,
                        align="start", x_text=M + 22))
        y += hh + gap
    # depends-on arrows between bands
    for i in range(len(ys) - 1):
        y0 = ys[i][0] + ys[i][1]
        y1 = ys[i + 1][0]
        cx = W / 2
        body.append(arrow(cx, y0 + 4, cx, y1 - 4, marker="arrowd", w=1.8))
        lbl = "depends on" if not (with_api and i == len(ys) - 2) else "speaks"
        body.append(f'<rect x="{cx+8}" y="{(y0+y1)/2-9}" width="86" '
                    f'height="18" rx="9" fill="{CARD}"/>')
        body.append(caption(cx + 51, (y0 + y1) / 2, lbl, fs=11))
    H = y - gap + 26
    write(fname, svg(W, H, "".join(body)))


def d_core_concepts(fname):
    W, H = 840, 430
    body = []
    # top builder
    bx, bw, bh = W / 2 - 120, 28, 50
    body.append(box(W / 2 - 120, 26, 240, 50, ["(L2 Fluent API)"],
                    kind="l2", title="Live::builder()"))
    body.append(caption(W / 2, 96, "configures", fs=12))
    # five concept boxes
    labels = [("Phases", "control"), ("Extractors", "control"),
              ("Tools", "tool"), ("Watchers", "control"),
              ("Telemetry", "tele")]
    n = len(labels)
    cw, ch = 132, 46
    gap = (W - 52 - n * cw) / (n - 1)
    xs = [26 + i * (cw + gap) for i in range(n)]
    cy = 132
    # fan from builder
    for x in xs:
        body.append(elbow([(W / 2, 76), (W / 2, 108),
                           (x + cw / 2, 108), (x + cw / 2, cy - 2)],
                          marker="arrow", w=1.6))
    for (lab, kind), x in zip(labels, xs):
        body.append(box(x, cy, cw, ch, kind=kind, fs=14,
                        lines=[lab]))
    # state box
    sw, sh = 360, 76
    sx, sy = 120, 318
    body.append(box(sx, sy, sw, sh, ["(prefix-scoped, concurrent)"],
                    kind="state", title="State"))
    # signals box
    gw, gh = 180, 60
    gx, gy = 560, 326
    body.append(box(gx, gy, gw, gh, ["Signals &", "Counters"],
                    kind="tele", fs=13))
    # first four converge to state
    midy = 246
    for (lab, kind), x in zip(labels[:4], xs[:4]):
        body.append(elbow([(x + cw / 2, cy + ch), (x + cw / 2, midy),
                           (sx + sw / 2, midy), (sx + sw / 2, sy - 2)],
                          marker="arrow", w=1.6))
    # telemetry -> signals -> state
    tx = xs[4] + cw / 2
    body.append(elbow([(tx, cy + ch), (tx, gy - 40), (gx + gw / 2, gy - 40),
                       (gx + gw / 2, gy - 2)], marker="arrowv", w=1.6))
    body.append(arrow(gx - 2, gy + gh / 2, sx + sw + 2, gy + gh / 2,
                      marker="arrow", w=1.8))
    write(fname, svg(W, H, "".join(body)))


def d_state_hierarchy(fname):
    W = 760
    rows = [
        ("app:caller_name = \"Alice\"", "state", "application state"),
        ("session:turn_count = 5", "control", "auto-tracked by SessionSignals"),
        ("session:total_token_count = 1284", "control", "auto-tracked from UsageMetadata"),
        ("derived:risk_level = \"high\"", "tele", "computed variable, read-only"),
        ("turn:transcript = \"I need help\"", "tool", "cleared each turn"),
        ("bg:verification_status = \"pending\"", "l0", "background agent result"),
    ]
    top = 30
    rh = 46
    H = top + 36 + len(rows) * rh + 14
    body = []
    # root
    rx0, rw, rhh = 30, 150, 46
    body.append(box(rx0, top, rw, rhh, kind="state", lines=["State"],
                    fs=16))
    spine_x = rx0 + 34
    spine_top = top + rhh
    last_y = top + 36 + (len(rows) - 1) * rh + rh / 2
    body.append(line(spine_x, spine_top, spine_x, last_y, color=ARROW, w=1.8))
    px = 90
    for i, (kv, kind, note) in enumerate(rows):
        cy = top + 36 + i * rh + rh / 2
        body.append(line(spine_x, cy, px, cy, color=ARROW, w=1.8))
        fs = 12.5
        w = charw(fs) * len(kv) + 22
        fill, stroke, tcol = PAL[kind]
        body.append(f'<rect x="{px}" y="{cy-15}" width="{w:.1f}" height="30" '
                    f'rx="8" fill="{fill}" stroke="{stroke}" stroke-width="1.4"/>')
        body.append(text(px + w / 2, cy, kv, fs=fs, col=tcol, weight="600"))
        body.append(caption(px + w + 16, cy, note, fs=11.5, anchor="start"))
    write(fname, svg(W, H, "".join(body)))


def d_phase_flow(fname):
    W, H = 860, 300
    phases = [
        ("greeting", '"Welcome…"', "—", "caller_name\nis_some()"),
        ("identify_caller", '"Get name…"', "[lookup]", "request_type\nis_some()"),
        ("handle_request", '"Help with…"', "[search, calc]", "resolved\n== true"),
        ("farewell", '"Say goodbye"', "—", "(terminal)"),
    ]
    n = len(phases)
    pw, ph = 178, 54
    gap = (W - 52 - n * pw) / (n - 1)
    xs = [26 + i * (pw + gap) for i in range(n)]
    py = 40
    body = []
    for i, ((name, instr, tools, trans), x) in enumerate(zip(phases, xs)):
        body.append(box(x, py, pw, ph, kind="control", fs=15,
                        lines=[name]))
        if i < n - 1:
            body.append(arrow(x + pw, py + ph / 2, xs[i + 1] - 2, py + ph / 2,
                              marker="arrowd"))
        cx = x + pw / 2
        # detail rows
        ys = py + ph + 30
        body.append(caption(cx, ys, "instruction", fs=11))
        body.append(text(cx, ys + 18, instr, fs=12, col=INK))
        body.append(caption(cx, ys + 48, "tools", fs=11))
        body.append(text(cx, ys + 66, tools, fs=12, col=INK))
        body.append(caption(cx, ys + 96, "transition", fs=11))
        body.append(mtext(cx, ys + 122, trans, fs=12, col=PAL["state"][2],
                          weight="600"))
    write(fname, svg(W, H, "".join(body)))


def d_extraction_pipeline(fname):
    W, H = 820, 320
    body = []
    a = box(40, 60, 230, 120, [
        '"Hi, I\'m Alice from', 'Acme Corp, I need',
        'help with billing."'], kind="neutral", title="Conversation transcript",
        title_fs=13)
    b = box(330, 84, 160, 72, ["with", "JSON Schema"], kind="l1",
            title="OOB LLM call", title_fs=13)
    c = box(560, 50, 220, 140, [
        'caller_name:  "Alice"', 'caller_org:   "Acme Corp"',
        'request_type: "billing"'], kind="state", title="State", title_fs=14,
        align="start", x_text=560 + 16, body_fs=12)
    body += [a, b, c]
    body.append(arrow(270, 120, 328, 120, marker="arrowd"))
    body.append(arrow(490, 120, 558, 120, marker="arrowd"))
    body.append(elbow([(670, 190), (670, 240), (490, 240)],
                      marker="arrowr", w=1.8))
    body.append(caption(360, 264, "triggers phase transition!", fs=12.5,
                        col=PAL["bad"][2]))
    write(fname, svg(W, H, "".join(body)))


def d_watchers_temporal(fname):
    W, H = 820, 380
    body = []
    # top row: state change -> watcher -> callback
    body.append(box(36, 40, 250, 56, ["app:score  0.85 → 0.95"],
                    kind="state", title="State change", title_fs=12, fs=13))
    body.append(box(338, 34, 150, 70, ["crossed_above", "threshold = 0.9"],
                    kind="control", title="Watcher", title_fs=13))
    body.append(box(540, 40, 244, 56, ['state.set("alert", true)'],
                    kind="ok", title="fires callback", title_fs=12, fs=12.5))
    body.append(arrow(286, 68, 336, 68, marker="arrowd"))
    body.append(arrow(488, 68, 538, 68, marker="arrowg"))
    body.append(line(36, 150, W - 36, 150, color=CARD_BORDER, w=1.4))
    body.append(caption(W / 2, 168, "Temporal patterns", fs=12,
                        col=MUTED))
    # bottom: two cards
    body.append(box(70, 196, 320, 130, [
        "condition: confused == true", "held for 30 seconds",
        "", "→  offer help"], kind="tele", title="when_sustained",
        align="start", x_text=70 + 18, body_fs=12.5))
    body.append(box(430, 196, 320, 130, [
        "condition: repeating == true", "for 3 consecutive turns",
        "", "→  break loop"], kind="tele", title="when_turns",
        align="start", x_text=430 + 18, body_fs=12.5))
    write(fname, svg(W, H, "".join(body)))


def d_tool_dispatcher(fname):
    W, H = 760, 386
    body = []
    body.append(box(W / 2 - 150, 28, 300, 46, kind="l1",
                    lines=["Model decides to call a tool"], fs=14))
    body.append(box(W / 2 - 130, 110, 260, 50, ["routes by function name"],
                    kind="control", title="ToolDispatcher", title_fs=14))
    body.append(arrow(W / 2, 74, W / 2, 108, marker="arrowd"))
    tools = [
        ("get_wx", "Simple Tool", "tool"),
        ("calc_pay", "Typed Tool", "tool"),
        ("verify_identity", "AgentTool\n(text agent pipeline)", "l2"),
    ]
    n = len(tools)
    tw, th = 200, 64
    gap = (W - 52 - n * tw) / (n - 1)
    xs = [26 + i * (tw + gap) for i in range(n)]
    ty = 232
    for (name, sub, kind), x in zip(tools, xs):
        body.append(elbow([(W / 2, 160), (W / 2, 200),
                           (x + tw / 2, 200), (x + tw / 2, ty - 2)],
                          marker="arrow", w=1.6))
        body.append(box(x, ty, tw, th, kind=kind, fs=14, lines=[name]))
        body.append(mtext(x + tw / 2, ty + th + 22, sub, fs=11.5, col=MUTED))
    body.append(caption(W / 2, H - 24,
                        "Background tools: the model keeps talking while the tool runs asynchronously",
                        fs=11.5))
    write(fname, svg(W, H, "".join(body)))


def d_telemetry_pipeline(fname):
    W, H = 820, 410
    body = []
    body.append(box(W / 2 - 150, 26, 300, 46, kind="l0",
                    lines=["SessionEvent stream"], fs=14))
    left = box(80, 120, 280, 60, ["(writes to State keys)"],
               kind="control", title="SessionSignals", title_fs=14)
    right = box(460, 120, 280, 60, ["(lock-free atomic counters)"],
                kind="tele", title="SessionTelemetry", title_fs=14)
    body += [left, right]
    body.append(elbow([(W / 2, 72), (W / 2, 96), (220, 96), (220, 118)],
                      marker="arrow", w=1.6))
    body.append(elbow([(W / 2, 72), (W / 2, 96), (600, 96), (600, 118)],
                      marker="arrowv", w=1.6))
    body.append(box(70, 230, 300, 110, [
        "session:turn_count", "session:total_token_count",
        "session:is_speaking", "session:silence_ms"], kind="neutral",
        align="start", x_text=70 + 18, fs=12.5))
    body.append(box(450, 230, 300, 110, [
        "audio_chunks_out:    1482", "avg_latency_ms:       340",
        "interruptions:          3", "total_token_count:   5280"],
        kind="neutral", align="start", x_text=450 + 18, fs=12.5))
    body.append(arrow(220, 180, 220, 228, marker="arrow"))
    body.append(arrow(600, 180, 600, 228, marker="arrowv"))
    body.append(caption(220, 364,
                        "available to phases, watchers, extractors, guards", fs=11))
    body.append(caption(600, 364, "snapshot() → JSON for the devtools UI", fs=11))
    write(fname, svg(W, H, "".join(body)))


def d_turn_flow(fname):
    W = 760
    steps = [
        ("1", "fast", "Fast lane: on_audio, on_input_transcript", "(sync, <1 ms)"),
        ("2", "fast", "Model responds, turn completes", ""),
        ("3", "control", "Control lane: TranscriptBuffer records the turn", ""),
        ("4", "control", "Extractors run (OOB LLM call)", 'writes caller_name="Alice", caller_org="Acme Corp"'),
        ("5", "control", "Watchers fire on state changes", "crossed_above, became_true, changed_to"),
        ("6", "control", "Computed variables recompute", "derived:risk_level updates"),
        ("7", "control", "Phase machine evaluates transitions", "identify_caller → handle_request"),
        ("8", "control", "Phase on_exit / on_enter hooks fire", "instruction + navigation context updated"),
        ("9", "tele", "Telemetry lane: Signals + Telemetry update", "turn_count++, latency + tokens recorded"),
    ]
    top = 64
    rh = 58
    H = top + len(steps) * rh + 24
    body = []
    body.append(box(W / 2 - 200, 22, 400, 30, kind="state",
                    lines=['User speaks: "I\'m Alice from Acme Corp"'], fs=13,
                    rx=15))
    sx = 60
    body.append(line(sx, top - 6, sx, top + (len(steps) - 1) * rh + 18,
                     color=ARROW, w=2))
    for i, (num, kind, main, sub) in enumerate(steps):
        cy = top + i * rh + 16
        fill, stroke, tcol = PAL[kind]
        body.append(f'<circle cx="{sx}" cy="{cy}" r="15" fill="{fill}" '
                    f'stroke="{stroke}" stroke-width="1.8"/>')
        body.append(text(sx, cy, num, fs=13, col=tcol, weight="700"))
        body.append(text(sx + 32, cy - (8 if sub else 0), main, fs=13.5,
                        col=INK, anchor="start", weight="500"))
        if sub:
            body.append(text(sx + 32, cy + 12, "→ " + sub, fs=11.5, col=MUTED,
                            anchor="start"))
    write(fname, svg(W, H, "".join(body)))


def d_three_lane(fname, title_from_l0=True):
    W, H = 820, 470
    body = []
    body.append(box(W / 2 - 175, 24, 350, 44, kind="l0", fs=14,
                    lines=["SessionEvent  (broadcast from L0)"]))
    body.append(box(W / 2 - 90, 100, 180, 50, ["zero-work dispatcher"],
                    kind="neutral", title="Router", title_fs=14))
    body.append(arrow(W / 2, 68, W / 2, 98, marker="arrowd"))
    body.append(caption(W / 2 + 150, 125, "no state access on hot path",
                        fs=10.5, anchor="start"))
    lanes = [
        ("fast", "Fast Lane", "sync · <1 ms", [
            "on_audio", "on_text", "on_vad_*",
            "on_input_transcript", "on_output_transcript"]),
        ("control", "Control Lane", "async · can block", [
            "on_tool_call", "on_interrupted", "Phase transitions",
            "Extractors (concurrent)", "Watchers · Computed state",
            "Temporal patterns", "TranscriptBuffer (owned)"]),
        ("tele", "Telemetry Lane", "own broadcast rx", [
            "SessionSignals (State)", "SessionTelemetry (AtomicU64)",
            "on_usage callback", "Debounced 100 ms flush"]),
    ]
    n = 3
    lw = 240
    gap = (W - 52 - n * lw) / (n - 1)
    xs = [26 + i * (lw + gap) for i in range(n)]
    ly = 210
    lh = 230
    for (kind, name, sub, items), x in zip(lanes, xs):
        fill, stroke, tcol = PAL[kind]
        body.append(elbow([(W / 2, 150), (W / 2, 182),
                           (x + lw / 2, 182), (x + lw / 2, ly - 2)],
                          marker="arrow", w=1.6))
        body.append(f'<rect x="{x}" y="{ly}" width="{lw}" height="{lh}" '
                    f'rx="12" fill="{fill}" stroke="{stroke}" stroke-width="1.6"/>')
        body.append(text(x + lw / 2, ly + 24, name, fs=15, col=tcol,
                        weight="700", font=SANS))
        body.append(text(x + lw / 2, ly + 44, sub, fs=11, col=tcol,
                        italic=True, font=SANS))
        body.append(line(x + 16, ly + 58, x + lw - 16, ly + 58,
                         color=stroke, w=1))
        for j, it in enumerate(items):
            body.append(text(x + 18, ly + 78 + j * 22, it, fs=12, col=tcol,
                            anchor="start"))
    write(fname, svg(W, H, "".join(body)))


def d_data_flow(fname):
    W, H = 860, 640
    body = []
    cols = [("Client App", 150), ("gemini-genai-rs", 430), ("Gemini API", 720)]
    for name, x in cols:
        body.append(text(x, 34, name, fs=13, col=MUTED, weight="700",
                        font=SANS))
        body.append(line(x - 90, 46, x + 90, 46, color=CARD_BORDER, w=1.2))

    def b(x, y, w, h, lines, kind, **kw):
        body.append(box(x - w / 2, y, w, h, lines if isinstance(lines, list)
                        else [lines], kind=kind, fs=12.5, **kw))
    # client column
    b(150, 70, 150, 40, "Microphone", "neutral")
    b(150, 140, 170, 40, "PCM16 16 kHz", "neutral")
    body.append(arrow(150, 110, 150, 138, marker="arrowd"))
    # genai column
    b(430, 140, 200, 44, "SessionHandle", "l0")
    b(430, 220, 200, 40, "SessionCommand", "l0")
    b(430, 290, 200, 40, "Transport::send()", "l0")
    b(430, 380, 200, 40, "Transport::recv()", "l0")
    b(430, 450, 200, 40, "Codec::decode()", "l0")
    b(430, 520, 220, 46, "(broadcast channel)", "control", title="SessionEvent",
      title_fs=13)
    # api column
    b(720, 140, 150, 44, "Gemini Live", "api")
    b(720, 290, 170, 56, ["processes", "audio/text/tools"], "api",
      title="Model")
    # arrows client->genai
    body.append(arrow(235, 160, 328, 160, marker="arrowd"))
    body.append(caption(282, 150, "send_audio()", fs=10))
    # down genai
    for y0, y1 in [(184, 218), (260, 288), (330, 378), (420, 448), (490, 518)]:
        body.append(arrow(430, y0, 430, y1, marker="arrowd"))
    # genai<->api
    body.append(arrow(530, 312, 636, 312, marker="arrowd"))
    body.append(caption(584, 302, "WebSocket", fs=10))
    body.append(arrow(720, 184, 720, 288, marker="arrowd"))
    body.append(elbow([(720, 346), (720, 470), (632, 470), (550, 472)],
                      marker="arrowd"))
    body.append(caption(660, 462, "frames", fs=10))
    # fan to lanes
    lanes = [("Fast Lane", "fast", 250), ("Control Lane", "control", 430),
             ("Telemetry", "tele", 610)]
    ly = 590
    for name, kind, x in lanes:
        body.append(box(x - 80, ly, 160, 36, kind=kind, fs=12,
                        lines=[name]))
        body.append(elbow([(430, 566), (430, 578), (x, 578), (x, ly - 2)],
                          marker="arrow", w=1.4))
    write(fname, svg(W, H, "".join(body)))


def d_two_lane(fname):
    W, H = 720, 280
    body = []
    body.append(box(W / 2 - 175, 26, 350, 44, kind="l0", fs=14,
                    lines=["SessionEvent  (broadcast)"]))
    body.append(box(W / 2 - 90, 104, 180, 50, ["zero-work dispatcher"],
                    kind="neutral", title="Router", title_fs=14))
    body.append(arrow(W / 2, 70, W / 2, 102, marker="arrowd"))
    body.append(caption(W / 2 + 150, 129, "no state access on hot path",
                        fs=10.5, anchor="start"))
    body.append(box(70, 200, 270, 56, ["sync · must finish in <1 ms"],
                    kind="fast", title="Fast Lane", title_fs=15))
    body.append(box(380, 200, 270, 56, ["async · may block / await"],
                    kind="control", title="Control Lane", title_fs=15))
    body.append(elbow([(W / 2, 154), (W / 2, 176), (205, 176), (205, 198)],
                      marker="arrowg", w=1.6))
    body.append(elbow([(W / 2, 154), (W / 2, 176), (515, 176), (515, 198)],
                      marker="arrow", w=1.6))
    write(fname, svg(W, H, "".join(body)))


def d_audio_pipeline(fname):
    W, H = 760, 400
    body = []
    # API box centered top
    body.append(box(W / 2 - 160, 30, 320, 74, [
        "Server-side VAD · Model inference"], kind="api",
        title="Gemini Live API", title_fs=14))
    # left outbound column
    body.append(box(60, 150, 230, 50, ["PCM16 16 kHz · mono"],
                    kind="neutral", title="Microphone", title_fs=13))
    body.append(box(60, 250, 230, 50, kind="l2", fs=13,
                    lines=["handle.send_audio(bytes)"]))
    body.append(arrow(175, 200, 175, 248, marker="arrowd"))
    body.append(elbow([(175, 300), (175, 330), (W / 2, 330), (W / 2, 106)],
                      marker="arrowd", w=1.8))
    body.append(caption_bg(218, 330, "outbound", fs=11, col=PAL["l1"][2]))
    # right inbound column
    body.append(box(470, 150, 230, 50, ["PCM16 24 kHz · mono"],
                    kind="neutral", title="Audio response", title_fs=13))
    body.append(box(470, 250, 230, 50, kind="control", fs=13,
                    lines=["on_audio(|data: &Bytes|)"]))
    body.append(box(470, 330, 230, 44, kind="ok", fs=13,
                    lines=["Speaker / playback"]))
    body.append(elbow([(W / 2, 106), (W / 2, 128), (585, 128), (585, 148)],
                      marker="arrowd", w=1.8))
    body.append(arrow(585, 200, 585, 248, marker="arrowd"))
    body.append(arrow(585, 300, 585, 328, marker="arrowg"))
    body.append(caption_bg(628, 128, "inbound", fs=11, col=PAL["api"][2]))
    write(fname, svg(W, H, "".join(body)))


def d_session_lifecycle(fname):
    W, H = 880, 240
    body = []
    states = ["Disconnected", "Connecting", "SetupSent", "Active"]
    sw = 150
    xs = [30, 230, 430, 630]
    sy = 60
    for name, x in zip(states, xs):
        kind = "ok" if name == "Active" else "neutral"
        body.append(box(x, sy, sw, 46, kind=kind, fs=13, lines=[name]))
    for i in range(len(xs) - 1):
        body.append(arrow(xs[i] + sw, sy + 23, xs[i + 1] - 2, sy + 23,
                          marker="arrowd"))
    # active loops back to disconnected (top)
    body.append(elbow([(705, sy), (705, 28), (105, 28), (105, sy - 2)],
                      marker="arrowd", w=1.6))
    body.append(caption(405, 20, "clean / unclean disconnect", fs=10.5))
    # active branches down
    body.append(box(560, 150, 130, 44, ["60 s warning"], kind="state",
                    title="GoAway", title_fs=12))
    body.append(box(710, 150, 140, 44, ["barge-in"], kind="bad",
                    title="Interrupted", title_fs=12))
    body.append(elbow([(705, 106), (705, 130), (625, 130), (625, 148)],
                      marker="arrow", w=1.5))
    body.append(elbow([(705, 106), (705, 130), (780, 130), (780, 148)],
                      marker="arrowr", w=1.5))
    write(fname, svg(W, H, "".join(body)))


def d_turn_complete_pipeline(fname):
    W, H = 820, 560
    body = []
    body.append(text(140, 40, "Gemini API", fs=13, col=MUTED, weight="700",
                    font=SANS))
    body.append(box(50, 60, 200, 120, [
        "Model speaks…", "Model finishes", "", "emits TurnComplete"],
        kind="api", title=None, fs=13))
    body.append(text(560, 40, "Control Lane pipeline", fs=13, col=MUTED,
                    weight="700", font=SANS))
    steps = [
        "1.  Reset turn state",
        "2.  Finalize transcript",
        "3.  Snapshot watched keys (before)",
        "4.  Run extractors (filtered by trigger)",
        "5.  Recompute derived state",
        "6.  Build transcript window",
        "7.  Evaluate phase transitions",
        "7b. Regenerate navigation context",
        "7c. Run OnPhaseChange extractors",
        "8.  Fire watchers (before vs after)",
        "9.  Check temporal patterns",
        "10. Instruction amendment",
        "11. Instruction template",
        "12. Send instruction update (deduped)",
        "13. Send on_enter context",
        "14. turnComplete if prompt_on_enter",
        "15. Turn boundary hook",
        "16. User turn-complete callback",
        "17. Increment turn_count",
    ]
    bx, bw = 330, 460
    by = 60
    bh = 470
    fill, stroke, tcol = PAL["control"]
    body.append(f'<rect x="{bx}" y="{by}" width="{bw}" height="{bh}" rx="12" '
                f'fill="{fill}" stroke="{stroke}" stroke-width="1.6"/>')
    for i, s in enumerate(steps):
        hi = s.startswith(("4.", "7.", "17"))
        body.append(text(bx + 22, by + 24 + i * 23.5, s, fs=12.5,
                        col=(PAL["state"][2] if hi else tcol),
                        anchor="start", weight=("700" if hi else "400")))
    body.append(arrow(250, 120, 328, 120, marker="arrowd"))
    body.append(caption(289, 110, "TurnComplete", fs=10))
    write(fname, svg(W, H, "".join(body)))


def d_state_flow(fname):
    W, H = 760, 620
    body = []

    def b(cx, y, w, h, title, lines, kind):
        body.append(box(cx - w / 2, y, w, h, lines, kind=kind, title=title,
                        title_fs=13, body_fs=11.5))
    # row 1: three boxes
    b(150, 40, 200, 56, "Conversation", ["(transcript)"], "neutral")
    b(380, 40, 200, 56, "Extractors", ["LLM / recognizer"], "control")
    b(610, 40, 200, 56, "State", ["(derived:)"], "state")
    body.append(arrow(250, 68, 280, 68, marker="arrowd"))
    body.append(arrow(480, 68, 510, 68, marker="arrowd"))
    # to computed
    body.append(elbow([(610, 96), (610, 130), (380, 130), (380, 158)],
                      marker="arrowd", w=1.6))
    b(380, 158, 240, 56, "Computed Variables", ["(dependency-sorted)"], "tele")
    # fan to three
    fan = [("Watchers", "(diffs)", 130), ("Temporal", "Patterns", 380),
           ("Phase", "Transitions", 630)]
    fy = 280
    for name, sub, x in fan:
        body.append(box(x - 95, fy, 190, 56, [sub], kind="control",
                        title=name, title_fs=13))
        body.append(elbow([(380, 214), (380, 250), (x, 250), (x, fy - 2)],
                          marker="arrow", w=1.5))
    # converge to instruction
    for x in [130, 380, 630]:
        body.append(elbow([(x, fy + 56), (x, 386), (380, 386), (380, 414)],
                          marker="arrow", w=1.5))
    b(380, 414, 260, 58, "Instruction Update", ["+ prompt_on_enter"], "l1")
    body.append(arrow(380, 472, 380, 506, marker="arrowd"))
    b(380, 506, 220, 50, "Model speaks", [], "ok")
    write(fname, svg(W, H, "".join(body)))


def d_turn_timeline(fname):
    W, H = 840, 360
    body = []
    body.append(arrow(40, 70, W - 30, 70, marker="arrowd", w=1.8))
    body.append(caption(40, 56, "time", fs=11, anchor="start"))
    marks = [(150, "User speaks"), (380, "Model responds"),
             (640, "TurnComplete fires")]
    for x, lab in marks:
        body.append(line(x, 62, x, 78, color=MUTED, w=1.5))
        body.append(caption(x, 44, lab, fs=11, col=INK, italic=False))
    body.append(box(70, 120, 150, 50, kind="neutral", fs=12.5,
                    lines=["Audio input"]))
    body.append(box(300, 120, 160, 50, ["(speech)"], kind="control",
                    title="Model turn", title_fs=13))
    body.append(box(520, 110, 290, 130, [
        '4.  Extract: caller_name="Jane"',
        "5.  Computed: risk_level=low",
        "7.  Transition: greeting→main",
        "12. Update instruction",
        "14. prompt_on_enter → model"], kind="state", title="Pipeline",
        title_fs=13, align="start", x_text=520 + 16, body_fs=12))
    body.append(arrow(220, 145, 298, 145, marker="arrowd"))
    body.append(arrow(460, 145, 518, 145, marker="arrowd"))
    body.append(box(540, 290, 250, 50, ["with updated instruction"],
                    kind="ok", title="Model speaks in new phase", title_fs=12))
    body.append(arrow(665, 240, 665, 288, marker="arrowg"))
    write(fname, svg(W, H, "".join(body)))


def d_guard_bug(fname):
    W, H = 760, 420
    body = []
    body.append(box(W / 2 - 110, 26, 220, 40, kind="neutral", fs=13,
                    lines=["Session connects"]))
    body.append(box(W / 2 - 200, 110, 400, 110, [
        "prompt_on_enter fires", 'Model: "Hello!"',
        "TurnComplete  →  Guard: true  →  transition fires"],
        kind="state", title="greeting phase enters", title_fs=14))
    body.append(arrow(W / 2, 66, W / 2, 108, marker="arrowd"))
    body.append(caption_bg(W / 2, 244, "…but the user hasn't spoken yet",
                           fs=12, col=PAL["bad"][2]))
    body.append(box(W / 2 - 220, 268, 440, 110, [
        'enter_prompt: "User said…"   ← nothing was said',
        "Model HALLUCINATES a response"], kind="bad",
        title="next_phase enters", title_fs=14))
    body.append(arrow(W / 2, 220, W / 2, 266, marker="arrowr"))
    write(fname, svg(W, H, "".join(body)))


def d_enter_prompt(fname):
    W, H = 800, 360
    body = []
    body.append(text(210, 40, "Phase A  (exiting)", fs=13, col=MUTED,
                    weight="700", font=SANS))
    body.append(text(590, 40, "Phase B  (entering)", fs=13, col=MUTED,
                    weight="700", font=SANS))
    body.append(line(30, 52, 390, 52, color=CARD_BORDER, w=1.2))
    body.append(line(410, 52, 770, 52, color=CARD_BORDER, w=1.2))
    body.append(box(60, 90, 300, 50, kind="control", fs=13,
                    lines=['Model: "How can I help?"']))
    body.append(box(430, 80, 320, 130, [
        "instruction → Phase B",
        "enter_prompt injected as",
        "Content::model():",
        '  "I have the name. I\'ll verify."',
        "turnComplete: true"], kind="state", title=None, fs=12,
        align="start", x_text=448))
    body.append(arrow(360, 140, 428, 140, marker="arrowd"))
    body.append(arrow(590, 210, 590, 250, marker="arrowd"))
    body.append(box(430, 250, 320, 80, [
        'Model sees its "own" previous output',
        "and generates a coherent continuation"], kind="ok", title=None,
        fs=12))
    write(fname, svg(W, H, "".join(body)))


def d_background_agent(fname):
    W, H = 800, 430
    body = []
    body.append(text(210, 38, "Voice Session (Live)", fs=13, col=MUTED,
                    weight="700", font=SANS))
    body.append(text(600, 38, "Background Agent", fs=13, col=MUTED,
                    weight="700", font=SANS))
    body.append(line(30, 50, 390, 50, color=CARD_BORDER, w=1.2))
    body.append(line(410, 50, 770, 50, color=CARD_BORDER, w=1.2))
    body.append(box(70, 70, 280, 44, kind="control", fs=13,
                    lines=["Turn completes → on_turn_complete"]))
    body.append(box(70, 150, 280, 44, ["next turn continues, no blocking"],
                    kind="ok", title=None, fs=12.5))
    body.append(box(70, 320, 280, 56, ["Transition guard checks",
                                       "state set by the agent"], kind="state",
                    title=None, fs=12.5))
    body.append(box(460, 90, 290, 130, [
        "runs generate() on flash LLM", "reads State",
        "writes State", "", "→ results land in State"], kind="tele",
        title="Agent", title_fs=13, align="start", x_text=478))
    body.append(arrow(210, 114, 210, 148, marker="arrowd"))
    body.append(elbow([(350, 92), (430, 92), (440, 110), (458, 120)],
                      marker="arrowv", w=1.6))
    body.append(caption(405, 78, "fire-and-forget", fs=10, col=PAL["tele"][2]))
    body.append(elbow([(600, 220), (600, 348), (470, 348), (352, 348)],
                      marker="arrow", w=1.6))
    write(fname, svg(W, H, "".join(body)))


def d_agent_tool_sync(fname):
    W, H = 600, 446
    body = []
    seq = [
        ('Model calls "verify_identity"', "l1"),
        ("TextAgentTool runs  (model waits)", "tool"),
        ("Agent: generate() · read/write State · return", "tele"),
        ("FunctionResponse sent to model", "control"),
        ("Model continues with the result", "ok"),
    ]
    y = 36
    bw, bh = 460, 50
    cx = W / 2
    for i, (lab, kind) in enumerate(seq):
        body.append(box(cx - bw / 2, y, bw, bh, kind=kind, fs=13,
                        lines=[lab]))
        if i < len(seq) - 1:
            body.append(arrow(cx, y + bh, cx, y + bh + 26, marker="arrowd"))
        y += bh + 26
    body.append(caption(cx, y + 12, "synchronous — the model blocks on the result",
                        fs=11))
    write(fname, svg(W, H, "".join(body)))


def d_background_tool(fname):
    W, H = 800, 348
    body = []
    body.append(text(190, 38, "Standard tool", fs=13, col=PAL["bad"][2],
                    weight="700", font=SANS))
    body.append(text(600, 38, "Background tool", fs=13, col=PAL["ok"][2],
                    weight="700", font=SANS))
    body.append(line(30, 50, 390, 50, color=CARD_BORDER, w=1.2))
    body.append(line(410, 50, 770, 50, color=CARD_BORDER, w=1.2))
    # standard
    body.append(box(60, 70, 290, 44, kind="neutral", fs=12.5,
                    lines=['Model: "Let me check…"']))
    body.append(box(60, 150, 290, 80, ["executes (3 seconds)", "",
                                       "— dead air —"], kind="bad", title=None,
                    fs=12.5))
    body.append(box(60, 270, 290, 44, ["Model speaks"], kind="neutral",
                    title=None, fs=12.5))
    body.append(arrow(205, 114, 205, 148, marker="arrowr"))
    body.append(arrow(205, 230, 205, 268, marker="arrowr"))
    # background
    body.append(box(450, 70, 300, 44, ['Ack: "running" → keeps talking'],
                    kind="ok", title=None, fs=12))
    body.append(box(450, 150, 300, 60, ["tool executes in background",
                                        "(3 seconds)"], kind="tool", title=None,
                    fs=12.5))
    body.append(box(450, 250, 300, 60, ["result injected",
                                        "model incorporates naturally"],
                    kind="ok", title=None, fs=12.5))
    body.append(arrow(600, 114, 600, 148, marker="arrowg"))
    body.append(arrow(600, 210, 600, 248, marker="arrowg"))
    body.append(caption_bg(683, 131, "no silence", fs=10, col=PAL["ok"][2]))
    write(fname, svg(W, H, "".join(body)))


# ---------------------------------------------------------------------------
def main():
    # README
    d_architecture_stack("architecture-stack.svg")
    d_core_concepts("core-concepts.svg")
    d_state_hierarchy("state-hierarchy.svg")
    d_phase_flow("phase-flow.svg")
    d_extraction_pipeline("extraction-pipeline.svg")
    d_watchers_temporal("watchers-temporal.svg")
    d_tool_dispatcher("tool-dispatcher.svg")
    d_telemetry_pipeline("telemetry-pipeline.svg")
    d_turn_flow("turn-flow.svg")
    d_three_lane("three-lane-processor.svg")
    # architecture.md
    d_architecture_stack("architecture-stack-full.svg", with_api=True)
    d_data_flow("data-flow.svg")
    # live-callbacks.md
    d_two_lane("two-lane-model.svg")
    # live-sessions.md
    d_audio_pipeline("audio-pipeline.svg")
    d_session_lifecycle("session-lifecycle.svg")
    # phase-transitions-deep-dive.md
    d_turn_complete_pipeline("turn-complete-pipeline.svg")
    d_state_flow("state-flow.svg")
    d_turn_timeline("turn-timeline.svg")
    d_guard_bug("unconditional-guard-bug.svg")
    d_enter_prompt("enter-prompt.svg")
    d_background_agent("background-agent-dispatch.svg")
    d_agent_tool_sync("agent-tool-sync.svg")
    d_background_tool("background-tool-execution.svg")


if __name__ == "__main__":
    main()
