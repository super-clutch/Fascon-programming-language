'use strict';
// Fascon 1.0 拡張機能の入口。0.1.0 の拡張機能（./extension.js: 診断・解説・辞書・C 生成）をそのまま起動し、
// 1.0 のコマンド（PC で実行・QEMU で起動（BIOS なし AArch64 / x86_64・AArch64 の仮想 BIOS）・BIOS の像・厳格形・
// 自動リフレーミング・画面の保存・例を開く・ツール確認）を足す。
// Entry point of the Fascon 1.0 extension. It starts the 0.1.0 extension (./extension.js: diagnostics, explanations,
// dictionary, C generation) unchanged and adds the 1.0 commands (run on the PC, boot in QEMU (BIOS-less AArch64 or the
// x86_64 / AArch64 virtual BIOS), BIOS images, the strict form, automatic reframing, save screens, open an example,
// check tools).
const vscode = require('vscode');
const fs = require('fs');
const path = require('path');
const cp = require('child_process');
const legacy = require('./extension.js');

let channel;

function lang() {
  const v = vscode.workspace.getConfiguration('fascon').get('language', 'auto');
  if (v === 'ja' || v === 'en') return v;
  return (vscode.env.language || 'en').toLowerCase().startsWith('ja') ? 'ja' : 'en';
}

function t(ja, en) {
  return lang() === 'ja' ? ja : en;
}

function platDir() {
  if (process.platform === 'win32') return 'win32-x64';
  if (process.platform === 'darwin') return 'darwin-universal';
  return process.arch === 'arm64' ? 'linux-arm64' : 'linux-x64';
}

function exePath(ctx) {
  const cfg = vscode.workspace.getConfiguration('fascon').get('serverPath');
  if (cfg && cfg.trim() !== '') return cfg;
  return ctx.asAbsolutePath(path.join('bin', platDir(), process.platform === 'win32' ? 'fascon.exe' : 'fascon'));
}

// VSIX（zip）では実行権限が落ちることがあるので付け直す / a VSIX (zip) may drop the executable bit, so set it again
function ensureExecutable(ctx) {
  if (process.platform === 'win32') return;
  for (const n of ['fascon', 'fascon-core']) {
    try {
      fs.chmodSync(ctx.asAbsolutePath(path.join('bin', platDir(), n)), 0o755);
    } catch (e) {
      /* 無ければ無視 / ignore if missing */
    }
  }
}

function toolEnv() {
  const extra = vscode.workspace.getConfiguration('fascon').get('extraPath', []) || [];
  const env = Object.assign({}, process.env);
  const key = Object.keys(env).find((k) => k.toLowerCase() === 'path') || 'PATH';
  if (extra.length > 0) env[key] = extra.join(path.delimiter) + path.delimiter + (env[key] || '');
  env.FASCON_LANG = lang();
  env.FASCON_COLUMNS = columnsOn() ? 'on' : 'off';
  // ユニバーサル OS 拡張: OS のプロファイル（rtos / generic-os）と fascon-gc（off = --gc=off）
  // the universal-OS extension: the OS profile (rtos / generic-os) and fascon-gc (off = --gc=off)
  const pf = profileSetting();
  if (pf === 'classic') delete env.FASCON_PROFILE;
  else env.FASCON_PROFILE = pf;
  if (gcSetting() === 'off') env.FASCON_GC = 'off';
  else delete env.FASCON_GC;
  const rf = reframingSetting();
  if (rf === 'auto') delete env.FASCON_REFRAMING;
  else env.FASCON_REFRAMING = rf;
  return env;
}

// 生成 C のターゲット（portable / x86_64 / aarch64） / the target of the generated C
function targetSetting() {
  const v = vscode.workspace.getConfiguration('fascon').get('target', 'portable');
  return v === 'x86_64' || v === 'aarch64' ? v : 'portable';
}

// OS のプロファイル（classic = 既存の C を 1 文字も変えない / rtos / generic-os。ファイル先頭の `// fascon-target:` が優先）
// the OS profile (classic = existing C unchanged / rtos / generic-os; a `// fascon-target:` line in the file wins)
function profileSetting() {
  const v = vscode.workspace.getConfiguration('fascon').get('profile', 'classic');
  return v === 'rtos' || v === 'generic-os' ? v : 'classic';
}

