#!/usr/bin/env python3
"""リリース一式を作る / build the release files

  python3 packaging/make_release.py --bin2 <dir> --core <dir> --legacy <dir> [--vsce "npx --yes @vscode/vsce"] [--out dist]

  --bin2    1.0 の実行ファイル（この repo のビルド） / the 1.0 executables (builds of this repository):
              win32-x64/fascon.exe  linux-x64/fascon  linux-arm64/fascon  darwin-universal/fascon
  --core    0.1.0 の実行ファイル（同じ並び。0.1.0 の VSIX の extension/bin がそのまま使える）。無いプラットフォームは
            0.1 エンジンなしで作る（1.0 の機能はそのまま使え、0.1 だけの構文のファイルには fascon がその旨を出す）
            the 0.1.0 executables in the same layout (extension/bin of the 0.1.0 VSIX works as is); a platform without one
            is packaged without the 0.1 engine (1.0 features are unaffected; fascon says so for files with 0.1-only syntax)
  --legacy  0.1.0 の Linux 版を展開したフォルダ（runtime/・bsp-examples/・lsp-config/・docs/・examples/・include/）
            the extracted 0.1.0 Linux archive (runtime/, bsp-examples/, lsp-config/, docs/, examples/, include/)

Output (in --out): fascon-1.0.0.vsix, fascon-1.0.0-windows-x64.zip, fascon-1.0.0-linux.tar.gz,
fascon-1.0.0-macos-universal.tar.gz, fascon-1.0.0-src.zip, SHA256SUMS.txt
"""
import argparse
import gzip
import hashlib
import io
import os
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile

VERSION = "1.0.0"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MTIME = time.mktime((2026, 11, 11, 0, 0, 0, 0, 0, -1))
# 0.1.0 の配布物にあり、この repo では別の名前になった文書 / documents of the 0.1.0 distribution renamed in this repository
LEGACY_DOCS_SUPERSEDED = ("SPEC-1.1-SMP.ja.md",)
PLATS = {"win32-x64": ".exe", "linux-x64": "", "linux-arm64": "", "darwin-universal": ""}
# 配布する 1.0 の例 / the 1.0 examples shipped in the VSIX and the archives
EXAMPLES = ("kernel_core.fsc", "gui_desktop.fsc", "gui_dual.fsc", "bios_dual.fsc", "rust_style.rs", "app_wordfreq.fsc", "vruntime_scan.fsc", "own_callbacks.fsc", "kernel_preempt.fsc", "kernel_storage.fsc", "kernel_net.fsc", "kernel_user.fsc", "user_init.fsc", "user_init.bin", "smp_kernel.fsc", "genos_smp.fsc", "genos_qemu.fsc", "gc_graph.fsc")

ap = argparse.ArgumentParser()
ap.add_argument("--bin2", required=True)
ap.add_argument("--core", required=True)
ap.add_argument("--legacy", required=True)
ap.add_argument("--vsce", default="npx --yes @vscode/vsce")
ap.add_argument("--out", default=os.path.join(ROOT, "dist"))
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)
work = tempfile.mkdtemp(prefix="fascon-release-")


def is_exec(rel):
    base = os.path.basename(rel)
    return base in ("fascon", "fascon-core") or base.endswith(".exe") or rel.endswith(".sh") or rel.endswith(".py")


# コンパイラのモジュールのキャッシュ（自動で作り直される）は配らない / compiler module caches (rebuilt automatically) are not shipped
NEVER = (".fascon-cache", "__pycache__")


def add_tree(entries, src, dst, skip=()):
    """src の中身を dst（アーカイブ内のパス）の下に足す / add the contents of src under dst (path inside the archive)"""
    for dirpath, dirnames, filenames in os.walk(src):
        dirnames[:] = sorted(d for d in dirnames if d not in skip and d not in NEVER)
        for f in sorted(filenames):
            full = os.path.join(dirpath, f)
            rel = os.path.relpath(full, src).replace(os.sep, "/")
            if any(rel == s or rel.startswith(s + "/") for s in skip):
                continue
            entries.append((full, dst + "/" + rel if dst else rel))


def unique(entries):
    """同じ名前は最初のものだけ（この repo のファイルが 0.1.0 のファイルより先） / keep the first entry of each name
    (files of this repository come before the 0.1.0 files)"""
    seen, out = set(), []
    for full, name in entries:
        if name in seen:
            print("skip duplicate:", name, "<-", full)
            continue
        seen.add(name)
        out.append((full, name))
    return out


