//! `fascon run`（PC 上）と `fascon qemu`（BIOS なし AArch64）の実行器 / runners for `fascon run` (PC) and `fascon qemu` (AArch64, no BIOS)

use crate::assets;
use crate::diag::Lang;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn find_tool(names: &[&str]) -> Option<PathBuf> {
    let exts: Vec<&str> = if cfg!(windows) { vec![".exe", ""] } else { vec![""] };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if cfg!(windows) {
        for d in ["C:\\Program Files\\qemu", "C:\\Program Files\\LLVM\\bin", "C:\\msys64\\mingw64\\bin", "C:\\msys64\\ucrt64\\bin"] {
            dirs.push(PathBuf::from(d));
        }
    }
    if cfg!(target_os = "macos") {
        for d in ["/opt/homebrew/bin", "/opt/homebrew/opt/llvm/bin", "/usr/local/bin", "/usr/local/opt/llvm/bin"] {
            dirs.push(PathBuf::from(d));
        }
    }
    for n in names {
        for d in &dirs {
            for e in &exts {
                let p = d.join(format!("{}{}", n, e));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn run_cmd(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{}: {}", what, e))?;
    if !out.status.success() {
        return Err(format!("{} failed:\n{}{}", what, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// BSP 一式を out_dir に書き出す / write the BSP files into out_dir
pub fn write_bsp(out_dir: &Path) -> std::io::Result<PathBuf> {
    let bsp = out_dir.join("bsp-qemu-virt-aarch64");
    std::fs::create_dir_all(&bsp)?;
    std::fs::write(bsp.join("start.S"), assets::BSP_START_S)?;
    std::fs::write(bsp.join("bsp.c"), assets::BSP_C)?;
    std::fs::write(bsp.join("kernel.ld"), assets::BSP_LD)?;
    std::fs::write(bsp.join("genos.c"), assets::BSP_GENOS_C)?;
    // generic-os: 像を 2 MiB へ（QEMU が RAM の先頭に置く 1 MiB の DTB を潰さない）、.rodata と .data をページ境界へ（W^X）
    // generic-os: the image at +2 MiB (QEMU puts a 1 MiB DTB at the start of RAM) and page-aligned .rodata / .data (W^X)
    let genos_ld = assets::BSP_LD
        .replace(". = 0x40080000;", ". = 0x40200000;")
        .replace(".rodata : ALIGN(16)", ".rodata : ALIGN(4096)")
        .replace(".data : ALIGN(16)", ".data : ALIGN(4096)");
    std::fs::write(bsp.join("kernel-genos.ld"), genos_ld)?;
    std::fs::write(bsp.join("firmware.S"), assets::BSP_FW_S)?;
    std::fs::write(bsp.join("firmware.ld"), assets::BSP_FW_LD)?;
    Ok(bsp)
}

/// x86_64 の仮想 BIOS の BSP を out_dir に書き出す / write the x86_64 virtual-BIOS BSP into out_dir
pub fn write_bsp_x86(out_dir: &Path) -> std::io::Result<PathBuf> {
    let bsp = out_dir.join("bsp-qemu-x86_64-bios");
    std::fs::create_dir_all(&bsp)?;
    std::fs::write(bsp.join("bios.S"), assets::BSP_X86_S)?;
    std::fs::write(bsp.join("bsp.c"), assets::BSP_X86_C)?;
    std::fs::write(bsp.join("bios.ld"), assets::BSP_X86_LD)?;
    Ok(bsp)
}

// ───────────── ELF → ROM の像 / ELF → ROM image ─────────────

/// ELF64（リトルエンディアン）の PT_LOAD のうち、ファイルに中身があるもの: (物理アドレス = LMA, 中身)
/// the PT_LOAD segments of an ELF64 (little-endian) file that have file contents: (physical address = LMA, bytes)
fn elf_segments(d: &[u8]) -> Result<Vec<(u64, Vec<u8>)>, String> {
    let rd16 = |o: usize| -> Option<u16> { d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };
    let rd32 = |o: usize| -> Option<u32> { d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    let rd64 = |o: usize| -> Option<u64> { d.get(o..o + 8).map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])) };
    if d.len() < 64 || &d[0..4] != b"\x7fELF" || d[4] != 2 || d[5] != 1 {
        return Err("not an ELF64 little-endian file".into());
    }
    let phoff = rd64(0x20).ok_or("bad ELF")? as usize;
    let phentsize = rd16(0x36).ok_or("bad ELF")? as usize;
    let phnum = rd16(0x38).ok_or("bad ELF")? as usize;
    let mut out = Vec::new();
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        let ty = rd32(o).ok_or("bad ELF")?;
        let off = rd64(o + 8).ok_or("bad ELF")? as usize;
        let paddr = rd64(o + 24).ok_or("bad ELF")?;
        let filesz = rd64(o + 32).ok_or("bad ELF")? as usize;
        if ty == 1 && filesz > 0 {
            let bytes = d.get(off..off + filesz).ok_or("ELF segment out of range")?.to_vec();
            out.push((paddr, bytes));
        }
    }
    Ok(out)
}

/// [base, end) の範囲の像を作る（範囲外の部分は捨てる。空きは 0） / build the image of [base, end) (other parts are dropped;
/// gaps are zero)
fn rom_image(segs: &[(u64, Vec<u8>)], base: u64, end: u64) -> Vec<u8> {
    let mut img = vec![0u8; (end - base) as usize];
    for (p, b) in segs {
        if *p >= base && *p + b.len() as u64 <= end {
            let o = (*p - base) as usize;
            img[o..o + b.len()].copy_from_slice(b);
        }
    }
    img
}

/// ベアメタルの C コンパイラ（clang + ld.lld、または GCC）。triple は clang の --target、gcc は候補の名前
/// a bare-metal C toolchain (clang + ld.lld, or GCC); triple is clang's --target, gcc lists candidate names
enum Toolchain {
    Llvm(PathBuf, PathBuf, &'static str),
    Gcc(PathBuf),
}

fn toolchain(triple: &'static str, gccs: &[&str], cc_pref: Option<&str>) -> Option<Toolchain> {
    let want_gcc = cc_pref == Some("gcc");
    if !want_gcc {
        let clang = find_tool(&["clang", "clang-18", "clang-17", "clang-16", "clang-19", "clang-20"]);
        let lld = find_tool(&["ld.lld", "ld.lld-18", "ld.lld-17", "ld.lld-19", "ld.lld-20", "lld"]);
        if let (Some(c), Some(l)) = (clang, lld) {
            return Some(Toolchain::Llvm(c, l, triple));
        }
    }
    find_tool(gccs).map(Toolchain::Gcc)
}

impl Toolchain {
    fn compile(&self, flags: &[&str], inc: &str, src: &Path, obj: &Path) -> Result<(), String> {
        let mut c = match self {
            Toolchain::Llvm(clang, _, t) => {
                let mut c = Command::new(clang);
                c.arg(format!("--target={}", t));
                c
            }
            Toolchain::Gcc(g) => Command::new(g),
        };
        c.args(flags).arg(inc).arg("-c").arg(src).arg("-o").arg(obj);
        run_cmd(&mut c, &format!("cc {}", src.display()))
    }
    fn link(&self, script: &Path, objs: &[PathBuf], out: &Path) -> Result<(), String> {
        let mut l = match self {
            Toolchain::Llvm(_, lld, _) => {
                let mut l = Command::new(lld);
                l.arg("-T").arg(script).arg("-nostdlib").arg("-static");
                l
            }
            Toolchain::Gcc(g) => {
                let mut l = Command::new(g);
                l.args(["-nostdlib", "-static", "-fno-pie", "-no-pie", "-Wl,--build-id=none"]).arg("-T").arg(script);
                l
            }
        };
        l.args(objs);
        if let Toolchain::Gcc(_) = self {
            l.arg("-lgcc");
        }
        l.arg("-o").arg(out);
        run_cmd(&mut l, "link")
    }
}

const X86_FLAGS: [&str; 17] = [
    "-O2",
    "-std=c11",
    "-ffreestanding",
    "-fno-builtin",
    "-nostdlib",
    "-mno-red-zone",
    "-fno-pic",
    "-fno-pie",
    "-fno-stack-protector",
    "-fno-asynchronous-unwind-tables",
    "-mcmodel=small",
    "-march=x86-64",
    "-Wall",
    "-Wextra",
    "-DFASCON2_FREESTANDING",
    "-DFASCON2_HEAP_SIZE=64",
    "-DFASCON2_FRAME_SIZE=64",
];

/// x86_64 の仮想 BIOS の像（QEMU の -bios）を作る。ROM の大きさはカーネル像の大きさから決める（1 回目のリンクで測り、
/// 2 回目で像を起動コードの直前に置く。像は 64 KiB の倍数 = QEMU の決まり）
/// build the x86_64 virtual-BIOS image (QEMU's -bios). The ROM size follows the kernel image: the first link measures it,
/// the second places the image right before the boot code (the image is a multiple of 64 KiB, as QEMU requires)
pub fn build_x86_64(c_file: &Path, out_dir: &Path, name: &str, lang: Lang, cc_pref: Option<&str>) -> Result<PathBuf, String> {
    let bsp = write_bsp_x86(out_dir).map_err(|e| e.to_string())?;
    let tc = toolchain("x86_64-none-elf", &["x86_64-elf-gcc", "x86_64-none-elf-gcc", "x86_64-linux-gnu-gcc", "x86_64-unknown-linux-gnu-gcc"], cc_pref)
        .or_else(|| if cfg!(target_arch = "x86_64") { find_tool(&["gcc", "cc"]).map(Toolchain::Gcc) } else { None })
        .ok_or_else(|| {
            lang.pick(
                "x86_64 向けの C コンパイラが見つかりません。clang + ld.lld（LLVM）か x86_64-elf-gcc / x86_64-linux-gnu-gcc を入れてください",
                "no C compiler for x86_64 found: install clang + ld.lld (LLVM) or x86_64-elf-gcc / x86_64-linux-gnu-gcc",
            )
            .to_string()
        })?;
    let inc = format!("-I{}", out_dir.display());
    // マルチコアのプログラム: BSP も同じ定義で翻訳する（コアごとの実体は TLS。FS の土台からの固定の位置で読む）。プロファイル
    // （rtos など）の定義も BSP に渡して ABI をそろえる
    // a multi-core program: the BSP is compiled with the same definition (per-core instances are TLS at fixed offsets
    // from the FS base); profile definitions (rtos ...) reach the BSP too so the ABI matches
    let mut flags: Vec<String> = X86_FLAGS.iter().map(|s| s.to_string()).collect();
    if is_smp_c(c_file) {
        flags.extend(["-DFASCON2_SMP=1".to_string(), "-ftls-model=local-exec".to_string()]);
    }
    flags.extend(profile_defs(c_file));
    flags.extend(std::env::var("FASCON_CFLAGS").map(|x| x.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap_or_default());
    let flags: Vec<&str> = flags.iter().map(|s| s.as_str()).collect();
    let mut objs = Vec::new();
    for (src, obj) in [(bsp.join("bios.S"), "x86_bios.o"), (bsp.join("bsp.c"), "x86_bsp.o"), (c_file.to_path_buf(), "x86_app.o")] {
        let o = out_dir.join(obj);
        tc.compile(&flags, &inc, &src, &o)?;
        objs.push(o);
    }
    let script = std::fs::read_to_string(bsp.join("bios.ld")).map_err(|e| e.to_string())?;
    let elf = out_dir.join(format!("{}.x86_64.elf", name));
    // 1 回目: 仮の ROM の先頭（4 GiB - 16 MiB）で測る / pass 1: measure with a provisional ROM base (4 GiB - 16 MiB)
    const PROBE: u64 = 0xFF00_0000;
    const STUB: u64 = 0xFFFF_0000;
    let ld1 = out_dir.join("x86_bios.probe.ld");
    std::fs::write(&ld1, script.replace("@ROM_BASE@", &format!("{:#x}", PROBE))).map_err(|e| e.to_string())?;
    tc.link(&ld1, &objs, &elf).map_err(|e| format!("{}\n{}", lang.pick("（カーネル像が ROM（16 MiB 弱）に収まらない可能性があります）", "(the kernel image may not fit in the ROM, just under 16 MiB)"), e))?;
    let segs = elf_segments(&std::fs::read(&elf).map_err(|e| e.to_string())?)?;
    let img_end = segs.iter().filter(|(p, _)| *p >= PROBE && *p < STUB).map(|(p, b)| *p + b.len() as u64).max().unwrap_or(PROBE);
    let size = (img_end - PROBE + 0xFFFF) & !0xFFFF;
    let base = STUB - size;
    // 2 回目: 像を起動コードの直前へ / pass 2: the image right before the boot code
    let ld2 = out_dir.join("x86_bios.ld");
    std::fs::write(&ld2, script.replace("@ROM_BASE@", &format!("{:#x}", base))).map_err(|e| e.to_string())?;
    tc.link(&ld2, &objs, &elf)?;
    let segs = elf_segments(&std::fs::read(&elf).map_err(|e| e.to_string())?)?;
    let img = rom_image(&segs, base, 0x1_0000_0000);
    if img.len() < 16 || img[img.len() - 16] != 0xE9 && img[img.len() - 16] != 0xEB {
        return Err("x86_64 BIOS: the reset vector is missing".into());
    }
    let bin = out_dir.join(format!("{}.x86_64.bios.bin", name));
    std::fs::write(&bin, &img).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&ld1);
    Ok(bin)
}

/// AArch64 の仮想 BIOS の像（QEMU virt の -bios。フラッシュの 0 番地から走り、カーネル像を RAM へ写して _start へ）
/// build the AArch64 virtual-BIOS image (QEMU virt's -bios: runs from flash at 0, copies the kernel image to RAM, jumps to _start)
pub fn build_aarch64_bios(c_file: &Path, out_dir: &Path, name: &str, lang: Lang, cc_pref: Option<&str>) -> Result<PathBuf, String> {
    let bsp = write_bsp(out_dir).map_err(|e| e.to_string())?;
    let tc = toolchain("aarch64-none-elf", &["aarch64-none-elf-gcc", "aarch64-elf-gcc", "aarch64-linux-gnu-gcc", "aarch64-unknown-linux-gnu-gcc"], cc_pref).ok_or_else(|| {
        lang.pick(
            "AArch64 向けの C コンパイラが見つかりません。clang + ld.lld（LLVM）か aarch64-none-elf-gcc / aarch64-linux-gnu-gcc を入れてください",
            "no C compiler for AArch64 found: install clang + ld.lld (LLVM) or aarch64-none-elf-gcc / aarch64-linux-gnu-gcc",
        )
        .to_string()
    })?;
    let flags = ["-mcpu=cortex-a72", "-O2", "-std=c11", "-ffreestanding", "-fno-builtin", "-nostdlib", "-mstrict-align", "-Wall", "-Wextra", "-DFASCON2_FREESTANDING", "-DFASCON2_HEAP_SIZE=64", "-DFASCON2_FRAME_SIZE=64"];
    let inc = format!("-I{}", out_dir.display());
    let mut objs = Vec::new();
    for (src, obj) in [(bsp.join("firmware.S"), "a64_fw.o"), (bsp.join("start.S"), "a64_start.o"), (bsp.join("bsp.c"), "a64_bsp.o"), (c_file.to_path_buf(), "a64_app.o")] {
        let o = out_dir.join(obj);
        tc.compile(&flags, &inc, &src, &o)?;
        objs.push(o);
    }
    let elf = out_dir.join(format!("{}.aarch64.elf", name));
    tc.link(&bsp.join("firmware.ld"), &objs, &elf)?;
    let segs = elf_segments(&std::fs::read(&elf).map_err(|e| e.to_string())?)?;
    const FLASH_END: u64 = 0x0400_0000; // QEMU virt の flash0 は 64 MiB / QEMU virt's flash0 is 64 MiB
    let end = segs.iter().filter(|(p, _)| *p < FLASH_END).map(|(p, b)| *p + b.len() as u64).max().unwrap_or(0);
    if end > FLASH_END {
        return Err(lang.pick("カーネル像がフラッシュ（64 MiB）に収まりません", "the kernel image does not fit in flash (64 MiB)").to_string());
    }
    let img = rom_image(&segs, 0, (end + 0xFFFF) & !0xFFFF);
    let bin = out_dir.join(format!("{}.aarch64.bios.bin", name));
    std::fs::write(&bin, &img).map_err(|e| e.to_string())?;
    Ok(bin)
}

/// ユーザープログラム（EL0）の像を作る: ELF をリンクして、USER_BASE から始まる平らなバイナリ（.text .rodata .data）にする
/// build a user program (EL0) image: link an ELF and flatten it into a binary starting at USER_BASE (.text .rodata .data)
pub fn build_user_aarch64(c_file: &Path, out_dir: &Path, name: &str, base: u64, size: u64, lang: Lang, cc_pref: Option<&str>) -> Result<PathBuf, String> {
    let bsp = out_dir.join("bsp-user-aarch64");
    let wr = |f: &str, t: &str| std::fs::write(bsp.join(f), t);
    std::fs::create_dir_all(&bsp).and_then(|_| wr("crt0.S", assets::USER_CRT0_S)).and_then(|_| wr("user.c", assets::USER_C)).and_then(|_| wr("user.ld", assets::USER_LD)).map_err(|e| e.to_string())?;
    let elf = out_dir.join(format!("{}.user.elf", name));
    let inc = format!("-I{}", out_dir.display());
    let common = ["-mcpu=cortex-a72", "-Os", "-std=c11", "-ffreestanding", "-fno-builtin", "-nostdlib", "-mstrict-align", "-ffunction-sections", "-fdata-sections", "-Wall", "-Wextra", "-DFASCON2_FREESTANDING", "-DFASCON2_USER=1", "-DFASCON2_HEAP_SIZE=64", "-DFASCON2_FRAME_SIZE=64"];
    let defs = [format!("--defsym=USER_BASE={:#x}", base), format!("--defsym=USER_SIZE={:#x}", size)];
    let want_gcc = cc_pref == Some("gcc");
    let clang = if want_gcc { None } else { find_tool(&["clang", "clang-18", "clang-17", "clang-16", "clang-19"]) };
    let lld = find_tool(&["ld.lld", "ld.lld-18", "ld.lld-17", "lld"]);
    if let (Some(clang), Some(lld)) = (clang, lld) {
        let mut objs = Vec::new();
        for (src, obj) in [(bsp.join("crt0.S"), "ucrt0.o"), (bsp.join("user.c"), "user.o"), (c_file.to_path_buf(), "uapp.o")] {
            let o = out_dir.join(obj);
            let mut c = Command::new(&clang);
            c.arg("--target=aarch64-none-elf").args(common).arg(&inc).arg("-c").arg(&src).arg("-o").arg(&o);
            run_cmd(&mut c, &format!("clang {}", src.display()))?;
            objs.push(o);
        }
        let mut l = Command::new(&lld);
        l.arg("-T").arg(bsp.join("user.ld")).args(&defs).args(["-nostdlib", "-static", "--gc-sections"]).args(&objs).arg("-o").arg(&elf);
        run_cmd(&mut l, "ld.lld")?;
    } else if let Some(gcc) = find_tool(&["aarch64-none-elf-gcc", "aarch64-elf-gcc", "aarch64-linux-gnu-gcc", "aarch64-unknown-linux-gnu-gcc"]) {
        let mut c = Command::new(&gcc);
        c.args(common).args(["-fno-pie", "-no-pie", "-static", "-Wl,--build-id=none", "-Wl,--gc-sections"]);
        for d in &defs {
            c.arg(format!("-Wl,{}", d));
        }
        c.arg(&inc).arg("-T").arg(bsp.join("user.ld")).arg(bsp.join("crt0.S")).arg(bsp.join("user.c")).arg(c_file).arg("-lgcc").arg("-o").arg(&elf);
        run_cmd(&mut c, "aarch64 gcc")?;
    } else {
        return Err(lang
            .pick(
                "AArch64 向けの C コンパイラが見つかりません。clang + ld.lld（LLVM）か aarch64-none-elf-gcc / aarch64-linux-gnu-gcc を入れてください",
                "no C compiler for AArch64 found: install clang + ld.lld (LLVM) or aarch64-none-elf-gcc / aarch64-linux-gnu-gcc",
            )
            .to_string());
    }
    let data = std::fs::read(&elf).map_err(|e| e.to_string())?;
    let segs = elf_segments(&data)?;
    let end = segs.iter().filter(|(p, _)| *p >= base && *p < base + size).map(|(p, b)| *p + b.len() as u64).max().unwrap_or(base);
    if end <= base {
        return Err("the user program image is empty".into());
    }
    let img = rom_image(&segs, base, end);
    let bin = out_dir.join(format!("{}.bin", name));
    std::fs::write(&bin, img).map_err(|e| e.to_string())?;
    Ok(bin)
}

/// x86_64 のユーザープログラム（Ring 3）の像を作る: 位置独立のコード（USER_BASE は 4 GiB 以上なので、32 ビットの絶対番地は
/// 使えない）を USER_BASE に置いてリンクし、平らなバイナリにする。配置（user.ld）は AArch64 と同じもの
/// build a user program image for x86_64 (ring 3): position-independent code (USER_BASE lies at or above 4 GiB, so 32-bit
/// absolute addresses are out) linked at USER_BASE and flattened; the layout (user.ld) is the same as on AArch64
pub fn build_user_x86_64(c_file: &Path, out_dir: &Path, name: &str, base: u64, size: u64, lang: Lang, cc_pref: Option<&str>) -> Result<PathBuf, String> {
    let bsp = out_dir.join("bsp-user-x86_64");
    let wr = |f: &str, t: &str| std::fs::write(bsp.join(f), t);
    std::fs::create_dir_all(&bsp).and_then(|_| wr("crt0.S", assets::USER_X86_CRT0_S)).and_then(|_| wr("user.c", assets::USER_X86_C)).and_then(|_| wr("user.ld", assets::USER_LD)).map_err(|e| e.to_string())?;
    let elf = out_dir.join(format!("{}.user.elf", name));
    let inc = format!("-I{}", out_dir.display());
    let common = [
        "-O2",
        "-std=c11",
        "-ffreestanding",
        "-fno-builtin",
        "-nostdlib",
        "-fpie",
        "-fno-stack-protector",
        "-fno-asynchronous-unwind-tables",
        "-march=x86-64",
        "-ffunction-sections",
        "-fdata-sections",
        "-Wall",
        "-Wextra",
        "-DFASCON2_FREESTANDING",
        "-DFASCON2_USER=1",
        "-DFASCON2_HEAP_SIZE=64",
        "-DFASCON2_FRAME_SIZE=64",
    ];
    let defs = [format!("--defsym=USER_BASE={:#x}", base), format!("--defsym=USER_SIZE={:#x}", size)];
    let want_gcc = cc_pref == Some("gcc");
    let clang = if want_gcc { None } else { find_tool(&["clang", "clang-18", "clang-17", "clang-16", "clang-19"]) };
    let lld = find_tool(&["ld.lld", "ld.lld-18", "ld.lld-17", "lld"]);
    if let (Some(clang), Some(lld)) = (clang, lld) {
        let mut objs = Vec::new();
        for (src, obj) in [(bsp.join("crt0.S"), "ux86_crt0.o"), (bsp.join("user.c"), "ux86_user.o"), (c_file.to_path_buf(), "ux86_app.o")] {
            let o = out_dir.join(obj);
            let mut c = Command::new(&clang);
            c.arg("--target=x86_64-none-elf").args(common).arg(&inc).arg("-c").arg(&src).arg("-o").arg(&o);
            run_cmd(&mut c, &format!("clang {}", src.display()))?;
            objs.push(o);
        }
        let mut l = Command::new(&lld);
        l.arg("-T").arg(bsp.join("user.ld")).args(&defs).args(["-nostdlib", "-static", "--no-pie", "--gc-sections"]).args(&objs).arg("-o").arg(&elf);
        run_cmd(&mut l, "ld.lld")?;
    } else if let Some(gcc) = find_tool(&["x86_64-elf-gcc", "x86_64-none-elf-gcc", "x86_64-linux-gnu-gcc", "x86_64-unknown-linux-gnu-gcc"]).or_else(|| if cfg!(target_arch = "x86_64") { find_tool(&["gcc", "cc"]) } else { None }) {
        let mut c = Command::new(&gcc);
        c.args(common).args(["-static", "-no-pie", "-Wl,--build-id=none", "-Wl,--gc-sections"]);
        for d in &defs {
            c.arg(format!("-Wl,{}", d));
        }
        c.arg(&inc).arg("-T").arg(bsp.join("user.ld")).arg(bsp.join("crt0.S")).arg(bsp.join("user.c")).arg(c_file).arg("-lgcc").arg("-o").arg(&elf);
        run_cmd(&mut c, "x86_64 gcc")?;
    } else {
        return Err(lang
            .pick(
                "x86_64 向けの C コンパイラが見つかりません。clang + ld.lld（LLVM）か x86_64-elf-gcc / x86_64-linux-gnu-gcc を入れてください",
                "no C compiler for x86_64 found: install clang + ld.lld (LLVM) or x86_64-elf-gcc / x86_64-linux-gnu-gcc",
            )
            .to_string());
    }
    let data = std::fs::read(&elf).map_err(|e| e.to_string())?;
    let segs = elf_segments(&data)?;
    let end = segs.iter().filter(|(p, _)| *p >= base && *p < base + size).map(|(p, b)| *p + b.len() as u64).max().unwrap_or(base);
    if end <= base {
        return Err("the user program image is empty".into());
    }
    let img = rom_image(&segs, base, end);
    let bin = out_dir.join(format!("{}.bin", name));
    std::fs::write(&bin, img).map_err(|e| e.to_string())?;
    Ok(bin)
}

/// 生成 C がマルチコア（`#define FASCON2_SMP 1`）か / whether the generated C is multi-core
pub fn is_smp_c(c_file: &Path) -> bool {
    std::fs::read_to_string(c_file).map_or(false, |t| t.lines().take(40).any(|l| l.starts_with("#define FASCON2_SMP 1")))
}

/// 生成 C の先頭にあるプロファイルの #define（`--target rtos` / `--target generic-os`）を、BSP・プラットフォーム層の翻訳にも
/// 同じ値で渡す -D の列にする（Id の幅などの ABI を揃える）。Classic のプログラムでは空
/// turn the profile #defines at the top of the generated C (`--target rtos` / `--target generic-os`) into -D flags for the
/// BSP and platform-layer translation units too (so the ABI, such as the Id width, matches); empty for Classic programs
pub fn profile_defs(c_file: &Path) -> Vec<String> {
    const KEYS: [&str; 9] = ["FASCON2_RTOS", "FASCON2_GENERIC_OS", "FASCON2_ID64", "FASCON2_UNIVERSAL", "FASCON2_BOARD", "FASCON2_GC", "FASCON2_GPU", "FASCON2_GPU_OFF", "FASCON2_HPC"];
    let mut out = Vec::new();
    if let Ok(t) = std::fs::read_to_string(c_file) {
        for l in t.lines().take(40) {
            for k in KEYS {
                if l.starts_with(&format!("#define {} 1", k)) {
                    out.push(format!("-D{}=1", k));
                }
            }
            // --target hpc: コアの数の上限も同じ値に（BSP のコアの表の大きさ） / --target hpc: the same core limit too (the
            // size of the BSP's core table)
            if let Some(v) = l.strip_prefix("#define FASCON2_MAX_CPUS ") {
                if let Some(n) = v.split_whitespace().next() {
                    out.push(format!("-DFASCON2_MAX_CPUS={}", n));
                }
            }
        }
    }
    out
}

/// 生成 C が generic-os か / whether the generated C is for generic-os
pub fn is_generic_os_c(c_file: &Path) -> bool {
    profile_defs(c_file).iter().any(|d| d == "-DFASCON2_GENERIC_OS=1")
}

/// ベアメタル AArch64 の ELF を作る / build a bare-metal AArch64 ELF
pub fn build_aarch64(c_file: &Path, out_dir: &Path, name: &str, lang: Lang, cc_pref: Option<&str>) -> Result<PathBuf, String> {
    let bsp = write_bsp(out_dir).map_err(|e| e.to_string())?;
    let elf = out_dir.join(format!("{}.elf", name));
    let inc = format!("-I{}", out_dir.display());
    let mut common = vec!["-mcpu=cortex-a72", "-O2", "-std=c11", "-ffreestanding", "-fno-builtin", "-nostdlib", "-mstrict-align", "-Wall", "-Wextra", "-DFASCON2_FREESTANDING", "-DFASCON2_HEAP_SIZE=64", "-DFASCON2_FRAME_SIZE=64"];
    // マルチコアのプログラム: BSP も同じ定義で翻訳する（コアごとの実体は TLS。TP からの固定の位置で読む）
    // a multi-core program: the BSP is compiled with the same definition (per-core instances are TLS at fixed offsets from TP)
    if is_smp_c(c_file) {
        common.extend(["-DFASCON2_SMP=1", "-ftls-model=local-exec"]);
    }
    let pdefs = profile_defs(c_file);
    common.extend(pdefs.iter().map(|s| s.as_str()));
    // generic-os: PAC と BTI（命令はヒント空間なので古いコアでは NOP）、アリーナの仮想番地の予約、装置の高位の写し、
    // 大きなフレームでも守りページを飛び越さないスタックの探り（4 KiB ごと）
    // generic-os: PAC and BTI (hint-space instructions, NOPs on older cores), VA-reserved arenas, the high device mirror,
    // and stack probes every 4 KiB so large frames cannot jump over a guard page
    let extra: Vec<String> = std::env::var("FASCON_CFLAGS").map(|x| x.split_whitespace().map(|s| s.to_string()).collect()).unwrap_or_default();
    common.extend(extra.iter().map(|s| s.as_str()));
    let genos = is_generic_os_c(c_file);
    if genos {
        common.extend(["-mbranch-protection=standard", "-fstack-clash-protection", "-DFASCON2_GENOS_VA=1", "-DFASCON2_ARENA_VA_BASE=0x5000000000ull", "-DFASCON2_MMIO_MIRROR=0x6000000000ull"]);
    }
    let common = common;
    let want_gcc = cc_pref == Some("gcc");
    let clang = if want_gcc { None } else { find_tool(&["clang", "clang-18", "clang-17", "clang-16", "clang-19"]) };
    let lld = find_tool(&["ld.lld", "ld.lld-18", "ld.lld-17", "lld"]);
    if let (Some(clang), Some(lld)) = (clang, lld) {
        let mut objs = Vec::new();
        for (src, obj) in [(bsp.join("start.S"), "start.o"), (bsp.join("bsp.c"), "bsp.o"), (c_file.to_path_buf(), "app.o")] {
            let o = out_dir.join(obj);
            let mut c = Command::new(&clang);
            c.arg("--target=aarch64-none-elf").args(&common).arg(&inc).arg("-c").arg(&src).arg("-o").arg(&o);
            run_cmd(&mut c, &format!("clang {}", src.display()))?;
            objs.push(o);
        }
        let mut l = Command::new(&lld);
        l.arg("-T").arg(bsp.join(if genos { "kernel-genos.ld" } else { "kernel.ld" })).arg("-nostdlib").arg("-static").args(&objs).arg("-o").arg(&elf);
        run_cmd(&mut l, "ld.lld")?;
        return Ok(elf);
    }
    if let Some(gcc) = find_tool(&["aarch64-none-elf-gcc", "aarch64-elf-gcc", "aarch64-linux-gnu-gcc", "aarch64-unknown-linux-gnu-gcc"]) {
        let mut c = Command::new(&gcc);
        c.args(&common);
        if genos {
            c.args(["--param", "stack-clash-protection-guard-size=12"]);
        }
        c.args(["-fno-pie", "-no-pie", "-static", "-Wl,--build-id=none"])
            .arg(&inc)
            .arg("-T")
            .arg(bsp.join(if genos { "kernel-genos.ld" } else { "kernel.ld" }))
            .arg(bsp.join("start.S"))
            .arg(bsp.join("bsp.c"))
            .arg(c_file)
            .arg("-lgcc")
            .arg("-o")
            .arg(&elf);
        run_cmd(&mut c, "aarch64 gcc")?;
        return Ok(elf);
    }
    Err(lang
        .pick(
            "AArch64 向けの C コンパイラが見つかりません。clang + ld.lld（LLVM）か aarch64-none-elf-gcc / aarch64-linux-gnu-gcc を入れてください",
            "no C compiler for AArch64 found: install clang + ld.lld (LLVM) or aarch64-none-elf-gcc / aarch64-linux-gnu-gcc",
        )
        .to_string())
}

/// PC 用にビルドする。native なら、この PC の CPU の命令（AVX2 / AVX-512・POPCNT など）を使う C にする（列の走査のベクトルの
/// 幅はこれで決まる）。そのフラグを受け付けないコンパイラなら、フラグなしでやり直す
/// build for the PC; with native, the C is compiled for this PC's CPU (AVX2 / AVX-512, POPCNT, ...; this sets the vector
/// width of the column scans). A compiler that rejects the flag gets a second try without it
/// 生成 C が gpu_arena を使い、装置へ出すか（PC ではワーカースレッドで真似る: -pthread が要る）
/// whether the generated C uses a gpu_arena and offloads (the PC emulates it with a worker thread: -pthread is needed)
pub fn is_gpu_c(c_file: &Path) -> bool {
    let d = profile_defs(c_file);
    d.iter().any(|x| x == "-DFASCON2_GPU=1") && !d.iter().any(|x| x == "-DFASCON2_GPU_OFF=1")
}

/// `extra`: 一緒にコンパイルする C のファイル（`<名前>.gpu.c` の GPU カーネルなど） / `extra`: C files compiled along (GPU
/// kernels in `<name>.gpu.c`, ...)
pub fn build_host(c_file: &Path, out_dir: &Path, name: &str, lang: Lang, native: bool, extra: &[PathBuf]) -> Result<PathBuf, String> {
    let shim = out_dir.join("fascon2_plat_host.c");
    std::fs::write(&shim, assets::HOST_SHIM).map_err(|e| e.to_string())?;
    let exe = out_dir.join(if cfg!(windows) { format!("{}.exe", name) } else { name.to_string() });
    let cc = find_tool(&["cc", "gcc", "clang"]).ok_or_else(|| {
        lang.pick(
            "PC 用の C コンパイラ（cc / gcc / clang）が見つかりません（Windows: MSYS2 の gcc か、LLVM + Visual Studio Build Tools）",
            "no C compiler for the PC (cc / gcc / clang) found (Windows: gcc from MSYS2, or LLVM + Visual Studio Build Tools)",
        )
        .to_string()
    })?;
    // 試す順のフラグの組（最後は何も足さない）。Windows の GCC（MinGW）は、AVX の 32 B の値をスタックへ退避するとき 16 B
    // にしか揃っていないスタックへ整列前提の命令を出すことがある（GCC PR 54412）ので、アセンブラに整列前提のベクトル移動を
    // 整列なしの命令へ置き換えさせる（binutils 2.38 以降。受け付けない clang などは次の組へ）
    // flag sets to try in order (the last adds nothing). GCC on Windows (MinGW) may emit alignment-assuming instructions
    // when it spills 32-byte AVX values to a stack that is only 16-byte aligned (GCC PR 54412), so the assembler is asked
    // to turn aligned vector moves into unaligned ones (binutils 2.38+; compilers that reject it, clang say, go to the next set)
    let cpu: Option<&str> = if !native {
        None
    } else if cfg!(target_arch = "x86_64") || cfg!(target_arch = "x86") {
        Some("-march=native")
    } else if cfg!(target_arch = "aarch64") {
        Some("-mcpu=native")
    } else {
        None
    };
    let mut tries: Vec<Vec<&str>> = Vec::new();
    if let Some(f) = cpu {
        if cfg!(windows) && f == "-march=native" {
            tries.push(vec![f, "-Wa,-muse-unaligned-vector-move"]);
        }
        tries.push(vec![f]);
    }
    tries.push(Vec::new());
    let smp = is_smp_c(c_file);
    let gpu = is_gpu_c(c_file);
    let build = |flags: &[&str]| -> Result<(), String> {
        let mut c = Command::new(&cc);
        c.args(["-O2", "-std=c11", "-Wall", "-Wextra", "-DFASCON2_HOSTED_MMIO"]);
        // マルチコアのプログラム: PC ではコア = スレッド（コアごとの実体はスレッドごとの実体になる）
        // a multi-core program: on the PC a core is a thread (per-core instances become per-thread instances)
        if smp {
            c.arg("-DFASCON2_SMP=1");
            if !cfg!(windows) {
                c.arg("-pthread");
            }
        }
        // GPU の真似のワーカースレッド / the worker thread of the GPU emulation
        if gpu && !smp && !cfg!(windows) {
            c.arg("-pthread");
        }
        c.args(profile_defs(c_file));
        c.args(flags);
        // FASCON_CFLAGS: 追加の C のフラグ（例: -DFASCON2_GC_TRACE） / extra C flags (e.g. -DFASCON2_GC_TRACE)
        if let Ok(x) = std::env::var("FASCON_CFLAGS") {
            c.args(x.split_whitespace());
        }
        c.arg(format!("-I{}", out_dir.display())).arg(c_file).arg(&shim).args(extra).arg("-o").arg(&exe);
        if cfg!(windows) {
            c.arg("-D_CRT_SECURE_NO_WARNINGS");
        }
        run_cmd(&mut c, "cc")
    };
    let mut last = Ok(());
    for t in &tries {
        last = build(t);
        if last.is_ok() {
            break;
        }
    }
    last?;
    Ok(exe)
}

// ───────────── PNG（固定ハフマンの deflate） / PNG (deflate with fixed Huffman codes) ─────────────

fn crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

struct BitW {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitW {
    fn put(&mut self, v: u32, bits: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// ハフマン符号は上位ビットから書く / Huffman codes are written most significant bit first
    fn put_code(&mut self, code: u32, bits: u32) {
        let mut r = 0;
        for i in 0..bits {
            r = (r << 1) | ((code >> i) & 1);
        }
        self.put(r, bits);
    }
    fn lit(&mut self, v: u32) {
        match v {
            0..=143 => self.put_code(0x30 + v, 8),
            144..=255 => self.put_code(0x190 + v - 144, 9),
            256..=279 => self.put_code(v - 256, 7),
            _ => self.put_code(0xC0 + v - 280, 8),
        }
    }
}

const LEN_BASE: [u32; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u32; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u32; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// zlib 形式（固定ハフマン + 単純な LZ77） / zlib stream (fixed Huffman + simple LZ77)
fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut w = BitW { out: vec![0x78, 0x01], acc: 0, n: 0 };
    w.put(1, 1); // BFINAL
    w.put(1, 2); // BTYPE = 01（固定ハフマン / fixed Huffman）
    const HBITS: u32 = 15;
    let mut head = vec![0usize; 1 << HBITS];
    let hash = |i: usize| -> usize { (((data[i] as u32) << 16 | (data[i + 1] as u32) << 8 | data[i + 2] as u32).wrapping_mul(2_654_435_761) >> (32 - HBITS)) as usize };
    let mut i = 0;
    while i < data.len() {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + 3 <= data.len() {
            let h = hash(i);
            let cand = head[h];
            head[h] = i + 1;
            if cand > 0 {
                let c = cand - 1;
                let dist = i - c;
                if dist > 0 && dist <= 32768 {
                    let max = (data.len() - i).min(258);
                    let mut l = 0;
                    while l < max && data[c + l] == data[i + l] {
                        l += 1;
                    }
                    if l >= 3 {
                        best_len = l;
                        best_dist = dist;
                    }
                }
            }
        }
        if best_len >= 3 {
            let li = LEN_BASE.iter().rposition(|&b| b as usize <= best_len).unwrap();
            w.lit(257 + li as u32);
            w.put(best_len as u32 - LEN_BASE[li], LEN_EXTRA[li]);
            let di = DIST_BASE.iter().rposition(|&b| b as usize <= best_dist).unwrap();
            w.put_code(di as u32, 5);
            w.put(best_dist as u32 - DIST_BASE[di], DIST_EXTRA[di]);
            // 一致の中の位置もハッシュに入れる / also hash the positions inside the match
            for k in i + 1..(i + best_len).min(data.len().saturating_sub(2)) {
                head[hash(k)] = k + 1;
            }
            i += best_len;
        } else {
            w.lit(data[i] as u32);
            i += 1;
        }
    }
    w.lit(256);
    if w.n > 0 {
        w.out.push(w.acc as u8);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = w.out;
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    z
}

pub fn png_from_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0u8);
        raw.extend_from_slice(&rgb[y * w as usize * 3..(y + 1) * w as usize * 3]);
    }
    let z = zlib_compress(&raw);
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let chunk = |ty: &[u8], data: &[u8], png: &mut Vec<u8>| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = ty.to_vec();
        c.extend_from_slice(data);
        png.extend_from_slice(&c);
        png.extend_from_slice(&crc32(&c, 0).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr, &mut png);
    chunk(b"IDAT", &z, &mut png);
    chunk(b"IEND", &[], &mut png);
    png
}

/// PPM（P6）を PNG に変換する / convert a PPM (P6) image to PNG
pub fn ppm_to_png(ppm: &[u8]) -> Option<Vec<u8>> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < ppm.len() {
        while i < ppm.len() && (ppm[i] as char).is_whitespace() {
            i += 1;
        }
        if i < ppm.len() && ppm[i] == b'#' {
            while i < ppm.len() && ppm[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let st = i;
        while i < ppm.len() && !(ppm[i] as char).is_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&ppm[st..i]).to_string());
    }
    if fields.len() < 4 || fields[0] != "P6" {
        return None;
    }
    let w: u32 = fields[1].parse().ok()?;
    let h: u32 = fields[2].parse().ok()?;
    i += 1;
    let need = (w * h * 3) as usize;
    if ppm.len() < i + need {
        return None;
    }
    Some(png_from_rgb(w, h, &ppm[i..i + need]))
}

// ───────────── QEMU ─────────────

pub struct QemuOpts {
    pub headless: bool,
    pub screenshot: Option<PathBuf>,
    pub frames: u32,
    pub timeout_s: u64,
    pub framebuffer: bool,
    /// virtio-blk のディスク像（AArch64 の virt だけ） / the virtio-blk disk image (AArch64 virt only)
    pub disk: Option<PathBuf>,
    /// virtio-net の相手: `udp:相手のポート:自分のポート`（127.0.0.1 の UDP に生の Ethernet フレームを流す）
    /// the virtio-net peer: `udp:peer_port:local_port` (raw Ethernet frames over UDP on 127.0.0.1)
    pub net: Option<String>,
    /// コアの数（0 = 指定なし = 1 コア） / the number of cores (0 = unset = one core)
    pub smp: u32,
    /// generic-os: `-cpu max`（PAN・PAC・BTI を持つコア）と PCIe の装置 1 つ / `-cpu max` (a core with PAN, PAC and BTI) and one PCIe device
    pub generic_os: bool,
}

/// 起動のしかた / how to boot
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Boot {
    /// QEMU virt（AArch64）に ELF を -kernel で渡す（BIOS なし） / QEMU virt (AArch64) with the ELF passed by -kernel (no BIOS)
    VirtKernel,
    /// QEMU virt（AArch64）に Fascon の仮想 BIOS（-bios） / QEMU virt (AArch64) with the Fascon virtual BIOS (-bios)
    VirtBios,
    /// QEMU pc（x86_64）に Fascon の仮想 BIOS（-bios） / QEMU pc (x86_64) with the Fascon virtual BIOS (-bios)
    PcBios,
}

fn qmp_screendump(port: u16, path: &Path) -> Result<(), String> {
    let mut s = None;
    for _ in 0..100 {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(x) => {
                s = Some(x);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    let s = s.ok_or("QMP connection failed")?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    let mut r = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
    let mut w = s;
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    w.write_all(b"{\"execute\":\"qmp_capabilities\"}\n").map_err(|e| e.to_string())?;
    line.clear();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let ppm = path.with_extension("ppm");
    let p = ppm.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    w.write_all(format!("{{\"execute\":\"screendump\",\"arguments\":{{\"filename\":\"{}\"}}}}\n", p).as_bytes()).map_err(|e| e.to_string())?;
    loop {
        line.clear();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            break;
        }
        if line.contains("\"return\"") || line.contains("\"error\"") {
            break;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    let data = std::fs::read(&ppm).map_err(|e| format!("screendump: {}", e))?;
    let png = ppm_to_png(&data).ok_or("screendump: unexpected image format")?;
    std::fs::write(path, png).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&ppm);
    Ok(())
}

pub fn run_qemu_boot(elf: &Path, boot: Boot, o: &QemuOpts, lang: Lang) -> Result<i32, String> {
    let qemu = if boot == Boot::PcBios {
        find_tool(&["qemu-system-x86_64"]).ok_or_else(|| {
            lang.pick(
                "qemu-system-x86_64 が見つかりません（Windows: QEMU for Windows、macOS: brew install qemu、Linux: qemu-system-x86 パッケージ）",
                "qemu-system-x86_64 not found (Windows: QEMU for Windows, macOS: brew install qemu, Linux: the qemu-system-x86 package)",
            )
            .to_string()
        })?
    } else {
        find_tool(&["qemu-system-aarch64"]).ok_or_else(|| {
            lang.pick(
                "qemu-system-aarch64 が見つかりません（Windows: QEMU for Windows、macOS: brew install qemu、Linux: qemu-system-arm パッケージ）",
                "qemu-system-aarch64 not found (Windows: QEMU for Windows, macOS: brew install qemu, Linux: the qemu-system-arm package)",
            )
            .to_string()
        })?
    };
    let mut cmd = Command::new(&qemu);
    match boot {
        Boot::PcBios => {
            // isa-debug-exit: 0xF4 へ書いた値 v で QEMU は (v << 1) | 1 で終わる / QEMU ends with (v << 1) | 1 for a write of v to 0xF4
            // -cpu max: TCG が真似られる機能を全部（BSP は PCID・INVPCID・SMEP・SMAP・NX を CPUID で見つけて使う）
            // -cpu max: every feature TCG can emulate (the BSP finds PCID, INVPCID, SMEP, SMAP and NX through CPUID)
            cmd.args(["-M", "pc", "-cpu", "max", "-m", "2G", "-nic", "none", "-no-reboot", "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-vga", "none"]);
            if o.smp > 1 {
                cmd.args(["-smp", &o.smp.to_string()]);
            }
        }
        _ => {
            let cpu = if o.generic_os { "max,pauth-impdef=on" } else { "cortex-a72" };
            cmd.args(["-M", "virt", "-cpu", cpu, "-m", "256M", "-nic", "none", "-semihosting"]);
            if o.generic_os {
                cmd.args(["-device", "virtio-rng-pci"]);
            }
            if o.smp > 1 {
                cmd.args(["-smp", &o.smp.to_string()]);
            }
            // virtio のデバイス（virtio-mmio。レガシーではなく仕様 1.x の形に固定する）。窓はバスの名前で固定する:
            // ディスクは窓 31（0x0a003e00、INTID 79）、ネットは窓 30（0x0a003c00、INTID 78）。片方だけでも同じ窓に付く
            // virtio devices (virtio-mmio, pinned to the 1.x layout instead of legacy). The transports are pinned by bus name:
            // the disk at transport 31 (0x0a003e00, INTID 79), the network at 30 (0x0a003c00, INTID 78), also when only one
            // of them is present
            if o.disk.is_some() || o.net.is_some() {
                cmd.args(["-global", "virtio-mmio.force-legacy=false"]);
            }
            if let Some(d) = &o.disk {
                cmd.arg("-drive").arg(format!("file={},if=none,format=raw,id=fsdisk", d.display()));
                cmd.args(["-device", "virtio-blk-device,drive=fsdisk,bus=virtio-mmio-bus.31"]);
            }
            if let Some(n) = &o.net {
                let mut it = n.trim_start_matches("udp:").split(':');
                let peer: u16 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                let local: u16 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                if peer == 0 || local == 0 {
                    return Err(lang.pick("--net は `udp:相手のポート:自分のポート` の形で指定します（例: --net udp:5601:5602）", "--net takes `udp:peer_port:local_port` (e.g. --net udp:5601:5602)").to_string());
                }
                cmd.arg("-netdev").arg(format!("socket,id=fsnet,udp=127.0.0.1:{},localaddr=127.0.0.1:{}", peer, local));
                cmd.args(["-device", "virtio-net-device,netdev=fsnet,mac=52:54:00:12:34:56,bus=virtio-mmio-bus.30"]);
            }
        }
    }
    if o.framebuffer {
        cmd.args(["-device", "ramfb"]);
    }
    let port = 45000 + (std::process::id() % 1000) as u16;
    if o.headless || !o.framebuffer {
        if o.framebuffer {
            cmd.args(["-display", "none", "-serial", "stdio"]);
            cmd.arg("-qmp").arg(format!("tcp:127.0.0.1:{},server,wait=off", port));
        } else {
            cmd.arg("-nographic");
        }
    } else {
        // 対話: 画面は QEMU のウィンドウ、UART は端末（Ctrl+A → X で終了） / interactive: screen in the QEMU window, UART in the terminal (Ctrl+A then X quits)
        cmd.args(["-serial", "mon:stdio"]);
    }
    match boot {
        Boot::VirtKernel => cmd.arg("-kernel").arg(elf),
        Boot::VirtBios | Boot::PcBios => cmd.arg("-bios").arg(elf),
    };
    eprintln!("[fascon] {} {}", qemu.display(), cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect::<Vec<_>>().join(" "));
    if !(o.headless && o.framebuffer) {
        // 対話実行: 端末をそのまま QEMU に渡す / interactive: hand the terminal to QEMU
        let st = cmd.status().map_err(|e| e.to_string())?;
        let code = st.code().unwrap_or(1);
        // x86_64: isa-debug-exit の (コード << 1) | 1 を元のコードへ戻す / x86_64: turn isa-debug-exit's (code << 1) | 1 back
        if boot == Boot::PcBios && code & 1 == 1 {
            return Ok(code >> 1);
        }
        return Ok(code);
    }
    // ヘッドレス: UART の目印ごとに画面を保存してキーを送る / headless: save the screen at each UART marker, then send a key
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let start = std::time::Instant::now();
    let mut line: Vec<u8> = Vec::new();
    let mut shots = 0u32;
    let mut byte = [0u8; 1];
    let mut code = 0;
    loop {
        if start.elapsed().as_secs() > o.timeout_s {
            eprintln!("[fascon] timeout");
            break;
        }
        match stdout.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                std::io::stdout().write_all(&byte).ok();
                if byte[0] == b'\n' {
                    let l = String::from_utf8_lossy(&line).to_string();
                    line.clear();
                    if l.contains("FASCON-GUI-READY") {
                        shots += 1;
                        let base = o.screenshot.clone().unwrap_or_else(|| elf.with_extension("png"));
                        let path = if o.frames > 1 {
                            let stem = base.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                            base.with_file_name(format!("{}-{}.png", stem, shots))
                        } else {
                            base
                        };
                        match qmp_screendump(port, &path) {
                            Ok(()) => eprintln!("[fascon] {} {}", lang.pick("画面を保存:", "screen saved:"), path.display()),
                            Err(e) => eprintln!("[fascon] screendump: {}", e),
                        }
                        if shots >= o.frames {
                            break;
                        }
                        stdin.write_all(b"n").ok();
                        stdin.flush().ok();
                    }
                    if l.contains("[bsp] exit code") {
                        code = l.trim().rsplit(' ').next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    }
                } else {
                    line.push(byte[0]);
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(code)
}

// ───────────── fascon doctor ─────────────

/// 必要なツールがそろっているかを調べる / check that the tools needed are installed
pub fn doctor(lang: Lang) -> (String, bool) {
    let mut out = String::new();
    let mut all = true;
    let mut line = |ok: bool, what: &str, detail: String, hint: &str| {
        out.push_str(&format!("  [{}] {:<34} {}\n", if ok { "OK" } else { "--" }, what, detail));
        if !ok && !hint.is_empty() {
            out.push_str(&format!("       {}\n", hint));
        }
    };
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    line(true, &format!("fascon {}", crate::driver::VERSION), exe, "");
    match crate::legacy::core_path() {
        Some(p) => line(true, lang.pick("0.1 エンジン（fascon-core）", "0.1 engine (fascon-core)"), p.display().to_string(), ""),
        None => {
            all = false;
            line(false, lang.pick("0.1 エンジン（fascon-core）", "0.1 engine (fascon-core)"), String::new(), lang.pick("0.1 だけの構文（spawn・chan・async・#[isr]）のファイルに必要。fascon と同じフォルダに置く", "needed for 0.1-only files (spawn, chan, async, #[isr]); put it next to fascon"))
        }
    }
    let host = find_tool(&["cc", "gcc", "clang"]);
    if host.is_none() {
        all = false;
    }
    line(
        host.is_some(),
        lang.pick("PC 用 C コンパイラ（fascon run）", "C compiler for the PC (fascon run)"),
        host.map(|p| p.display().to_string()).unwrap_or_default(),
        lang.pick("Windows: MSYS2 の gcc（pacman -S mingw-w64-ucrt-x86_64-gcc）/ macOS: xcode-select --install / Linux: gcc", "Windows: gcc from MSYS2 (pacman -S mingw-w64-ucrt-x86_64-gcc) / macOS: xcode-select --install / Linux: gcc"),
    );
    let clang = find_tool(&["clang", "clang-18", "clang-17", "clang-16", "clang-19"]);
    let lld = find_tool(&["ld.lld", "ld.lld-18", "ld.lld-17", "lld"]);
    let gcc = find_tool(&["aarch64-none-elf-gcc", "aarch64-elf-gcc", "aarch64-linux-gnu-gcc", "aarch64-unknown-linux-gnu-gcc"]);
    let a64 = match (&clang, &lld, &gcc) {
        (Some(c), Some(l), _) => Some(format!("{} + {}", c.display(), l.display())),
        (_, _, Some(g)) => Some(g.display().to_string()),
        _ => None,
    };
    if a64.is_none() {
        all = false;
    }
    line(
        a64.is_some(),
        lang.pick("AArch64 用 C コンパイラ（fascon qemu）", "C compiler for AArch64 (fascon qemu)"),
        a64.unwrap_or_default(),
        lang.pick("LLVM（clang + ld.lld）を入れる: Windows は winget install LLVM.LLVM、macOS は brew install llvm lld、Linux は clang lld", "install LLVM (clang + ld.lld): Windows: winget install LLVM.LLVM, macOS: brew install llvm lld, Linux: clang lld"),
    );
    let q = find_tool(&["qemu-system-aarch64"]);
    if q.is_none() {
        all = false;
    }
    line(
        q.is_some(),
        "QEMU (qemu-system-aarch64)",
        q.map(|p| p.display().to_string()).unwrap_or_default(),
        lang.pick("Windows: winget install SoftwareFreedomConservancy.QEMU / macOS: brew install qemu / Linux: qemu-system-arm", "Windows: winget install SoftwareFreedomConservancy.QEMU / macOS: brew install qemu / Linux: qemu-system-arm"),
    );
    // x86_64 の仮想 BIOS（fascon qemu --target x86_64 / fascon bios） / the x86_64 virtual BIOS
    let gx = find_tool(&["x86_64-elf-gcc", "x86_64-none-elf-gcc", "x86_64-linux-gnu-gcc", "x86_64-unknown-linux-gnu-gcc"]).or_else(|| if cfg!(target_arch = "x86_64") && !cfg!(windows) && !cfg!(target_os = "macos") { find_tool(&["gcc"]) } else { None });
    let x64 = match (&clang, &lld, &gx) {
        (Some(c), Some(l), _) => Some(format!("{} + {}", c.display(), l.display())),
        (_, _, Some(g)) => Some(g.display().to_string()),
        _ => None,
    };
    if x64.is_none() {
        all = false;
    }
    line(
        x64.is_some(),
        lang.pick("x86_64 用 C コンパイラ（仮想 BIOS）", "C compiler for x86_64 (virtual BIOS)"),
        x64.unwrap_or_default(),
        lang.pick("LLVM（clang + ld.lld）が 1 つで x86_64 と AArch64 の両方に使える", "LLVM (clang + ld.lld) covers both x86_64 and AArch64"),
    );
    let qx = find_tool(&["qemu-system-x86_64"]);
    if qx.is_none() {
        all = false;
    }
    line(
        qx.is_some(),
        "QEMU (qemu-system-x86_64)",
        qx.map(|p| p.display().to_string()).unwrap_or_default(),
        lang.pick("Windows: winget install SoftwareFreedomConservancy.QEMU / macOS: brew install qemu / Linux: qemu-system-x86", "Windows: winget install SoftwareFreedomConservancy.QEMU / macOS: brew install qemu / Linux: qemu-system-x86"),
    );
    (out, all)
}

#[cfg(test)]
mod tests {
    #[test]
    fn zlib_vectors() {
        // 圧縮結果を書き出し、外の zlib で展開して確かめる / write streams out; an external zlib checks them
        let dir = std::env::var("FASCON_ZTEST_DIR").unwrap_or_else(|_| "/tmp/fascon-ztest".into());
        std::fs::create_dir_all(&dir).unwrap();
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (seed >> 16) as u8
        };
        let mut cases: Vec<Vec<u8>> = vec![vec![], vec![7], vec![1, 2], vec![0; 3], vec![0; 1000], (0..=255u8).collect()];
        cases.push((0..70000).map(|_| rnd()).collect());
        cases.push((0..70000).map(|i| (i % 7) as u8).collect());
        cases.push((0..100000).map(|_| rnd() % 3).collect());
        let mut v = Vec::new();
        for k in 0..5000 {
            v.extend_from_slice(&[(k % 256) as u8, 1, 2, 3, 4, 5]);
        }
        cases.push(v);
        for (i, c) in cases.iter().enumerate() {
            std::fs::write(format!("{}/{}.raw", dir, i), c).unwrap();
            std::fs::write(format!("{}/{}.z", dir, i), super::zlib_compress(c)).unwrap();
        }
    }
}

/// board.toml の [boot] から実機の像を作る（汎用 AArch64 BSP: start.S・bsp.c・生成したリンカスクリプト）。
/// ELF と、フラッシュ・ブートメディアへそのまま書ける生の像（.img = objcopy -O binary と同じ）を返す
/// build a real-hardware image from [boot] of board.toml (the generic AArch64 BSP: start.S, bsp.c and a generated linker
/// script); returns the ELF and a raw image ready for flash / boot media (.img = what objcopy -O binary gives)
pub fn build_board_image(c_file: &Path, out_dir: &Path, name: &str, b: &crate::board::Board, lang: Lang, cc_pref: Option<&str>) -> Result<(PathBuf, PathBuf), String> {
    let generic = b.arch_name.is_empty() || b.arch_name == "aarch64" || b.arch_name == "arm64";
    let pkg_has_bsp = std::env::var("FASCON_BOARD_DIR").ok().map_or(false, |d| Path::new(&d).join("start.S").is_file() && Path::new(&d).join("bsp.c").is_file());
    if !generic && (b.toolchain.is_none() || !pkg_has_bsp) {
        return Err(lang
            .pick(
                "汎用 BSP は AArch64 用です。ほかのアーキテクチャは、ボードのパッケージに start.S・bsp.c（と [toolchain]）を置いてください",
                "the generic BSP is for AArch64; for other architectures put start.S and bsp.c (and [toolchain]) in the board package",
            )
            .to_string());
    }
    let dir = out_dir.join(format!("bsp-{}", b.name));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // ボードのパッケージに start.S / bsp.c / link.ld があればそれを使う（コンパイラは変えずに差し替えられる）
    // a board package may carry its own start.S / bsp.c / link.ld (replaced without touching the compiler)
    let pkg_dir: Option<PathBuf> = std::env::var("FASCON_BOARD_DIR").ok().map(PathBuf::from);
    let pick = |f: &str, def: &str| -> Result<PathBuf, String> {
        let dst = dir.join(f);
        if let Some(p) = pkg_dir.as_ref().map(|d| d.join(f)).filter(|p| p.is_file()) {
            std::fs::copy(&p, &dst).map_err(|e| e.to_string())?;
        } else {
            std::fs::write(&dst, def).map_err(|e| e.to_string())?;
        }
        Ok(dst)
    };
    let start = pick("start.S", crate::assets::GEN_A64_START)?;
    let bspc = pick("bsp.c", crate::assets::GEN_A64_BSP)?;
    let ld = pick("link.ld", &crate::board::linker_script(b))?;
    let elf = out_dir.join(format!("{}.{}.elf", name, b.name));
    let img = out_dir.join(format!("{}.{}.img", name, b.name));
    let inc = format!("-I{}", out_dir.display());
    let mut common: Vec<String> = ["-mcpu=cortex-a53", "-O2", "-std=c11", "-ffreestanding", "-fno-builtin", "-nostdlib", "-mstrict-align", "-Wall", "-Wextra", "-DFASCON2_FREESTANDING", "-DFASCON2_HEAP_SIZE=64", "-DFASCON2_FRAME_SIZE=64", "-DFASCON2_BOARD=1"].iter().map(|s| s.to_string()).collect();
    if is_smp_c(c_file) {
        common.extend(["-DFASCON2_SMP=1".to_string(), "-ftls-model=local-exec".to_string()]);
    }
    common.extend(profile_defs(c_file));
    if let Ok(x) = std::env::var("FASCON_CFLAGS") {
        common.extend(x.split_whitespace().map(|s| s.to_string()));
    }
    // ボードのパッケージの [toolchain]（RISC-V など）: その cc・ld・cflags で翻訳する / a package [toolchain] (RISC-V, ...)
    if let Some((cc, ldn, cflags)) = b.toolchain.clone().filter(|_| !generic) {
        common.retain(|f| f != "-mcpu=cortex-a53" && f != "-mstrict-align");
        let ccp = find_tool(&[cc.as_str(), "clang", "clang-18"]).ok_or_else(|| format!("{} not found", cc))?;
        let ldp = find_tool(&[ldn.as_str(), "ld.lld", "ld.lld-18"]).ok_or_else(|| format!("{} not found", ldn))?;
        let mut objs = Vec::new();
        for (src, obj) in [(&start, "start.o"), (&bspc, "bsp.o"), (&c_file.to_path_buf(), "app.o")] {
            let o = dir.join(obj);
            let mut c = Command::new(&ccp);
            c.args(&cflags).args(&common).arg(&inc).arg(format!("-I{}", dir.display())).arg("-c").arg(src).arg("-o").arg(&o);
            run_cmd(&mut c, &cc)?;
            objs.push(o);
        }
        let mut l = Command::new(&ldp);
        l.arg("-T").arg(&ld).args(&objs).arg("-o").arg(&elf);
        run_cmd(&mut l, &ldn)?;
        let bytes = std::fs::read(&elf).map_err(|e| e.to_string())?;
        let (_, raw) = crate::board::elf_to_bin(&bytes)?;
        std::fs::write(&img, raw).map_err(|e| e.to_string())?;
        return Ok((elf, img));
    }
    let want_gcc = cc_pref == Some("gcc");
    let clang = if want_gcc { None } else { find_tool(&["clang", "clang-18", "clang-17", "clang-16", "clang-19"]) };
    let lld = find_tool(&["ld.lld", "ld.lld-18", "ld.lld-17", "lld"]);
    if let (Some(clang), Some(lld)) = (clang, lld) {
        let mut objs = Vec::new();
        for (src, obj) in [(&start, "start.o"), (&bspc, "bsp.o"), (&c_file.to_path_buf(), "app.o")] {
            let o = dir.join(obj);
            let mut c = Command::new(&clang);
            c.args(["--target=aarch64-none-elf"]).args(&common).arg(&inc).arg(format!("-I{}", dir.display())).arg("-c").arg(src).arg("-o").arg(&o);
            run_cmd(&mut c, "clang")?;
            objs.push(o);
        }
        let mut l = Command::new(&lld);
        l.arg("-T").arg(&ld).args(&objs).arg("-o").arg(&elf);
        run_cmd(&mut l, "ld.lld")?;
    } else if let Some(gcc) = find_tool(&["aarch64-linux-gnu-gcc", "aarch64-none-elf-gcc", "aarch64-elf-gcc"]) {
        let mut c = Command::new(&gcc);
        c.args(&common).args(["-fno-pie", "-no-pie", "-static", "-Wl,--build-id=none"]).arg(&inc).arg(format!("-I{}", dir.display())).arg("-T").arg(&ld).arg(&start).arg(&bspc).arg(c_file).arg("-lgcc").arg("-o").arg(&elf);
        run_cmd(&mut c, "gcc")?;
    } else {
        return Err(lang.pick("AArch64 の C コンパイラが見つかりません（clang + ld.lld か aarch64-linux-gnu-gcc）", "no AArch64 C compiler found (clang + ld.lld or aarch64-linux-gnu-gcc)").to_string());
    }
    let bytes = std::fs::read(&elf).map_err(|e| e.to_string())?;
    let (_, raw) = crate::board::elf_to_bin(&bytes)?;
    std::fs::write(&img, raw).map_err(|e| e.to_string())?;
    Ok((elf, img))
}