// fascon-gc（on / off）。off は gc_arena を F2623 で拒否する完全決定論のビルド / fascon-gc (on / off); off rejects gc_arena
// with F2623 (a fully deterministic build)
function gcSetting() {
  return vscode.workspace.getConfiguration('fascon').get('gc', 'on') === 'off' ? 'off' : 'on';
}

// 自動リフレーミング（auto = ファイル先頭の #![reframing(rust)] に従う） / automatic reframing (auto = follow the file)
function reframingSetting() {
  const v = vscode.workspace.getConfiguration('fascon').get('reframing', 'auto');
  return v === 'rust' || v === 'off' ? v : 'auto';
}

function targetArgs(t) {
  const v = t || targetSetting();
  return v === 'portable' ? [] : ['--target', v];
}

function reframingArgs() {
  const v = reframingSetting();
  return v === 'auto' ? [] : ['--reframing', v];
}

// 列（集計ごとに、その集計が読む値だけの列を表に足す。既定 on） / columns (one column per reduction holding just the
// values it reads; default on)
function columnsOn() {
  return vscode.workspace.getConfiguration('fascon').get('columns', true) !== false;
}

// fascon run の C を、この PC の CPU 向けにコンパイルするか（既定 native） / whether `fascon run` compiles the C for this PC's
// CPU (default native)
function cpuArgs() {
  const v = vscode.workspace.getConfiguration('fascon').get('cpu', 'native');
  return ['--cpu', v === 'baseline' ? 'baseline' : 'native'];
}

function outDirFor(doc) {
  const od = vscode.workspace.getConfiguration('fascon').get('outDir', 'build') || 'build';
  if (path.isAbsolute(od)) return od;
  const ws = vscode.workspace.getWorkspaceFolder(doc.uri);
  return path.join(ws ? ws.uri.fsPath : path.dirname(doc.uri.fsPath), od);
}

function stemOf(file) {
  const b = path.basename(file);
  for (const ext of ['.fsc.rs', '.fsc', '.fascon', '.rs']) {
    if (b.endsWith(ext)) return b.slice(0, -ext.length);
  }
  return b;
}

async function activeFasconDoc() {
  const ed = vscode.window.activeTextEditor;
  if (!ed || ed.document.languageId !== 'fascon') {
    vscode.window.showWarningMessage(t('Fascon: Fascon のファイルを開いてください', 'Fascon: open a Fascon file first'));
    return undefined;
  }
  if (ed.document.isUntitled) {
    vscode.window.showWarningMessage(t('Fascon: 先にファイルを保存してください（拡張子 .fsc）', 'Fascon: save the file first (extension .fsc)'));
    return undefined;
  }
  await ed.document.save();
  return ed.document;
}

// 子プロセスで fascon を動かし、出力を「Fascon 1.0」チャネルに流す / run fascon as a child process, streaming into the "Fascon 1.0" channel
function runFascon(ctx, args, cwd) {
  return new Promise((resolve) => {
    channel.show(true);
    channel.appendLine('$ fascon ' + args.join(' '));
    let p;
    try {
      p = cp.spawn(exePath(ctx), args, { cwd, env: toolEnv() });
    } catch (e) {
      channel.appendLine(String(e));
      resolve(-1);
      return;
    }
    p.stdout.on('data', (d) => channel.append(d.toString()));
    p.stderr.on('data', (d) => channel.append(d.toString()));
    p.on('error', (e) => {
      channel.appendLine(String(e));
      resolve(-1);
    });
    p.on('close', (code) => {
      channel.appendLine(t(`（終了コード ${code}）`, `(exit code ${code})`));
      resolve(code);
    });
  });
}

async function openImage(file) {
  if (fs.existsSync(file)) await vscode.commands.executeCommand('vscode.open', vscode.Uri.file(file), { viewColumn: vscode.ViewColumn.Beside, preview: false });
}

async function hostRun(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const out = outDirFor(doc);
  const png = path.join(out, stemOf(doc.uri.fsPath) + '.png');
  try {
    fs.unlinkSync(png);
  } catch (e) {
    /* 無ければよい / fine if missing */
  }
  const code = await runFascon(ctx, ['run', doc.uri.fsPath, '--out-dir', out, '--columns', columnsOn() ? 'on' : 'off'].concat(cpuArgs(), reframingArgs()), path.dirname(doc.uri.fsPath));
  if (code === 3) {
    const pick = await vscode.window.showErrorMessage(t('Fascon: C コンパイラが見つかりません', 'Fascon: no C compiler found'), t('ツールを確認', 'Check tools'));
    if (pick) showVersion(ctx);
  }
  await openImage(png);
}