def write_zip(path, entries):
    entries = unique(entries)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for full, name in entries:
            zi = zipfile.ZipInfo(name, time.localtime(MTIME)[:6])
            zi.compress_type = zipfile.ZIP_DEFLATED
            zi.external_attr = ((0o755 if is_exec(name) else 0o644) | 0o100000) << 16
            with open(full, "rb") as f:
                z.writestr(zi, f.read())


def write_tgz(path, entries):
    # gzip のヘッダにも固定の時刻を入れる（作るたびに SHA256 が変わらないように） / a fixed time in the gzip header too
    # (so the SHA256 does not change from build to build)
    entries = unique(entries)
    with open(path, "wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=int(MTIME)) as gz, tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as t:
        dirs = set()
        for _, name in entries:
            parts = name.split("/")[:-1]
            for k in range(1, len(parts) + 1):
                dirs.add("/".join(parts[:k]))
        for d in sorted(dirs):
            ti = tarfile.TarInfo(d)
            ti.type, ti.mode, ti.mtime = tarfile.DIRTYPE, 0o755, MTIME
            t.addfile(ti)
        for full, name in entries:
            data = open(full, "rb").read()
            ti = tarfile.TarInfo(name)
            ti.size, ti.mode, ti.mtime = len(data), (0o755 if is_exec(name) else 0o644), MTIME
            t.addfile(ti, io.BytesIO(data))


def exe(base, plat, name):
    p = os.path.join(base, plat, name + PLATS[plat])
    if not os.path.isfile(p):
        sys.exit("missing: " + p)
    return p


def core(plat):
    """0.1.0 のエンジン（無ければ None） / the 0.1.0 engine (None when there is none)"""
    p = os.path.join(a.core, plat, "fascon" + PLATS[plat])
    if os.path.isfile(p):
        return p
    print(f"note: no 0.1.0 engine for {plat} ({p}): packaged without fascon-core")
    return None


def with_core(plat, dst):
    """fascon-core のエントリ（あれば） / the fascon-core entry, when there is one"""
    c = core(plat)
    return [(c, dst)] if c else []


# ───── VSIX ─────
ext = os.path.join(work, "extension")
shutil.copytree(os.path.join(ROOT, "vscode-extension"), ext, ignore=shutil.ignore_patterns(*NEVER))
# out/extension.js に同梱した第三者のパッケージの著作権表示 / notices of the third-party packages bundled in out/extension.js
shutil.copy(os.path.join(ROOT, "THIRD_PARTY_NOTICES.md"), os.path.join(ext, "THIRD_PARTY_NOTICES.md"))
for f in EXAMPLES:
    shutil.copy(os.path.join(ROOT, "examples", f), os.path.join(ext, "examples", f))
for plat, suffix in PLATS.items():
    d = os.path.join(ext, "bin", plat)
    os.makedirs(d, exist_ok=True)
    shutil.copy(exe(a.bin2, plat, "fascon"), os.path.join(d, "fascon" + suffix))
    if core(plat):
        shutil.copy(core(plat), os.path.join(d, "fascon-core" + suffix))
    for f in os.listdir(d):
        os.chmod(os.path.join(d, f), 0o755)
os.remove(os.path.join(ext, "bin", "README.txt"))
# ボードのパッケージ（fascon build --board <名前> が実行ファイルの 2 つ上の boards/ を探す）と標準ライブラリ・データの写し
# board packages (fascon build --board <name> looks in boards/ two levels above the executable), and copies of the standard
# library and the data files for reference
shutil.copytree(os.path.join(ROOT, "boards"), os.path.join(ext, "boards"), ignore=shutil.ignore_patterns(*NEVER))
shutil.copytree(os.path.join(ROOT, "lib"), os.path.join(ext, "lib"), ignore=shutil.ignore_patterns(*NEVER))
shutil.copytree(os.path.join(ROOT, "data"), os.path.join(ext, "data"), ignore=shutil.ignore_patterns(*NEVER))
vsix = os.path.join(a.out, f"fascon-{VERSION}.vsix")
cmd = shlex.split(a.vsce) + ["package", "--no-dependencies", "--allow-missing-repository", "-o", vsix]
print("$", " ".join(cmd))
subprocess.run(cmd, cwd=ext, check=True)

# ───── 共通の中身 / common payload ─────
def payload(top, linux=False):
    e = []
    for f in ("README.en.txt", "README.ja.txt"):
        e.append((os.path.join(ROOT, "packaging", f), f"{top}/{f}"))
    for f in ("LICENSE-MIT", "LICENSE-APACHE"):
        e.append((os.path.join(ROOT, f), f"{top}/{f}"))
    e.append((os.path.join(ROOT, "THIRD_PARTY_NOTICES.md"), f"{top}/THIRD_PARTY_NOTICES.md"))
    e.append((os.path.join(ROOT, "runtime", "fascon2_rt.h"), f"{top}/include/fascon2_rt.h"))
    e.append((os.path.join(a.legacy, "include", "fascon_rt.h"), f"{top}/include/fascon_rt.h"))
    add_tree(e, os.path.join(ROOT, "bsp"), f"{top}/bsp")
    add_tree(e, os.path.join(ROOT, "boards"), f"{top}/boards")
    add_tree(e, os.path.join(ROOT, "lib"), f"{top}/lib")
    add_tree(e, os.path.join(ROOT, "data"), f"{top}/data")
    for f in EXAMPLES:
        e.append((os.path.join(ROOT, "examples", f), f"{top}/examples/{f}"))
    add_tree(e, os.path.join(ROOT, "tests", "err"), f"{top}/examples/errors")
    add_tree(e, os.path.join(a.legacy, "examples"), f"{top}/examples")
    add_tree(e, os.path.join(ROOT, "docs"), f"{top}/docs")
    # 0.1.0 の文書（LANGUAGE・USAGE）を足す。この repo で名前を変えた古い文書は入れない（SPEC-1.1-SMP.ja.md → SPEC-1.0-SMP.ja.md）
    # add the 0.1.0 documents (LANGUAGE, USAGE); old documents renamed in this repository are left out
    # (SPEC-1.1-SMP.ja.md is now SPEC-1.0-SMP.ja.md)
    add_tree(e, os.path.join(a.legacy, "docs"), f"{top}/docs", skip=LEGACY_DOCS_SUPERSEDED)
    add_tree(e, os.path.join(a.legacy, "runtime"), f"{top}/runtime", skip=() if linux else ("x86_64-linux-host",))
    add_tree(e, os.path.join(a.legacy, "bsp-examples"), f"{top}/bsp-examples")
    add_tree(e, os.path.join(a.legacy, "lsp-config"), f"{top}/lsp-config")
    return e


top = f"fascon-{VERSION}-windows-x64"
e = [(exe(a.bin2, "win32-x64", "fascon"), f"{top}/fascon.exe")] + with_core("win32-x64", f"{top}/fascon-core.exe") + payload(top)
write_zip(os.path.join(a.out, f"{top}.zip"), e)

top = f"fascon-{VERSION}-linux"
e = []
for plat in ("linux-x64", "linux-arm64"):
    e += [(exe(a.bin2, plat, "fascon"), f"{top}/{plat}/fascon")] + with_core(plat, f"{top}/{plat}/fascon-core")
write_tgz(os.path.join(a.out, f"{top}.tar.gz"), e + payload(top, linux=True))

top = f"fascon-{VERSION}-macos-universal"
e = [(exe(a.bin2, "darwin-universal", "fascon"), f"{top}/fascon")] + with_core("darwin-universal", f"{top}/fascon-core") + payload(top)
write_tgz(os.path.join(a.out, f"{top}.tar.gz"), e)

# ───── ソース / source ─────
top = f"fascon-{VERSION}-src"
e = []
for f in ("Cargo.toml", "Cargo.lock", "README.md", "LICENSE-MIT", "LICENSE-APACHE", "CONTRIBUTING.md", "THIRD_PARTY_NOTICES.md", ".gitignore"):
    if os.path.isfile(os.path.join(ROOT, f)):
        e.append((os.path.join(ROOT, f), f"{top}/{f}"))
for d in ("src", "runtime", "bsp", "boards", "lib", "data", "examples", "tests", "bench", "docs", "tools", "packaging", "vscode-extension"):
    add_tree(e, os.path.join(ROOT, d), f"{top}/{d}", skip=("build", "__pycache__"))
write_zip(os.path.join(a.out, f"{top}.zip"), e)

# ───── チェックサム / checksums ─────
names = [f"fascon-{VERSION}.vsix", f"fascon-{VERSION}-src.zip", f"fascon-{VERSION}-windows-x64.zip", f"fascon-{VERSION}-linux.tar.gz", f"fascon-{VERSION}-macos-universal.tar.gz"]
with open(os.path.join(a.out, "SHA256SUMS.txt"), "w", newline="\n") as out:
    for n in names:
        h = hashlib.sha256(open(os.path.join(a.out, n), "rb").read()).hexdigest()
        out.write(f"{h}  {n}\n")
print(open(os.path.join(a.out, "SHA256SUMS.txt")).read())
shutil.rmtree(work, ignore_errors=True)
