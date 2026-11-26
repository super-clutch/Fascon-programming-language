#!/usr/bin/env python3
"""辞書の検査 / dictionary audit

  python3 tools/check_dictionary.py [--verbose]

確かめること / checks:
  1. コンパイラ（src/*.rs）が出しうるエラー番号は、すべて data/dictionary.json の項目（名前か別名）にある
     every error code the compiler (src/*.rs) can emit has an entry (by name or alias) in data/dictionary.json
  2. tests/err/*.fsc の「// expect: Fxxxx」も辞書にある / the codes expected by tests/err/*.fsc are in the dictionary
  3. data/rules.json・data/fixes.json の番号は、コンパイラが出す番号 / the codes of data/rules.json and data/fixes.json are
     codes the compiler emits
  4. data/dictionary.json の項目はどれも名前・分類・要約が日英そろい（エラー番号の項目は説明と対処も）、名前は重ならず、
     文にシングルクォート（アポストロフィ）を含まない / every entry of data/dictionary.json has its name and both languages
     of category and summary (entries for error codes also detail and remedy), names are unique, and the text contains no
     single quotes (apostrophes)
  5. VS Code 拡張の辞書（vscode-extension/dictionary.json = 日本語、dictionary.en.json = 英語）は、1.0 の項目を同じ名前・
     同じ文で持つ（拡張には 0.1 の項目も入っている） / the dictionaries of the VS Code extension hold the 1.0 entries with the
     same names and the same text (they also carry the 0.1 entries)
終了コード 0 = 問題なし / exit code 0 = no problem
"""
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VERBOSE = "--verbose" in sys.argv
CODE = re.compile(r"F\d{4}")
problems = []