async function qemuRun(ctx, target) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const t2 = target || targetSetting();
  // 対話実行: 端末そのものを fascon にする（PowerShell の引用規則に左右されない）
  // interactive: the terminal process is fascon itself (independent of PowerShell quoting rules)
  const term = vscode.window.createTerminal({
    name: t2 === 'portable' ? 'Fascon QEMU' : `Fascon QEMU ${t2}`,
    shellPath: exePath(ctx),
    shellArgs: ['qemu', doc.uri.fsPath, '--out-dir', outDirFor(doc), '--columns', columnsOn() ? 'on' : 'off'].concat(targetArgs(t2), reframingArgs()),
    cwd: path.dirname(doc.uri.fsPath),
    env: toolEnv(),
  });
  term.show();
  vscode.window.showInformationMessage(
    t(
      'Fascon: QEMU を起動しました。画面は QEMU のウィンドウに出ます。キー入力はこの端末で（終了は Ctrl+A → X、または QEMU のウィンドウを閉じる）',
      'Fascon: QEMU started. The screen appears in the QEMU window; type keys in this terminal (quit with Ctrl+A then X, or close the QEMU window)'
    )
  );
}

async function qemuScreenshot(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const cfg = vscode.workspace.getConfiguration('fascon');
  const frames = Math.max(1, Math.min(20, cfg.get('screenshotFrames', 3)));
  const timeout = Math.max(10, cfg.get('qemuTimeout', 90));
  const out = outDirFor(doc);
  const base = path.join(out, stemOf(doc.uri.fsPath) + '-qemu.png');
  const tg = targetSetting();
  const what = tg === 'x86_64' ? t('x86_64・仮想 BIOS', 'x86_64, virtual BIOS') : tg === 'aarch64' ? t('AArch64・仮想 BIOS', 'AArch64, virtual BIOS') : t('BIOS なし AArch64', 'BIOS-less AArch64');
  const code = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: t(`Fascon: QEMU（${what}）で起動中…`, `Fascon: booting in QEMU (${what})…`) },
    () => runFascon(ctx, ['qemu', doc.uri.fsPath, '--out-dir', out, '--screenshot', base, '--frames', String(frames), '--timeout', String(timeout), '--columns', columnsOn() ? 'on' : 'off'].concat(targetArgs(tg), reframingArgs()), path.dirname(doc.uri.fsPath))
  );
  const shots = [];
  if (frames === 1) shots.push(base);
  else for (let i = 1; i <= frames; i++) shots.push(base.replace(/\.png$/, `-${i}.png`));
  const found = shots.filter((s) => fs.existsSync(s));
  if (found.length === 0) {
    const pick = await vscode.window.showWarningMessage(
      t('Fascon: 画面を保存できませんでした（出力を確認してください）', 'Fascon: no screen was saved (see the output)'),
      t('ツールを確認', 'Check tools')
    );
    if (pick) showVersion(ctx);
    return;
  }
  for (const s of found) await openImage(s);
  vscode.window.showInformationMessage(t(`Fascon: 画面を ${found.length} 枚保存しました（${out}）`, `Fascon: saved ${found.length} screen(s) (${out})`));
  return code;
}

// 1 つのソースから x86_64 と AArch64 の BIOS の像 / BIOS images for x86_64 and AArch64 from one source
async function buildBios(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const out = outDirFor(doc);
  const code = await runFascon(ctx, ['bios', doc.uri.fsPath, '--out-dir', out].concat(reframingArgs()), path.dirname(doc.uri.fsPath));
  if (code === 0) {
    vscode.window.showInformationMessage(t(`Fascon: BIOS の像を作りました（${out}）`, `Fascon: BIOS images built (${out})`));
  } else if (code === 3) {
    const pick = await vscode.window.showErrorMessage(t('Fascon: ベアメタル用の C コンパイラが見つかりません', 'Fascon: no bare-metal C compiler found'), t('ツールを確認', 'Check tools'));
    if (pick) showVersion(ctx);
  }
}

