#!/usr/bin/env python3
"""辞書の同期 / dictionary sync

  python3 tools/sync_dictionary.py [--check]

data/dictionary.json（日英の 1.0 辞書。コンパイラに埋め込まれる）の項目を、VS Code 拡張の辞書へ写す:
vscode-extension/dictionary.json（日本語）と vscode-extension/dictionary.en.json（英語）。
copies the entries of data/dictionary.json (the bilingual 1.0 dictionary embedded in the compiler) into the dictionaries of
the VS Code extension: vscode-extension/dictionary.json (Japanese) and vscode-extension/dictionary.en.json (English).

  - 同じ名前の項目は、その位置のまま 1.0 の文に置き換える / an entry with the same name is replaced in place by the 1.0 text
  - 新しい項目は、data/dictionary.json で直前にある項目の後ろへ入れる / a new entry goes after the entry that precedes it in
    data/dictionary.json
  - 拡張にだけある項目（0.1.0 の辞書）はそのまま残す / entries found only in the extension (the 0.1.0 dictionary) stay as they are

--check: ファイルを書かず、同期が必要なら終了コード 1 / write nothing; exit code 1 if a sync is needed
検査は tools/check_dictionary.py / the audit is tools/check_dictionary.py
"""
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIELDS = ("category", "summary", "detail", "constraints", "remedy")


def load(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as f:
        return json.load(f)


def dump(v):
    return json.dumps(v, ensure_ascii=False, indent=1) + "\n"


def one_language(e, lang):
    """日英の項目を拡張の 1 言語の形へ / a bilingual entry in the single-language shape of the extension"""
    out = {"name": e["name"]}
    for f in FIELDS:
        v = e.get(f)
        out[f] = v.get(lang, "") if isinstance(v, dict) else ""
    out["aliases"] = list(e.get("aliases", []))
    return out


def sync(data, ext, lang):
    ext = list(ext)
    for i, e in enumerate(data):
        want = one_language(e, lang)
        names = [x["name"] for x in ext]
        if e["name"] in names:
            ext[names.index(e["name"])] = want
            continue
        # 直前の 1.0 の項目の後ろへ（無ければ末尾へ） / after the preceding 1.0 entry (or at the end)
        at = len(ext)
        for prev in reversed(data[:i]):
            if prev["name"] in names:
                at = names.index(prev["name"]) + 1
                break
        ext.insert(at, want)
    return ext


def main():
    check = "--check" in sys.argv
    data = load("data/dictionary.json")
    changed = []
    for rel, lang in (("vscode-extension/dictionary.json", "ja"), ("vscode-extension/dictionary.en.json", "en")):
        path = os.path.join(ROOT, rel)
        with open(path, encoding="utf-8") as f:
            old = f.read()
        new = dump(sync(data, json.loads(old), lang))
        if new != old:
            changed.append(rel)
            if not check:
                with open(path, "w", encoding="utf-8") as f:
                    f.write(new)
    for rel in changed:
        print(("needs sync: " if check else "updated: ") + rel)
    if not changed:
        print("in sync")
    sys.exit(1 if (check and changed) else 0)


if __name__ == "__main__":
    main()