def load(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as f:
        return json.load(f)


def codes_of_entry(e):
    """項目が受け持つ番号（名前と別名から） / the codes an entry covers (from its name and aliases)"""
    out = set(CODE.findall(e["name"]))
    for a in e.get("aliases", []):
        out.update(CODE.findall(a))
    return out


# ───── コンパイラが出しうる番号 / codes the compiler can emit ─────
emitted = {}
for name in sorted(os.listdir(os.path.join(ROOT, "src"))):
    if not name.endswith(".rs"):
        continue
    for n, line in enumerate(open(os.path.join(ROOT, "src", name), encoding="utf-8"), 1):
        s = line.split("//")[0]  # 注釈の中の番号は数えない / codes inside comments do not count
        # 診断の番号: "F2601" という文字列、または "error[F3801]" のように書式の中に置いたもの / diagnostic codes: a
        # "F2601" string, or one placed inside a format such as "error[F3801]"
        for c in re.findall(r'"(F\d{4})"', s) + re.findall(r'(?:error|warning)\[(F\d{4})\]', s):
            emitted.setdefault(c, "%s:%d" % (name, n))

data = load("data/dictionary.json")
covered = {}
for e in data:
    for c in codes_of_entry(e):
        covered.setdefault(c, e["name"])

for c in sorted(emitted):
    if c not in covered:
        problems.append("emitted but not in data/dictionary.json: %s (%s)" % (c, emitted[c]))

# ───── tests/err の期待する番号 / codes expected by tests/err ─────
expected = {}
for name in sorted(os.listdir(os.path.join(ROOT, "tests", "err"))):
    if name.endswith(".fsc"):
        for line in open(os.path.join(ROOT, "tests", "err", name), encoding="utf-8"):
            m = re.match(r"//\s*expect:\s*(F\d{4})", line)
            if m:
                expected[m.group(1)] = name
                break
for c, f in sorted(expected.items()):
    if c not in covered:
        problems.append("tests/err/%s expects %s, which has no dictionary entry" % (f, c))
    if c not in emitted:
        problems.append("tests/err/%s expects %s, which src/*.rs never emits" % (f, c))

# ───── 解説の規則と自動修正 / explanation rules and quick fixes ─────
# rules.json の "*" は既定の解説。fixes.json は 1 つの番号に複数の修正を持てる（例: F3302 の == と !=）
# "*" in rules.json is the default explanation; fixes.json may hold several fixes for one code (e.g. == and != of F3302)
for rel, key in (("data/rules.json", "code"), ("data/fixes.json", "code")):
    seen = set()
    for r in load(rel):
        c = r.get(key, "")
        if c == "*" and rel.endswith("rules.json"):
            continue
        if c in seen and rel.endswith("rules.json"):
            problems.append("%s: duplicate code %s" % (rel, c))
        seen.add(c)
        if c not in emitted:
            problems.append("%s: %s is not a code the compiler emits" % (rel, c))
        for lang in ("ja", "en"):
            for field in ("title", "math") if rel.endswith("rules.json") else ():
                if not r.get(field, {}).get(lang, "").strip():
                    problems.append("%s: %s has no %s.%s" % (rel, c, field, lang))

# ───── 項目の形 / shape of the entries ─────
names = [e["name"] for e in data]
for n in sorted(set(x for x in names if names.count(x) > 1)):
    problems.append("data/dictionary.json: duplicate name %s" % n)
for e in data:
    # どの項目も分類と要約を日英で持つ。エラー番号の項目は、説明と対処も持つ（概念の項目に対処は要らない）
    # every entry has a category and a summary in both languages; entries for error codes also have a detail and a remedy
    # (concept entries need no remedy)
    fields = ("category", "summary", "detail", "remedy") if CODE.search(e["name"]) else ("category", "summary")
    for field in fields:
        v = e.get(field)
        if not isinstance(v, dict) or not v.get("ja", "").strip() or not v.get("en", "").strip():
            problems.append("data/dictionary.json: %s has no %s in both languages" % (e["name"], field))
    if not isinstance(e.get("aliases"), list):
        problems.append("data/dictionary.json: %s has no alias list" % e["name"])
    # このプロジェクトの決まり: 文にシングルクォートを使わない（ASCII のアポストロフィも U+2019 も） / project rule: no single
    # quotes in the text (neither the ASCII apostrophe nor U+2019)
    for field in ("category", "summary", "detail", "constraints", "remedy"):
        v = e.get(field)
        for lang in ("ja", "en"):
            t = v.get(lang, "") if isinstance(v, dict) else ""
            if "'" in t or "\u2019" in t:
                problems.append("data/dictionary.json: %s.%s.%s contains a single quote" % (e["name"], field, lang))

# ───── VS Code 拡張の辞書 / dictionaries of the VS Code extension ─────
for rel, lang in (("vscode-extension/dictionary.json", "ja"), ("vscode-extension/dictionary.en.json", "en")):
    ext = load(rel)
    by_name = {}
    for e in ext:
        if e["name"] in by_name:
            problems.append("%s: duplicate name %s" % (rel, e["name"]))
        by_name[e["name"]] = e
    for e in data:
        x = by_name.get(e["name"])
        if x is None:
            problems.append("%s: missing the 1.0 entry %s" % (rel, e["name"]))
            continue
        for field in ("category", "summary", "detail", "constraints", "remedy"):
            want = e.get(field, {}).get(lang, "") if isinstance(e.get(field), dict) else ""
            if (x.get(field) or "") != want:
                problems.append("%s: %s.%s differs from data/dictionary.json" % (rel, e["name"], field))
        if sorted(x.get("aliases", [])) != sorted(e.get("aliases", [])):
            problems.append("%s: %s.aliases differ from data/dictionary.json" % (rel, e["name"]))

if VERBOSE:
    print("codes emitted by src/*.rs: %d, covered by data/dictionary.json: %d entries / %d codes" % (len(emitted), len(data), len(covered)))
    print("codes in the dictionary but never emitted (0.1 or retired):", ", ".join(sorted(c for c in covered if c not in emitted)) or "none")
for p in problems:
    print("PROBLEM", p)
print("%d problem(s)" % len(problems))
sys.exit(1 if problems else 0)