// 厳格な Fascon（機能安全証明形態）を書き出して横に開く / write the strict Fascon form and open it beside the source
async function emitStrict(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const out = outDirFor(doc);
  const code = await runFascon(ctx, ['check', doc.uri.fsPath, '--out-dir', out, '--emit-strict-fascon'].concat(reframingArgs()), path.dirname(doc.uri.fsPath));
  const file = path.join(out, stemOf(doc.uri.fsPath) + '.strict.fsc');
  if (code === 0 && fs.existsSync(file)) {
    const d = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
    await vscode.window.showTextDocument(d, { viewColumn: vscode.ViewColumn.Beside, preview: false });
  } else {
    vscode.window.showWarningMessage(t('Fascon: エラーがあるため厳格形を書き出せません（問題パネルを確認してください）', 'Fascon: the strict form cannot be written while there are errors (see the Problems panel)'));
  }
}

// ───── ユニバーサル OS 拡張のコマンド / commands of the universal-OS extension ─────

// ボードのパッケージを選ぶ（設定の fascon.board、無ければ boards/ の一覧から） / pick a board package (fascon.board, or a
// list of boards/)
async function pickBoard(ctx, doc) {
  const cfg = vscode.workspace.getConfiguration('fascon').get('board', '');
  if (cfg && cfg.trim() !== '') return cfg.trim();
  const roots = [];
  const ws = vscode.workspace.getWorkspaceFolder(doc.uri);
  if (ws) roots.push(path.join(ws.uri.fsPath, 'boards'));
  roots.push(path.join(path.dirname(doc.uri.fsPath), 'boards'));
  roots.push(ctx.asAbsolutePath('boards'));
  const names = new Set();
  for (const r of roots) {
    try {
      for (const e of fs.readdirSync(r)) {
        if (fs.existsSync(path.join(r, e, 'board.toml'))) names.add(e);
        else if (e.endsWith('.toml')) names.add(e.slice(0, -5));
      }
    } catch (e) {
      /* 無ければ次へ / skip missing folders */
    }
  }
  const items = Array.from(names).sort();
  if (items.length === 0) {
    vscode.window.showWarningMessage(t('Fascon: boards/ にボードのパッケージがありません', 'Fascon: no board packages under boards/'));
    return undefined;
  }
  return vscode.window.showQuickPick(items, { placeHolder: t('ボード（board.toml）を選ぶ', 'Pick a board (board.toml)') });
}

// fascon build --board: 実機の像（ELF と生の .img）を作る / build the real-hardware image (ELF and raw .img)
async function buildBoard(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const board = await pickBoard(ctx, doc);
  if (!board) return;
  const out = outDirFor(doc);
  const code = await runFascon(ctx, ['build', doc.uri.fsPath, '--board', board, '--out-dir', out], path.dirname(doc.uri.fsPath));
  if (code === 0) {
    vscode.window.showInformationMessage(t(`Fascon: ${board} の像を作りました（${out}）`, `Fascon: built the ${board} image (${out})`));
  }
}

// fascon qemu --board: その板の QEMU のマシンで起動 / boot it on the board's QEMU machine
async function qemuBoard(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const board = await pickBoard(ctx, doc);
  if (!board) return;
  await runFascon(ctx, ['qemu', '--board', board, doc.uri.fsPath, '--out-dir', outDirFor(doc)], path.dirname(doc.uri.fsPath));
}

// fascon pkg label: パッケージの安全性（Pure Safe / Low-Level / Hardware）を構文から付けて fascon.toml へ
// fascon pkg label: the package safety class (Pure Safe / Low-Level / Hardware) from the syntax into fascon.toml
async function pkgLabel(ctx) {
  const ws = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
  const ed = vscode.window.activeTextEditor;
  const dir = ws ? ws.uri.fsPath : ed ? path.dirname(ed.document.uri.fsPath) : undefined;
  if (!dir) return;
  const code = await runFascon(ctx, ['pkg', 'label', '--dir', dir], dir);
  if (code !== 0) {
    vscode.window.showWarningMessage(t('Fascon: fascon.toml がありません（Fascon: パッケージを作る → fascon pkg init）', 'Fascon: no fascon.toml (create one with fascon pkg init)'));
  }
}

