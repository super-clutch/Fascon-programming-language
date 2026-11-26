#!/usr/bin/env python3
"""docs/images の英語版の図を作る / build the English figures in docs/images

  python3 tools/make_doc_images.py [docs/images]

memory-model.en.svg   : the four kinds of storage in Fascon 1.0 and what keeps each safe
traversal-bench.en.svg: the traversal benchmark (numbers from bench/RESULTS.md)
"""
import os
import sys

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs", "images")
INK, QUIET, EDGE, TINT, ACCENT, MUTED = "#1f2328", "#59636e", "#8c959f", "#f3f4f6", "#2f6fdb", "#9aa1aa"
FONT = "font-family='-apple-system, Segoe UI, Helvetica, Arial, sans-serif'"


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def memory_model():
    W, H, BW, BH = 760, 468, 340, 120
    xs, ys = (24, 396), (80, 256)
    boxes = [
        ("Values and second-class references (per call)", ["Holds: locals, struct values, parameter references", "e.g. fn f(t: &Task) / if let Some(p) = @a.get(id)", "Guarantee: references cannot be stored (F0515)", "Freed: when the function or block ends"]),
        ("own regions (ownership trees)", ["Holds: a tree of own T; children packed into the parent", "e.g. ok: own Button / win: Option<own Window>", "Guarantee: one owner, move only (F0382)", "Freed: the whole tree at once with its owner"]),
        ("Arenas (static tables in .bss)", ["Holds: tables like task_struct; links are Ids", "e.g. arena @process: Process[64];", "Guarantee: colouring (F2101), stale Id gives None", "Freed: free bumps the generation (old Ids die)"]),
        ("Fixed-address regions (MMIO / VRAM)", ["Holds: the framebuffer and device registers", "e.g. mmio @fb: [[u32; 800]; 600] at 0x4800_0000;", "Guarantee: shape is the type, bounds at compile time", "Freed: never (it is hardware)"]),
    ]
    o = [f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {W} {H}' width='{W}' height='{H}' {FONT} font-size='11.5'>",
         f"<rect width='{W}' height='{H}' fill='#ffffff'/>",
         "<defs><marker id='a' viewBox='0 0 10 10' refX='9' refY='5' markerWidth='6' markerHeight='6' orient='auto-start-reverse'>"
         f"<path d='M0 0L10 5L0 10z' fill='{EDGE}'/></marker></defs>",
         f"<text x='24' y='30' font-size='15' font-weight='600' fill='{INK}'>Four kinds of storage, each kept safe by its own rule</text>",
         f"<text x='24' y='52' fill='{QUIET}'>All of them are checked at compile time, with no lifetime annotations</text>",
         f"<g fill='none' stroke='{EDGE}' stroke-width='1.25'><path d='M364 140H394' marker-end='url(#a)'/><path d='M150 200V254' marker-end='url(#a)'/><path d='M330 200V228H560V254' marker-end='url(#a)'/></g>",
         f"<text x='380' y='130' text-anchor='middle' fill='{QUIET}'>own</text>",
         f"<text x='160' y='232' fill='{QUIET}'>Id&lt;@a&gt; (a 4-byte value)</text>",
         f"<text x='404' y='220' fill='{QUIET}'>read and written by index (@fb[y][x])</text>"]
    for k, (name, lines) in enumerate(boxes):
        x, y = xs[k % 2], ys[k // 2]
        o.append(f"<rect x='{x}' y='{y}' width='{BW}' height='{BH}' rx='8' fill='none' stroke='{EDGE}' stroke-width='1.25'/>")
        o.append(f"<text x='{x + 16}' y='{y + 26}' font-size='13' font-weight='600' fill='{INK}'>{esc(name)}</text>")
        for i, line in enumerate(lines):
            o.append(f"<text x='{x + 16}' y='{y + 50 + 18 * i}' fill='{QUIET if i == 1 else INK}'>{esc(line)}</text>")
    o.append(f"<rect x='24' y='400' width='712' height='44' rx='8' fill='{TINT}' stroke='{EDGE}' stroke-width='1.25'/>")
    o.append(f"<text x='380' y='427' text-anchor='middle' fill='{INK}'>Generated C: the only pointers stored in structs are own; links are uint32_t Ids; VRAM is a constant address + index</text>")
    o.append("</svg>")
    return "\n".join(o)


# bench/RESULTS.md の 1.0.0 の中央値（ns / タスク） / the 1.0.0 medians from bench/RESULTS.md (ns per task)
# (グループ, 行, 種類, gcc, clang, 比の計算での役割) / (group, row, kind, gcc, clang, role in the ratios)
ROWS = [
    ("order", "c_packed", "C", 2.86, 2.86, "pc"),
    ("order", "f_get", "F", 3.82, 5.35, "fc"),
    ("order", "f_idx", "F", 3.70, 5.45, ""),
    ("scattered", "c_packed", "C", 55.61, 55.33, "ps"),
    ("scattered", "c_frag", "C", 78.58, 76.22, "ps"),
    ("scattered", "f_get", "F", 28.95, 44.87, "fs"),
    ("scattered", "f_idx", "F", 33.52, 28.57, "fs"),
    ("scan", "f_scan", "F", 0.94, 0.91, ""),
]

TEXT = {
    "en": {
        "font": FONT,
        "title": "On a scattered list, the Fascon Id chain beats raw pointers",
        "sub": "Time per task (ns, shorter is faster) - summing vruntime over 60,000 tasks - Fascon 1.0.0",
        "lead": "Scattered: Fascon {a:.1f}-{b:.1f}x faster.   Creation order: raw pointers {c:.1f}-{d:.1f}x faster.   Table scan: under 1 ns.",
        "fascon": "Fascon", "craw": "C with raw pointers", "dark": "dark = gcc 13.3", "light": "light = clang 18.1",
        "order": "Linked in creation order", "scattered": "Linked in a scattered order", "scan": "Scanning the table (independent of links)",
        "c_packed": "C, raw pointers (packed malloc)", "c_frag": "C, raw pointers (fragmented malloc)",
        "f_get": "Fascon, Id chain with get (branch)", "f_idx": "Fascon, Id chain with @task[id] (latched)", "f_scan": "Fascon, for id in @task (sum scan)",
    },
    "ja": {
        "font": "font-family='Noto Sans CJK JP, Noto Sans JP, sans-serif'",
        "title": "ばらばらにつないだリストでは、Fascon の Id の連鎖が生ポインタより速い",
        "sub": "1 タスクあたりの時間（ns、短いほど速い）· 6 万タスクの vruntime の合計 · Fascon 1.0.0",
        "lead": "ばらばら: Fascon が {a:.1f}〜{b:.1f} 倍速い。作成順: 生ポインタが {c:.1f}〜{d:.1f} 倍速い。表の走査: 1 ns 未満。",
        "fascon": "Fascon", "craw": "生ポインタの C", "dark": "濃い色 = gcc 13.3", "light": "薄い色 = clang 18.1",
        "order": "作成順につないだ場合", "scattered": "ばらばらの順につないだ場合", "scan": "表を走査（つなぎ方に関係なし）",
        "c_packed": "C・生ポインタ（連続した malloc）", "c_frag": "C・生ポインタ（断片化した malloc）",
        "f_get": "Fascon・get での Id の連鎖（分岐）", "f_idx": "Fascon・@task[id] での Id の連鎖（ラッチ）", "f_scan": "Fascon・for id in @task（総和の走査）",
    },
}


def bench(lang):
    T = TEXT[lang]
    W, H, x0, span = 780, 470, 320, 380
    peak = max(max(r[3], r[4]) for r in ROWS)
    ps = [r[3 + c] for r in ROWS if r[5] == "ps" for c in (0, 1)]
    fs = [r[3 + c] for r in ROWS if r[5] == "fs" for c in (0, 1)]
    pc = [r[3 + c] for r in ROWS if r[5] == "pc" for c in (0, 1)]
    fc = [r[3 + c] for r in ROWS if r[5] == "fc" for c in (0, 1)]
    rb = [p / f for p in ps for f in fs]
    ra = [f / p for f in fc for p in pc]
    o = [f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {W} {H}' width='{W}' height='{H}' {T['font']} font-size='12'>",
         f"<rect width='{W}' height='{H}' fill='#ffffff'/>",
         f"<text x='24' y='28' font-size='15' font-weight='600' fill='{INK}'>{esc(T['title'])}</text>",
         f"<text x='24' y='50' font-size='11.5' fill='{QUIET}'>{esc(T['sub'])}</text>",
         f"<text x='24' y='68' font-size='11.5' fill='{INK}'>{esc(T['lead'].format(a=min(rb), b=max(rb), c=min(ra), d=max(ra)))}</text>",
         f"<rect x='24' y='79' width='12' height='10' rx='2' fill='{ACCENT}'/><text x='42' y='88' font-size='11.5' fill='{QUIET}'>{esc(T['fascon'])}</text>",
         f"<rect x='104' y='79' width='12' height='10' rx='2' fill='{MUTED}'/><text x='122' y='88' font-size='11.5' fill='{QUIET}'>{esc(T['craw'])}</text>",
         f"<rect x='262' y='79' width='12' height='10' rx='2' fill='{MUTED}'/><text x='280' y='88' font-size='11.5' fill='{QUIET}'>{esc(T['dark'])}</text>",
         f"<rect x='406' y='79' width='12' height='10' rx='2' fill='{MUTED}' fill-opacity='0.45'/><text x='424' y='88' font-size='11.5' fill='{QUIET}'>{esc(T['light'])}</text>"]
    y, prev = 92, None
    for g, s, kind, gcc, clang, _ in ROWS:
        if prev != g:
            y += 34
            o.append(f"<text x='24' y='{y - 8}' font-weight='600' fill='{INK}'>{esc(T[g])}</text>")
        else:
            y += 12
        prev = g
        o.append(f"<text x='36' y='{y + 16}' fill='{INK}'>{esc(T[s])}</text>")
        for c, v in enumerate((gcc, clang)):
            w = max(1.5, v / peak * span)
            fill = ACCENT if kind == "F" else MUTED
            o.append(f"<rect x='{x0}' y='{y}' width='{w:.1f}' height='10' rx='2' fill='{fill}' fill-opacity='{0.45 if c else 1}'/>")
            o.append(f"<text x='{x0 + w + 6:.1f}' y='{y + 9}' font-size='11' fill='{QUIET}'>{v:.2f} ns</text>")
            y += 13
    o.append("</svg>")
    return "\n".join(o)


os.makedirs(OUT, exist_ok=True)
for name, svg in (("memory-model.en.svg", memory_model()), ("traversal-bench.en.svg", bench("en")), ("traversal-bench.ja.svg", bench("ja"))):
    with open(os.path.join(OUT, name), "w", encoding="utf-8") as f:
        f.write(svg + "\n")
    print("wrote", os.path.join(OUT, name))
