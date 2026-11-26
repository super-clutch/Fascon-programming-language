#!/usr/bin/env python3
"""tsoa_summary.py — bench/tsoa_bench.sh の結果を表にする（回ごとの中央値の中央値） / tabulate bench/tsoa_bench.sh results
(the median over rounds of each run's median)

usage: python3 bench/tsoa_summary.py <results.tsv> [--markdown] [--names <before>,<after>]
       python3 bench/tsoa_summary.py --float <results.tsv> [--markdown]   (bench/fsimd_bench.sh)
"""
import statistics
import sys


def floats(path, md):
    """bench/fsimd_bench.sh の結果: Fascon と C（素直な -O2）と C（-ffast-math） / bench/fsimd_bench.sh results: Fascon, C
    (plain -O2) and C (-ffast-math)"""
    rows = {}
    ghz = []
    cur = 0.0
    ops = []
    for line in open(path, encoding="utf-8"):
        parts = line.rstrip("\n").split("\t")
        if len(parts) >= 2 and parts[1].startswith("#"):
            try:
                cur = float(parts[1].split("core clock ")[1].split(" GHz")[0])
                ghz.append(cur)
            except (IndexError, ValueError):
                pass
            continue
        if len(parts) != 9:
            continue
        _round, label, op, fns, cns, cfns = parts[:6]
        size, build = label.split(" ", 1)
        cc, march = build.split("-")
        if op not in ops:
            ops.append(op)
        d = rows.setdefault((size, cc, march, op), {"F": [], "C": [], "CF": [], "Fy": [], "Cy": [], "CFy": []})
        for k, v in (("F", fns), ("C", cns), ("CF", cfns)):
            d[k].append(float(v))
            d[k + "y"].append(float(v) * cur)
    g = statistics.median(ghz) if ghz else 0.0
    print("core clock: %.2f-%.2f GHz over the runs (IMUL chain, median %.2f); ns per particle (cycles per particle, each run at its own clock); medians over the rounds" % (min(ghz or [0]), max(ghz or [0]), g))
    for size in ("60000", "4194304"):
        for cc in ("gcc", "clang"):
            for march in ("base", "native"):
                hdr = "%s particles, %s, %s" % (size, cc, "baseline (SSE2)" if march == "base" else "-march=native")
                if md:
                    print("\n**%s**\n" % hdr)
                    print("| operation | C (AoS, -O2) | C (AoS, -ffast-math) | Fascon | Fascon / C | Fascon / C fast-math |")
                    print("|---|---:|---:|---:|---:|---:|")
                else:
                    print("\n" + hdr)
                for op in ops:
                    d = rows.get((size, cc, march, op))
                    if not d:
                        continue
                    m = {k: statistics.median(v) for k, v in d.items()}
                    if md:
                        print("| %s | %.3f (%.2f) | %.3f (%.2f) | %.3f (%.2f) | %.2f | %.2f |" % (op, m["C"], m["Cy"], m["CF"], m["CFy"], m["F"], m["Fy"], m["Fy"] / m["Cy"], m["Fy"] / m["CFy"]))
                    else:
                        print("  %-44s C %6.3f (%5.2f)  C-fast %6.3f (%5.2f)  Fascon %6.3f (%5.2f)  F/C %.2f  F/Cfast %.2f" % (op, m["C"], m["Cy"], m["CF"], m["CFy"], m["F"], m["Fy"], m["Fy"] / m["Cy"], m["Fy"] / m["CFy"]))


def main():
    if "--float" in sys.argv[1:]:
        floats(next(a for a in sys.argv[1:] if not a.startswith("--")), "--markdown" in sys.argv[1:])
        return
    path = sys.argv[1]
    md = "--markdown" in sys.argv[2:]
    names = ("Fascon before (mirrored)", "Fascon after (True SoA)")
    if "--names" in sys.argv:
        names = tuple(sys.argv[sys.argv.index("--names") + 1].split(",", 1))
    # 回ごとに、その回のクロック（IMUL の鎖）でクロック数に直す（仮想機械のクロックは回によって変わる）
    # each run's own clock (IMUL chain) turns its ns into cycles (the VM's clock changes from run to run)
    rows = {}
    cyc = {}
    ghz = []
    cur = 0.0
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        if not line:
            continue
        parts = line.split("\t")
        if len(parts) >= 2 and parts[1].startswith("#"):
            try:
                cur = float(parts[1].split("core clock ")[1].split(" GHz")[0])
                ghz.append(cur)
            except (IndexError, ValueError):
                pass
            continue
        if len(parts) != 7:
            continue
        _round, label, op, fns, cns, _fcy, _ccy = parts
        size, build = label.split(" ", 1)
        variant, cc, march = build.split("-")
        rows.setdefault((size, cc, march, op), {}).setdefault(variant, []).append(float(fns))
        rows[(size, cc, march, op)].setdefault("C", []).append(float(cns))
        cyc.setdefault((size, cc, march, op), {}).setdefault(variant, []).append(float(fns) * cur)
        cyc[(size, cc, march, op)].setdefault("C", []).append(float(cns) * cur)
    g = statistics.median(ghz) if ghz else 0.0
    ops = []
    for k in rows:
        if k[3] not in ops:
            ops.append(k[3])
    print("core clock: %.2f-%.2f GHz over the runs (IMUL chain, median %.2f); ns per task (cycles per task, each run at its own clock); medians over the rounds" % (min(ghz or [0]), max(ghz or [0]), g))
    for size in ("60000", "4194304"):
        for cc in ("gcc", "clang"):
            for march in ("base", "native"):
                hdr = "%s tasks, %s, %s" % (size, cc, "baseline (SSE2)" if march == "base" else "-march=native")
                if md:
                    print("\n**%s**\n" % hdr)
                    print("| operation | C (AoS) | %s | %s | after / C |" % names)
                    print("|---|---:|---:|---:|---:|")
                else:
                    print("\n" + hdr)
                for op in ops:
                    d = rows.get((size, cc, march, op))
                    if not d:
                        continue
                    y = cyc[(size, cc, march, op)]
                    c = statistics.median(d["C"])
                    b = statistics.median(d.get("before", [0]))
                    a = statistics.median(d.get("after", [0]))
                    cc_, bc, ac = statistics.median(y["C"]), statistics.median(y.get("before", [0])), statistics.median(y.get("after", [0]))
                    if md:
                        print("| %s | %.3f (%.2f) | %.3f (%.2f) | %.3f (%.2f) | %.2f |" % (op, c, cc_, b, bc, a, ac, ac / cc_ if cc_ else 0))
                    else:
                        print("  %-44s C %6.3f (%5.2f)  before %6.3f (%5.2f)  after %6.3f (%5.2f)  after/C %.2f" % (op, c, cc_, b, bc, a, ac, ac / cc_ if cc_ else 0))


if __name__ == "__main__":
    main()