// fascon stack: 呼び出しグラフからの最大スタックの証明（--target rtos） / the maximum-stack proof from the call graph
async function stackReport(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  await runFascon(ctx, ['stack', doc.uri.fsPath, '--out-dir', outDirFor(doc)], path.dirname(doc.uri.fsPath));
}

// generic-os で QEMU に起動（PAN・PAC・BTI・守りページ・W^X の BSP） / boot under QEMU with the generic-os BSP
async function qemuGenericOs(ctx) {
  const doc = await activeFasconDoc();
  if (!doc) return;
  const env = toolEnv();
  env.FASCON_PROFILE = 'generic-os';
  const exe = exePath(ctx);
  channel.show(true);
  channel.appendLine(`$ FASCON_PROFILE=generic-os ${exe} qemu ${doc.uri.fsPath}`);
  await new Promise((resolve) => {
    const p = cp.spawn(exe, ['qemu', doc.uri.fsPath, '--out-dir', outDirFor(doc), '--headless'], { cwd: path.dirname(doc.uri.fsPath), env });
    p.stdout.on('data', (d) => channel.append(d.toString()));
    p.stderr.on('data', (d) => channel.append(d.toString()));
    p.on('close', (c) => resolve(c));
    p.on('error', (e) => {
      channel.appendLine(String(e));
      resolve(-1);
    });
  });
}

async function toggleReframing() {
  const cfg = vscode.workspace.getConfiguration('fascon');
  const cur = reframingSetting();
  const next = cur === 'rust' ? 'auto' : 'rust';
  await cfg.update('reframing', next, vscode.ConfigurationTarget.Workspace);
  vscode.window.showInformationMessage(
    next === 'rust'
      ? t('Fascon: 自動リフレーミングをオンにしました（ふつうの Rust 構文を受け付けます）', 'Fascon: automatic reframing is on (plain Rust syntax is accepted)')
      : t('Fascon: 自動リフレーミングを「ファイルの指定に従う」に戻しました', 'Fascon: automatic reframing follows each file again')
  );
}

async function newExample(ctx) {
  const items = [
    {
      label: 'kernel_core.fsc',
      description: t('ミッション1: task_struct / VFS 風の循環構造（アリーナ・世代タグ・ABA 防止）', 'Mission 1: task_struct / VFS-like cyclic structures (arenas, generation tags, ABA prevention)'),
      file: 'kernel_core.fsc',
    },
    {
      label: 'gui_desktop.fsc',
      description: t('ミッション2: BIOS なし AArch64 の GUI（固定番地 VRAM・own のウィンドウツリー）', 'Mission 2: GUI on BIOS-less AArch64 (fixed-address VRAM, own window tree)'),
      file: 'gui_desktop.fsc',
    },
    {
      label: 'gui_dual.fsc',
      description: t('1 つのソースで x86_64 と AArch64 の GUI（仮想 BIOS・port_io のキー・bump_arena・own_alloc）', 'one GUI source for x86_64 and AArch64 (virtual BIOS, port_io keys, bump_arena, own_alloc)'),
      file: 'gui_dual.fsc',
    },
    {
      label: 'bios_dual.fsc',
      description: t('1 つのソースから x86_64 と AArch64 の仮想 BIOS（COM1 = port_io、POST）', 'one source, two virtual BIOS images (COM1 = port_io, POST)'),
      file: 'bios_dual.fsc',
    },
    {
      label: 'rust_style.rs',
      description: t('自動リフレーミング: ふつうの Rust 構文のままのカーネル', 'automatic reframing: a kernel in plain Rust syntax'),
      file: 'rust_style.rs',
    },
    {
      label: 'app_wordfreq.fsc',
      description: t('一般アプリ: 単語の出現回数（arena・own_alloc・bump_arena）', 'a general application: word frequencies (arena, own_alloc, bump_arena)'),
      file: 'app_wordfreq.fsc',
    },
    {
      label: 'vruntime_scan.fsc',
      description: t('for id in task の vruntime 集計（32 ビット Id = 世代 16 | 添字 16、分岐なしの直線走査）', 'the vruntime sum of for id in task (32-bit Id = generation 16 | index 16, a branch-free straight scan)'),
      file: 'vruntime_scan.fsc',
    },
    {
      label: 'own_callbacks.fsc',
      description: t('構造体の own フィールド（MyContext { buffer: own i32 }・連動解放）と stack_only のコールバック（fn(&mut i32)・F0520）', 'own fields in structs (MyContext { buffer: own i32 }, freed along with the struct) and stack_only callbacks (fn(&mut i32), F0520)'),
      file: 'own_callbacks.fsc',
    },
    {
      label: 'kernel_preempt.fsc',
      description: t('カーネル拡張: タイマー割り込みでのタスク切り替え（task_arena・irq・critical、PC と QEMU AArch64）', 'kernel extensions: task switching by the timer interrupt (task_arena, irq, critical; PC and QEMU AArch64)'),
      file: 'kernel_preempt.fsc',
    },
    {
      label: 'kernel_storage.fsc',
      description: t('カーネル拡張: virtio-blk・ブロックキャッシュ・ファイル（#[wire]・dma・dma_arena、QEMU AArch64 + --disk）', 'kernel extensions: virtio-blk, a block cache and files (#[wire], dma, dma_arena; QEMU AArch64 + --disk)'),
      file: 'kernel_storage.fsc',
    },
    {
      label: 'kernel_net.fsc',
      description: t('カーネル拡張: virtio-net・ゼロコピーの ARP / ICMP エコー応答（QEMU AArch64 + --net）', 'kernel extensions: virtio-net, zero-copy ARP / ICMP echo replies (QEMU AArch64 + --net)'),
      file: 'kernel_net.fsc',
    },
    {
      label: 'kernel_user.fsc',
      description: t('カーネル拡張: ユーザー空間（EL0）とシステムコール（user_mem・syscall・trap。user_init.fsc と像も一緒に保存、QEMU AArch64）', 'kernel extensions: user space (EL0) and system calls (user_mem, syscall, trap; user_init.fsc and its image are saved alongside; QEMU AArch64)'),
      file: 'kernel_user.fsc',
      extra: ['user_init.fsc', 'user_init.bin'],
    },
    {
      label: 'smp_kernel.fsc',
      description: t('1.0 の SMP 拡張: 4 コアのカーネル（コアごとのアリーナ・chan・shared arena・par()、PC と QEMU -smp 4）', 'the SMP extension of 1.0: a kernel on four cores (per-core arenas, chan, shared arena, par(); PC and QEMU -smp 4)'),
      file: 'smp_kernel.fsc',
    },
  ];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: t('開く例を選んでください', 'Pick an example to open') });
  if (!pick) return;
  const text = fs.readFileSync(ctx.asAbsolutePath(path.join('examples', pick.file)), 'utf8');
  const ws = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
  if (ws) {
    const target = await vscode.window.showSaveDialog({
      defaultUri: vscode.Uri.file(path.join(ws.uri.fsPath, pick.file)),
      filters: pick.file.endsWith('.rs') ? { 'Fascon (Rust style)': ['rs'] } : { Fascon: ['fsc'] },
      saveLabel: t('保存して開く', 'Save and open'),
    });
    if (!target) return;
    fs.writeFileSync(target.fsPath, text, 'utf8');
    // 一緒に要るファイル（ユーザープログラムのソースと像）を同じフォルダへ / companion files go to the same folder
    for (const x of pick.extra || []) {
      fs.copyFileSync(ctx.asAbsolutePath(path.join('examples', x)), path.join(path.dirname(target.fsPath), x));
    }
    const doc = await vscode.workspace.openTextDocument(target);
    await vscode.window.showTextDocument(doc);
  } else {
    const doc = await vscode.workspace.openTextDocument({ language: 'fascon', content: text });
    await vscode.window.showTextDocument(doc);
    vscode.window.showInformationMessage(t('Fascon: 実行する前に .fsc として保存してください', 'Fascon: save it as a .fsc file before running it'));
  }
}

async function showVersion(ctx) {
  await runFascon(ctx, ['version'], undefined);
  await runFascon(ctx, ['doctor'], undefined);
}

async function activate(ctx) {
  ensureExecutable(ctx);
  // 言語サーバー（C の生成・リージョンの表示）にも列の設定を渡す（環境変数は起動時に受け継がれる）
  // pass the columns setting to the language server too (C generation, region view; it inherits the environment)
  process.env.FASCON_COLUMNS = columnsOn() ? 'on' : 'off';
  // 言語サーバーへ自動リフレーミングの設定を渡す（auto ならファイル先頭の #![reframing(rust)] に従う）
  // pass the reframing setting to the language server (auto follows #![reframing(rust)] at the top of each file)
  const applyReframingEnv = () => {
    const rf = reframingSetting();
    if (rf === 'auto') delete process.env.FASCON_REFRAMING;
    else process.env.FASCON_REFRAMING = rf;
    // 言語サーバーの「C を生成」にもターゲットと厳格形の設定を渡す / the target and strict-form settings for the server's build
    const tg = targetSetting();
    if (tg === 'portable') delete process.env.FASCON_TARGET;
    else process.env.FASCON_TARGET = tg;
    process.env.FASCON_EMIT_STRICT = vscode.workspace.getConfiguration('fascon').get('emitStrictOnBuild', false) ? '1' : '0';
  };
  applyReframingEnv();
  if (profileSetting() !== 'classic') process.env.FASCON_PROFILE = profileSetting();
  if (gcSetting() === 'off') process.env.FASCON_GC = 'off';
  channel = vscode.window.createOutputChannel('Fascon 1.0');
  ctx.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (!e.affectsConfiguration('fascon.columns') && !e.affectsConfiguration('fascon.reframing') && !e.affectsConfiguration('fascon.target') && !e.affectsConfiguration('fascon.emitStrictOnBuild') && !e.affectsConfiguration('fascon.profile') && !e.affectsConfiguration('fascon.gc')) return;
      process.env.FASCON_COLUMNS = columnsOn() ? 'on' : 'off';
      if (profileSetting() === 'classic') delete process.env.FASCON_PROFILE;
      else process.env.FASCON_PROFILE = profileSetting();
      if (gcSetting() === 'off') process.env.FASCON_GC = 'off';
      else delete process.env.FASCON_GC;
      applyReframingEnv();
      vscode.window
        .showInformationMessage(
          t('Fascon: 設定（列・自動リフレーミング）は言語サーバーを再起動すると診断と C の生成に反映されます', 'Fascon: restart the language server to apply the settings (columns, reframing) to diagnostics and C generation'),
          t('再起動', 'Restart')
        )
        .then((pick) => {
          if (pick) vscode.commands.executeCommand('fascon.restartServer');
        });
    })
  );
  ctx.subscriptions.push(
    channel,
    vscode.commands.registerCommand('fascon.hostRun', () => hostRun(ctx)),
    vscode.commands.registerCommand('fascon.qemuRun', () => qemuRun(ctx)),
    vscode.commands.registerCommand('fascon.qemuRunX86', () => qemuRun(ctx, 'x86_64')),
    vscode.commands.registerCommand('fascon.qemuRunArmBios', () => qemuRun(ctx, 'aarch64')),
    vscode.commands.registerCommand('fascon.buildBios', () => buildBios(ctx)),
    vscode.commands.registerCommand('fascon.emitStrict', () => emitStrict(ctx)),
    vscode.commands.registerCommand('fascon.toggleReframing', () => toggleReframing()),
    vscode.commands.registerCommand('fascon.qemuScreenshot', () => qemuScreenshot(ctx)),
    vscode.commands.registerCommand('fascon.newExample2', () => newExample(ctx)),
    vscode.commands.registerCommand('fascon.showVersion', () => showVersion(ctx)),
    vscode.commands.registerCommand('fascon.buildBoard', () => buildBoard(ctx)),
    vscode.commands.registerCommand('fascon.qemuBoard', () => qemuBoard(ctx)),
    vscode.commands.registerCommand('fascon.pkgLabel', () => pkgLabel(ctx)),
    vscode.commands.registerCommand('fascon.stackReport', () => stackReport(ctx)),
    vscode.commands.registerCommand('fascon.qemuGenericOs', () => qemuGenericOs(ctx))
  );
  // 0.1.0 の拡張機能（言語サーバーは bin/<OS>/fascon lsp = 1.0 の中継サーバー）
  // the 0.1.0 extension (its language server is bin/<OS>/fascon lsp = the 1.0 relay server)
  await legacy.activate(ctx);
}

function deactivate() {
  return legacy.deactivate ? legacy.deactivate() : undefined;
}

module.exports = { activate, deactivate };
