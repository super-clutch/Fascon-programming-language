/*
 * fascon2_rt.h — Fascon 1.0 runtime (header-only, freestanding C11)
 *
 *   - 世代タグ付きアリーナ（Id = 世代 16 ビット << 16 | 添字 16 ビット）
 *     generational arenas (Id = 16-bit generation << 16 | 16-bit slot index)
 *   - own T の所有リージョン（サイズ別プール・O(1) 確保と一括解放）
 *     own-T regions (size-class pools, O(1) allocation and bulk free)
 *   - 固定番地リージョン（MMIO / フレームバッファ）の境界検査ヘルパ
 *     bounds-checked helpers for fixed-address regions (MMIO / framebuffers)
 *   - 検査付き整数演算（C の未定義動作を出さない） / checked integer arithmetic (no C undefined behaviour)
 *
 * 生成コード（*.fascon.c）は FASCON2_RT_IMPLEMENTATION を定義してから 1 回だけ include する。
 * BSP など他のファイルは宣言だけを include する。
 * The generated file (*.fascon.c) defines FASCON2_RT_IMPLEMENTATION and includes this header once;
 * other files (BSPs) include it for the declarations only.
 *
 * プラットフォーム境界（BSP が実装。0.1 の BSP と同じ名前）:
 * platform boundary (implemented by the BSP; same names as 0.1 BSPs):
 *   void fascon_plat_print(const char *s, size_t len);
 *   FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg);
 *   FASCON_NORETURN void fascon_plat_exit(int code);      (1.0)
 *   FASCON_NORETURN void fascon_plat_halt(void);          (1.0)
 */
#ifndef FASCON2_RT_H
#define FASCON2_RT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FASCON2_RT_VERSION 0x020000

#ifndef FASCON_LIKELY
#define FASCON_LIKELY(x) __builtin_expect(!!(x), 1)
#define FASCON_UNLIKELY(x) __builtin_expect(!!(x), 0)
#endif
#ifndef FASCON_NORETURN
#define FASCON_NORETURN _Noreturn
#endif
/* 生成コードの補助関数は使わなくても警告しない / generated helpers may legitimately go unused */
#define FASCON_MAYBE_UNUSED __attribute__((unused))

/* f32 / f64 を使う生成 C（FASCON2_FLOAT）: a * b + c を FMA に縮約させない（Fascon の浮動小数点は書いた順の IEEE-754 の
 * 演算そのもの = どの CPU・最適化でも同じビット）。再結合は #[fast_math] の集計の走査関数だけが明示的に行う
 * generated C that uses f32 / f64 (FASCON2_FLOAT): never contract a * b + c into an FMA (Fascon floats are the IEEE-754
 * operations as written = the same bits on every CPU and optimisation level); only the scan functions of #[fast_math]
 * reductions reassociate, explicitly */
#if defined(FASCON2_FLOAT) && defined(FASCON2_RT_IMPLEMENTATION)
#if defined(__clang__)
#pragma STDC FP_CONTRACT OFF
#pragma clang fp contract(off)
#elif defined(__GNUC__)
#pragma GCC optimize("fp-contract=off")
#endif
#endif

/* Id の型（下の「32 ビット Id と世代タグ付きアリーナ」の節を参照）。--target generic-os が生成した C は
 * FASCON2_ID64 を定義して 64 ビット [上位 32 = 世代 | 下位 32 = 添字] にする（0ULL = None）。プラットフォーム層の
 * 関数もこの型で Id を受け渡すので、BSP は生成 C と同じ定義でコンパイルする（fascon qemu / run が -D を揃える）
 * the Id type (see "32-bit Ids and generational arenas" below). C generated for --target generic-os defines FASCON2_ID64
 * and gets 64 bits [upper 32 = generation | lower 32 = index] (0ULL = None). The platform layer passes Ids in this type
 * too, so a BSP is compiled with the same definitions as the generated C (fascon qemu / run pass matching -D flags) */
#if defined(FASCON2_ID64)
typedef uint64_t f20_id_t; /* AArch64 の X レジスタ 1 本 / one AArch64 X register */
#else
typedef uint32_t f20_id_t; /* AArch64 の W レジスタ 1 本 / one AArch64 W register */
#endif

/* ───────────── バリア・MMIO（0.1 の fascon_rt.h と同じ定義） / barriers and MMIO (same as 0.1 fascon_rt.h) ───────────── */
#ifndef FASCON_RT_H
#define FASCON_COMPILER_BARRIER() __asm__ __volatile__("" ::: "memory")
#if defined(__x86_64__) || defined(__i386__)
#define FASCON_HW_FENCE() __asm__ __volatile__("mfence" ::: "memory")
#define FASCON_IO_WMB() __asm__ __volatile__("" ::: "memory")
#define FASCON_IO_RMB() __asm__ __volatile__("" ::: "memory")
#define FASCON_CPU_RELAX() __asm__ __volatile__("pause" ::: "memory")
#elif defined(__aarch64__)
#define FASCON_HW_FENCE() __asm__ __volatile__("dmb ish" ::: "memory")
#define FASCON_IO_WMB() __asm__ __volatile__("dmb oshst" ::: "memory")
#define FASCON_IO_RMB() __asm__ __volatile__("dmb oshld" ::: "memory")
#define FASCON_CPU_RELAX() __asm__ __volatile__("yield" ::: "memory")
#elif defined(__riscv)
#define FASCON_HW_FENCE() __asm__ __volatile__("fence rw, rw" ::: "memory")
#define FASCON_IO_WMB() __asm__ __volatile__("fence w, o" ::: "memory")
#define FASCON_IO_RMB() __asm__ __volatile__("fence i, r" ::: "memory")
#define FASCON_CPU_RELAX() __asm__ __volatile__("nop" ::: "memory")
#elif defined(__arm__)
#define FASCON_HW_FENCE() __asm__ __volatile__("dmb sy" ::: "memory")
#define FASCON_IO_WMB() __asm__ __volatile__("dmb sy" ::: "memory")
#define FASCON_IO_RMB() __asm__ __volatile__("dmb sy" ::: "memory")
#define FASCON_CPU_RELAX() __asm__ __volatile__("nop" ::: "memory")
#else
#define FASCON_HW_FENCE() __sync_synchronize()
#define FASCON_IO_WMB() __sync_synchronize()
#define FASCON_IO_RMB() __sync_synchronize()
#define FASCON_CPU_RELAX() __asm__ __volatile__("" ::: "memory")
#endif
#define FASCON_BYPASS_ENTER() FASCON_COMPILER_BARRIER()
#define FASCON_BYPASS_EXIT() FASCON_COMPILER_BARRIER()
#if defined(FASCON2_PREEMPT) && defined(FASCON2_HOSTED_MMIO)
/* PC 実行: cpu_relax() が仮想の時計を進め、期限の来たタイマー割り込みを届ける（PC では割り込まれうる点はここと
 * wait_irq() と閉じた区間の出口だけ）
 * PC runs: cpu_relax() advances the virtual clock and delivers a due timer interrupt (on a PC the only points that can be
 * interrupted are this, wait_irq() and the end of a closed section) */
void fascon2_host_poll(void);
static inline void fascon_cpu_relax(void) {
    FASCON_CPU_RELAX();
    fascon2_host_poll();
}
#else
static inline void fascon_cpu_relax(void) { FASCON_CPU_RELAX(); }
#endif
static inline void fascon_hw_fence(void) { FASCON_HW_FENCE(); }
static inline void fascon_compiler_fence(void) { FASCON_COMPILER_BARRIER(); }
#define FASCON_DEF_MMIO(bits)                                                   \
    static inline uint##bits##_t fascon_mmio_read##bits(uintptr_t a) {          \
        uint##bits##_t v = *(volatile uint##bits##_t *)a;                       \
        FASCON_IO_RMB();                                                        \
        return v;                                                               \
    }                                                                           \
    static inline void fascon_mmio_write##bits(uintptr_t a, uint##bits##_t v) { \
        FASCON_IO_WMB();                                                        \
        *(volatile uint##bits##_t *)a = v;                                      \
    }
FASCON_DEF_MMIO(8)
FASCON_DEF_MMIO(16)
FASCON_DEF_MMIO(32)
FASCON_DEF_MMIO(64)
#endif /* !FASCON_RT_H */

/* ───────────── I/O ポート（port_io） / I/O ports (port_io) ─────────────
 *   x86_64 : CPU の I/O ポート空間そのもの。`com1 = c;` は `out dx, al`、`let s = lsr;` は `in al, dx`（1 命令）。
 *            "Nd" 制約: 0〜255 の定数ポートは即値、それ以外は DX
 *   AArch64: I/O ポート空間が無いので、コンパイラが MMIO へ写した関数を生成する（COM1 → 下の f20_com1_*: PL011 で 16550 を真似る）
 *   PC 実行: FASCON2_HOSTED_MMIO のときはプラットフォーム層の fascon2_host_pio_in / out（COM1 の送信は標準出力）
 *   x86_64 : the CPU's own I/O port space: `com1 = c;` is `out dx, al` and `let s = lsr;` is `in al, dx` (one instruction).
 *            The "Nd" constraint uses an immediate for constant ports 0-255 and DX otherwise
 *   AArch64: there is no I/O port space, so the compiler emits functions remapped to MMIO (COM1 → f20_com1_* below: a 16550
 *            emulated on the PL011)
 *   PC run : under FASCON2_HOSTED_MMIO the platform layer's fascon2_host_pio_in / out (COM1 transmit goes to stdout) */
#if defined(__x86_64__) || defined(__i386__)
static inline FASCON_MAYBE_UNUSED uint8_t fascon_inb(uint16_t port) {
    uint8_t v;
    __asm__ __volatile__("inb %1, %0" : "=a"(v) : "Nd"(port) : "memory");
    return v;
}
static inline FASCON_MAYBE_UNUSED uint16_t fascon_inw(uint16_t port) {
    uint16_t v;
    __asm__ __volatile__("inw %1, %0" : "=a"(v) : "Nd"(port) : "memory");
    return v;
}
static inline FASCON_MAYBE_UNUSED uint32_t fascon_inl(uint16_t port) {
    uint32_t v;
    __asm__ __volatile__("inl %1, %0" : "=a"(v) : "Nd"(port) : "memory");
    return v;
}
static inline FASCON_MAYBE_UNUSED void fascon_outb(uint16_t port, uint8_t v) { __asm__ __volatile__("outb %0, %1" : : "a"(v), "Nd"(port) : "memory"); }
static inline FASCON_MAYBE_UNUSED void fascon_outw(uint16_t port, uint16_t v) { __asm__ __volatile__("outw %0, %1" : : "a"(v), "Nd"(port) : "memory"); }
static inline FASCON_MAYBE_UNUSED void fascon_outl(uint16_t port, uint32_t v) { __asm__ __volatile__("outl %0, %1" : : "a"(v), "Nd"(port) : "memory"); }
#endif
#if defined(__aarch64__)
/* COM1（16550）を QEMU virt の PL011 で真似る / COM1 (a 16550) emulated on the PL011 of QEMU virt
 *   +0 THR / RBR → DR（DLAB = 1 の間は除数の下位として覚えるだけ） / DR (only remembered as the divisor low byte while DLAB = 1)
 *   +2 IIR → 1（割り込みなし） / 1 (no interrupt pending)
 *   +5 LSR ← FR: bit0 受信あり = !RXFE、bit5 送信可 = !TXFF、bit6 送信完了 = TXFE && !BUSY
 *            bit0 data ready = !RXFE, bit5 THR empty = !TXFF, bit6 transmitter empty = TXFE && !BUSY
 *   +6 MSR → 0xB0（DCD・DSR・CTS） / 0xB0 (DCD, DSR, CTS)
 *   それ以外（IER・FCR・LCR・MCR・SCR）は値を覚えるだけ（PL011 は QEMU が設定済み）
 *   the rest (IER, FCR, LCR, MCR, SCR) is only remembered (QEMU has already set the PL011 up) */
#ifndef FASCON2_PL011_BASE
#define FASCON2_PL011_BASE 0x09000000u
#endif
static FASCON_MAYBE_UNUSED uint8_t f20__com1_reg[8];
static inline FASCON_MAYBE_UNUSED uint32_t f20_com1_in(uint32_t off) {
    const uint32_t dlab = (uint32_t)f20__com1_reg[3] & 0x80u;
    switch (off & 7u) {
    case 0u:
        if (dlab) return f20__com1_reg[0];
        return fascon_mmio_read32((uintptr_t)FASCON2_PL011_BASE) & 0xFFu;
    case 2u:
        return 0x01u;
    case 5u: {
        const uint32_t fr = fascon_mmio_read32((uintptr_t)FASCON2_PL011_BASE + 0x18u);
        return (uint32_t)((fr & 0x10u) == 0u) | ((uint32_t)((fr & 0x20u) == 0u) << 5) | ((uint32_t)((fr & 0x80u) != 0u && (fr & 0x08u) == 0u) << 6);
    }
    case 6u:
        return 0xB0u;
    default:
        return f20__com1_reg[off & 7u];
    }
}
static inline FASCON_MAYBE_UNUSED void f20_com1_out(uint32_t off, uint32_t v) {
    if ((off & 7u) == 0u && ((uint32_t)f20__com1_reg[3] & 0x80u) == 0u) {
        fascon_mmio_write32((uintptr_t)FASCON2_PL011_BASE, v & 0xFFu);
        return;
    }
    f20__com1_reg[off & 7u] = (uint8_t)v;
}
#endif
#if defined(FASCON2_HOSTED_MMIO)
/* PC 実行のプラットフォーム層（bsp/host）が実装する / implemented by the PC platform layer (bsp/host) */
uint32_t fascon2_host_pio_in(uint32_t port, uint32_t bits);
void fascon2_host_pio_out(uint32_t port, uint32_t bits, uint32_t value);
#endif

/* ───────────── パニック・出力 / panics and output ───────────── */

#define FASCON_PANIC_OOM 0x0001u        /* own ヒープの枯渇 / own heap exhausted */
#define FASCON_PANIC_TOO_LARGE 0x0004u  /* 1 つの所有リージョンが大きすぎる / one own region is too large */
#define FASCON_PANIC_USER 0x0006u       /* panic("...") */
#define FASCON_PANIC_ARITH 0x000Bu      /* 整数オーバーフロー・ゼロ除算 / integer overflow or division by zero */
#define FASCON_PANIC_BOUNDS 0x000Cu     /* 添字の範囲外 / index out of bounds */
#define FASCON_PANIC_STALE_ID 0x000Du   /* 古い（解放済み・再利用済み）Id で @a[id] / a stale Id used with @a[id] */
#define FASCON_PANIC_UNWRAP 0x000Eu     /* None に unwrap / unwrap on None */
#define FASCON_PANIC_OWN_CORRUPT 0x000Fu /* 所有リージョンの見出しが壊れている / corrupted own-region header */

FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg);
void fascon_plat_print(const char *s, size_t len);
#if defined(FASCON2_USER)
/* ユーザープログラム（EL0）のシステムコール: 番号を x8、引数を x0〜x7 に入れて svc。戻り値は x0。ほかのレジスタは
 * カーネルが保存フレームごと戻すので壊れない
 * a system call from a user program (EL0): the number in x8, arguments in x0 to x7, then svc; the result is in x0. The
 * kernel restores the whole saved frame, so no other register changes */
static inline FASCON_MAYBE_UNUSED int64_t fascon_user_svc(uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5, uint64_t a6, uint64_t a7) {
#if defined(__aarch64__)
    register uint64_t x0 __asm__("x0") = a0;
    register uint64_t x1 __asm__("x1") = a1;
    register uint64_t x2 __asm__("x2") = a2;
    register uint64_t x3 __asm__("x3") = a3;
    register uint64_t x4 __asm__("x4") = a4;
    register uint64_t x5 __asm__("x5") = a5;
    register uint64_t x6 __asm__("x6") = a6;
    register uint64_t x7 __asm__("x7") = a7;
    register uint64_t x8 __asm__("x8") = nr;
    __asm__ __volatile__("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x6), "r"(x7), "r"(x8) : "memory");
    return (int64_t)x0;
#elif defined(__x86_64__)
    /* Ring 3 → 0: 番号を rax、引数を rdi・rsi・rdx・r10・r8・r9・r12・r13 に入れて int $0x80（DPL 3 のゲート）。戻り値は rax
     * ring 3 → 0: the number in rax, arguments in rdi, rsi, rdx, r10, r8, r9, r12 and r13, then int $0x80 (a DPL 3 gate);
     * the result is in rax */
    register uint64_t r10 __asm__("r10") = a3;
    register uint64_t r8 __asm__("r8") = a4;
    register uint64_t r9 __asm__("r9") = a5;
    register uint64_t r12 __asm__("r12") = a6;
    register uint64_t r13 __asm__("r13") = a7;
    uint64_t rax = nr;
    __asm__ __volatile__("int $0x80" : "+a"(rax) : "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9), "r"(r12), "r"(r13) : "memory", "rcx", "r11");
    return (int64_t)rax;
#else
    (void)nr; (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5; (void)a6; (void)a7;
    return -38; /* ENOSYS: ユーザープログラムは AArch64 の EL0 でだけ動く / user programs run at AArch64 EL0 only */
#endif
}
#endif
FASCON_NORETURN void fascon_plat_exit(int code);
FASCON_NORETURN void fascon_plat_halt(void);
uint32_t fascon_plat_cpu_id(void);

/* ───── カーネル拡張: ワイヤ構造体・窓（&[u8]）・ビュー
 *       kernel extensions: wire structs, windows (&[u8]) and views
 *
 * 窓は (番地, 長さ)。Fascon では第二級の参照なので、構造体・戻り値・アリーナには置けない（F0515）。
 * A window is (address, length). In Fascon it is a second-class reference: never stored in structs, results or arenas. */
typedef struct {
    uint8_t *p;
    uint64_t n;
} f20_slice_t;
/* big-endian のフィールドの読み書き（対応するターゲットはすべて little-endian）
 * reading and writing big-endian fields (every supported target is little-endian) */
#define fascon_be16(x) __builtin_bswap16((uint16_t)(x))
#define fascon_be32(x) __builtin_bswap32((uint32_t)(x))
#define fascon_be64(x) __builtin_bswap64((uint64_t)(x))
/* ビュー: [off, off + size) が収まれば先頭の番地、収まらなければ NULL（止まらない）
 * a view: the start address when [off, off + size) fits, NULL otherwise (never stops) */
static inline FASCON_MAYBE_UNUSED void *f20_view(uint8_t *p, uint64_t n, uint64_t off, uint64_t size) {
    return (off <= n && size <= n - off) ? (void *)(p + off) : NULL;
}
/* 窓: [off, off + len) が収まれば out に入れて out を返す。収まらなければ NULL
 * a window: fills `out` and returns it when [off, off + len) fits, NULL otherwise */
static inline FASCON_MAYBE_UNUSED f20_slice_t *f20_window(f20_slice_t *out, uint8_t *p, uint64_t n, uint64_t off, uint64_t len) {
    if (!(off <= n && len <= n - off)) return NULL;
    out->p = p + off;
    out->n = len;
    return out;
}
static inline FASCON_MAYBE_UNUSED bool f20_slice_eq(f20_slice_t a, f20_slice_t b) {
    return a.n == b.n && (a.n == 0u || __builtin_memcmp(a.p, b.p, (size_t)a.n) == 0);
}
/* 短い方の長さまで写す（重なっていてもよい）。写したバイト数を返す
 * copies up to the shorter length (overlap allowed); returns the number of bytes copied */
static inline FASCON_MAYBE_UNUSED uint64_t f20_bytes_copy(uint8_t *d, uint64_t dn, const uint8_t *s, uint64_t sn) {
    const uint64_t n = dn < sn ? dn : sn;
    if (n != 0u) __builtin_memmove(d, s, (size_t)n);
    return n;
}

/* DMA バッファの貸し出し（to_device = 1）と回収（0）の前後でキャッシュを合わせる。一貫した DMA（QEMU・x86）では何もしない。
 * board.toml の [dma] coherent = false（FASCON2_DMA_NONCOHERENT）では、キャッシュライン（FASCON2_CACHE_LINE: 64 / 128 B）
 * ごとに、貸す前は PoC まで掃き出し（装置が最新を読む）、戻ったら掃き出し + 無効化（CPU が装置の書いた値を読む。端の
 * 半端なラインの汚れも失わない）
 * cache maintenance around lending a DMA buffer (to_device = 1) and reclaiming it (0); nothing for coherent DMA (QEMU,
 * x86). With [dma] coherent = false in board.toml (FASCON2_DMA_NONCOHERENT), per cache line (FASCON2_CACHE_LINE: 64 / 128 B):
 * clean to the PoC before lending (the device reads the latest data) and clean + invalidate on return (the CPU reads what
 * the device wrote; dirty partial lines at the edges are not lost) */
#if defined(FASCON2_DMA_NONCOHERENT)
#ifndef FASCON2_CACHE_LINE
#define FASCON2_CACHE_LINE 64u
#endif
static inline void fascon_plat_dma_sync(void *p, size_t n, int to_device) {
    uintptr_t a = (uintptr_t)p & ~(uintptr_t)(FASCON2_CACHE_LINE - 1u);
    const uintptr_t e = (uintptr_t)p + n;
#if defined(__aarch64__)
    __asm__ __volatile__("dsb sy" ::: "memory");
    for (; a < e; a += FASCON2_CACHE_LINE) {
        if (to_device) {
            __asm__ __volatile__("dc cvac, %0" : : "r"(a) : "memory");
        } else {
            __asm__ __volatile__("dc civac, %0" : : "r"(a) : "memory");
        }
    }
    __asm__ __volatile__("dsb sy" ::: "memory");
#elif defined(__riscv) && defined(FASCON2_RISCV_ZICBOM)
    for (; a < e; a += FASCON2_CACHE_LINE) {
        if (to_device) {
            __asm__ __volatile__("cbo.clean (%0)" : : "r"(a) : "memory");
        } else {
            __asm__ __volatile__("cbo.flush (%0)" : : "r"(a) : "memory");
        }
    }
    __asm__ __volatile__("fence rw, rw" ::: "memory");
#else
    (void)a;
    (void)e;
    (void)to_device;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
#endif
}
#else
static inline void fascon_plat_dma_sync(void *p, size_t n, int to_device) {
    (void)p;
    (void)n;
    (void)to_device;
    FASCON_COMPILER_BARRIER();
}
#endif

/* ───── gpu_arena: アクセラレータへの貸し出し（FASCON2_GPU のときだけ） / gpu_arena: offloading to accelerators (only
 *       under FASCON2_GPU)
 *
 *   生成 C が呼ぶのはプラットフォーム層の次の 4 つだけ（CUDA・Vulkan・スレッドの API は BSP の中に閉じ込める）
 *   the generated C calls only these four platform hooks (CUDA, Vulkan and thread APIs stay inside the BSP):
 *     fascon_plat_gpu_register(base, bytes, arena_id)   スロットの配列を装置と共有する（ピン留め）。0 = 成功
 *                                                        share the slot array with the device (pin it); 0 = success
 *     fascon_plat_gpu_dispatch(arena_id, slot_idx, kernel_id, grid, block, &fence)
 *                                                        スロット 1 つにカーネルを走らせる仕事を出す。0 = 成功
 *                                                        queue a kernel run on one slot; 0 = success
 *     fascon_plat_gpu_poll(fence)                        終わっていれば 0 以外（待たない） / nonzero when done (no waiting)
 *     fascon_plat_gpu_wait(fence)                        終わるまで待つ / wait until done
 *   スロット i の要素の番地は fascon2_gpu_arenas[arena_id].base + i × stride（生成 C が出す記述子）。PC の実行層
 *   （bsp/host）はワーカースレッドで、FASCON_GPU_KERNEL(番号) で登録した C の関数を走らせて真似る。
 *   --gpu=off（FASCON2_GPU_OFF）と、GPU の層を持たないベアメタル（FASCON2_GPU_SYNC）では、dispatch は呼んだコアでその場で
 *   登録された関数を走らせ（無ければ何もしない）、すぐ完了する。どちらも上の 4 つは呼ばない
 *   the element of slot i is at fascon2_gpu_arenas[arena_id].base + i × stride (descriptors emitted by the generated C).
 *   The PC layer (bsp/host) emulates the device with a worker thread running the C functions registered with
 *   FASCON_GPU_KERNEL(id). With --gpu=off (FASCON2_GPU_OFF) and on bare metal without a GPU layer (FASCON2_GPU_SYNC),
 *   dispatch runs the registered function in place on the calling core (nothing when none is registered) and completes
 *   at once; neither calls the four hooks */
#if defined(FASCON2_GPU)
#define FASCON_PANIC_GPU 0x0014u /* 装置へ出せない・古いハンドル / cannot offload, or a stale handle */
#ifndef FASCON2_GPU_KERNELS_MAX
#define FASCON2_GPU_KERNELS_MAX 64u
#endif
#if !defined(FASCON2_GPU_OFF) && !defined(FASCON2_HOSTED_MMIO) && !defined(FASCON2_BOARD_GPU)
#define FASCON2_GPU_SYNC 1 /* ベアメタルで、ボードが GPU の層を持たない / bare metal, and the board has no GPU layer */
#endif
/* カーネル: スロットの要素・添字・grid・block を受け取る C の関数 / a kernel: a C function taking the slot's element,
 * its index, grid and block */
typedef void (*fascon_gpu_kernel_fn)(void *elem, uint32_t slot, uint64_t grid, uint64_t block);
typedef struct {
    const char *name;
    void *base;       /* slot[0] の番地 / the address of slot[0] */
    size_t bytes;     /* スロットの配列の大きさ / the size of the slot array */
    size_t stride;    /* スロット 1 つの大きさ / the size of one slot */
    size_t elem_size; /* 要素の大きさ（スロットの先頭にある） / the element size (at the start of the slot) */
} FasconGpuArena;
extern const FasconGpuArena fascon2_gpu_arenas[];
extern const uint32_t fascon2_gpu_arena_count;
void fascon_gpu_kernel_register(uint32_t kernel_id, fascon_gpu_kernel_fn fn);
fascon_gpu_kernel_fn fascon_gpu_kernel_lookup(uint32_t kernel_id);
/* カーネルの定義と登録（main の前に登録される）。`<名前>.gpu.c` に書くと `fascon run` が一緒にビルドする
 * defining and registering a kernel (registered before main); written in `<name>.gpu.c`, `fascon run` builds it along
 *   FASCON_GPU_KERNEL(1) { Particle *p = elem; p->x += p->vx; } */
#define FASCON_GPU_KERNEL(id)                                                                                          \
    static void fsc_gpu_kernel_##id(void *elem, uint32_t slot, uint64_t grid, uint64_t block);                         \
    __attribute__((constructor)) static void fsc_gpu_kernel_reg_##id(void) { fascon_gpu_kernel_register((uint32_t)(id), fsc_gpu_kernel_##id); } \
    static void fsc_gpu_kernel_##id(void *elem, uint32_t slot, uint64_t grid, uint64_t block)
#if !defined(FASCON2_GPU_OFF) && !defined(FASCON2_GPU_SYNC)
int fascon_plat_gpu_register(void *base, size_t bytes, uint32_t arena_id);
int fascon_plat_gpu_dispatch(uint32_t arena_id, uint32_t slot_idx, uint32_t kernel_id, uint64_t grid, uint64_t block, uint64_t *out_fence);
int fascon_plat_gpu_poll(uint64_t fence);
void fascon_plat_gpu_wait(uint64_t fence);
#endif
static inline FASCON_MAYBE_UNUSED void f20_gpu_register(void *base, size_t bytes, uint32_t arena_id) {
#if defined(FASCON2_GPU_OFF) || defined(FASCON2_GPU_SYNC)
    (void)base;
    (void)bytes;
    (void)arena_id;
#else
    if (fascon_plat_gpu_register(base, bytes, arena_id) != 0) fascon_plat_kernel_panic(FASCON_PANIC_GPU, "gpu_arena: the platform cannot share the slot array with the device");
#endif
}
static inline FASCON_MAYBE_UNUSED uint64_t f20_gpu_dispatch(uint32_t arena_id, uint32_t slot, uint32_t kernel, uint64_t grid, uint64_t block, void *elem) {
#if defined(FASCON2_GPU_OFF) || defined(FASCON2_GPU_SYNC)
    const fascon_gpu_kernel_fn fn = fascon_gpu_kernel_lookup(kernel);
    (void)arena_id;
    if (fn != NULL) fn(elem, slot, grid, block);
    return 0u;
#else
    uint64_t f = 0u;
    (void)elem;
    if (fascon_plat_gpu_dispatch(arena_id, slot, kernel, grid, block, &f) != 0) fascon_plat_kernel_panic(FASCON_PANIC_GPU, "gpu_arena: dispatch failed (no device, or no kernel with this id)");
    return f;
#endif
}
static inline FASCON_MAYBE_UNUSED int f20_gpu_poll(uint64_t fence) {
#if defined(FASCON2_GPU_OFF) || defined(FASCON2_GPU_SYNC)
    (void)fence;
    return 1;
#else
    return fascon_plat_gpu_poll(fence);
#endif
}
static inline FASCON_MAYBE_UNUSED void f20_gpu_wait(uint64_t fence) {
#if defined(FASCON2_GPU_OFF) || defined(FASCON2_GPU_SYNC)
    (void)fence;
#else
    fascon_plat_gpu_wait(fence);
#endif
}
#endif

/* ───── --target hpc: 計算ノードの形・ピン留め・NUMA の固定（FASCON2_HPC のときだけ） / --target hpc: the compute node's
 *       shape, pinning and NUMA binding (only under FASCON2_HPC)
 *
 *   fascon_plat_topology(&t)          論理コア・物理コア・SMT・NUMA ノードの数 / logical cores, physical cores, SMT, NUMA nodes
 *   fascon_plat_pin_core(core)        呼んだスレッド（コア core）を 1 つの論理 CPU に固定する。できるだけ別々の物理コアへ
 *                                     （SMT の兄弟への二重割り当てを避ける） / pin the calling thread (core `core`) to one
 *                                     logical CPU, on distinct physical cores where possible (no SMT sibling sharing)
 *   fascon_plat_numa_node(core)       そのコアの NUMA ノード / the NUMA node of that core
 *   fascon_plat_numa_bind(p, n, node) [p, p + n) のページをノード node の物理メモリへ固定する（MPOL_BIND。触った
 *                                     ページは移す）。0 = 成功 / bind the pages of [p, p + n) to the physical memory of
 *                                     node `node` (MPOL_BIND; touched pages move); 0 = success
 *   生成 C は各コアの入口（main と spawn_on の入口）で 1 回、ピン留めとコアごとのメモリ（コアごとのアリーナ・
 *   fascon__cpu[core]・既定の own ヒープ）の固定をする。ベアメタル（FASCON2_HOSTED_MMIO なし）でボードが層を持たない
 *   ときは何もしない版が入る
 *   the generated C pins and binds the per-core memory (per-core arenas, fascon__cpu[core], the default own heap) once
 *   at each core's entry (main and spawn_on entries). On bare metal (no FASCON2_HOSTED_MMIO) without a board layer the
 *   do-nothing versions are used */
#if defined(FASCON2_HPC)
typedef struct {
    uint32_t cpus;       /* 論理コア / logical cores */
    uint32_t cores;      /* 物理コア / physical cores */
    uint32_t smt;        /* 物理コアあたりのスレッド / threads per physical core */
    uint32_t numa_nodes; /* NUMA ノード / NUMA nodes */
} FasconTopology;
#if defined(FASCON2_HOSTED_MMIO) || defined(FASCON2_BOARD_HPC)
void fascon_plat_topology(FasconTopology *t);
void fascon_plat_pin_core(uint32_t core);
uint32_t fascon_plat_numa_node(uint32_t core);
int fascon_plat_numa_bind(void *base, size_t bytes, uint32_t node);
#else
static inline FASCON_MAYBE_UNUSED void fascon_plat_topology(FasconTopology *t) {
    t->cpus = 1u;
    t->cores = 1u;
    t->smt = 1u;
    t->numa_nodes = 1u;
}
static inline FASCON_MAYBE_UNUSED void fascon_plat_pin_core(uint32_t core) { (void)core; }
static inline FASCON_MAYBE_UNUSED uint32_t fascon_plat_numa_node(uint32_t core) {
    (void)core;
    return 0u;
}
static inline FASCON_MAYBE_UNUSED int fascon_plat_numa_bind(void *base, size_t bytes, uint32_t node) {
    (void)base;
    (void)bytes;
    (void)node;
    return 0;
}
#endif
#endif

/* ───── カーネル拡張: 割り込み・閉じた区間・タスク（プラットフォーム層 = BSP か PC の実行層が実装する）
 *       kernel extensions: interrupts, closed sections and tasks (implemented by the platform layer: the BSP or the PC layer)
 *
 *   irq_save / irq_restore : 割り込みを禁止して前の状態を返す / 状態を戻す（入れ子にできる）
 *                            mask interrupts and return the previous state / put the state back (may nest)
 *   task_create            : スロット slot のタスク（Id は id）を用意する。stack の下 1/4 はそのタスクのフレームリージョン。
 *                            入口の関数から戻ると on_exit を呼ぶ
 *                            prepares the task of slot `slot` (its Id is `id`); the lowest quarter of `stack` is the task's
 *                            frame region; on_exit is called when the entry function returns
 *   task_yield             : いますぐ切り替える（id が古ければ何もしない） / switch now (nothing happens for a stale id)
 *   task_resume            : 割り込みの出口での切り替えを予約する / request a switch on interrupt exit
 *   task_enter             : 割り込みを許可して最初のタスクへ（戻らない） / enable interrupts and enter the first task
 *   task_exit              : いまのタスクを終えて、ほかの生きているタスクへ（無ければ終了コード 0 で終わる）
 *                            end the current task and switch to another live one (exit code 0 when none is left)
 * 保存フレームは 1 種類（f20_trapframe_t、800 バイト）で、割り込み・yield・Lower EL の例外に共通。
 * There is one saved frame (f20_trapframe_t, 800 bytes) shared by interrupts, yields and lower-EL exceptions. */
typedef struct {
    uint32_t intid;
    void (*handler)(void);
    const char *name;
} FasconIrqEntry;
#define FASCON2_TASK_MAX 256u                   /* task_arena の容量の上限 / the capacity limit of a task_arena */
#define FASCON_PANIC_STACK 0x0010u              /* タスクのスタックの番兵語が壊れた / a task's stack canary was overwritten */
#define FASCON_PANIC_IRQ 0x0011u                /* 登録の無い割り込み・割り込みの嵐 / unexpected interrupt or interrupt storm */
#define FASCON_PANIC_TASK 0x0012u               /* タスク表の誤用 / misuse of the task table */
#define FASCON_STACK_CANARY UINT64_C(0xFA5C0DE5CA9A8B7C)
/* 保存フレーム（AArch64）。start.S の SAVE_FRAME / RESTORE_FRAME と同じ並び
 * the saved frame (AArch64), laid out exactly as SAVE_FRAME / RESTORE_FRAME in start.S */
typedef struct {
    uint64_t x[31];
    uint64_t sp_el0, elr, spsr, fpsr, fpcr;
    uint8_t q[512];
} f20_trapframe_t;
_Static_assert(sizeof(f20_trapframe_t) == 800, "f20_trapframe_t must be 800 bytes (start.S)");
_Static_assert(offsetof(f20_trapframe_t, sp_el0) == 248 && offsetof(f20_trapframe_t, elr) == 256 && offsetof(f20_trapframe_t, q) == 288, "f20_trapframe_t layout (start.S)");
void fascon_plat_irq_register(const FasconIrqEntry *table, uint32_t count);
uint64_t fascon_plat_irq_save(void);
void fascon_plat_irq_restore(uint64_t state);
void fascon_plat_irq_open(void);
void fascon_plat_wait_irq(void);
uint64_t fascon_plat_timer_freq(void);
uint64_t fascon_plat_timer_now(void);
void fascon_plat_timer_arm(uint64_t ticks);
bool fascon_plat_task_create(uint32_t slot, f20_id_t id, void (*entry)(uint64_t), uint64_t arg, void *stack, size_t stack_size, void (*on_exit)(void));
f20_id_t fascon_plat_task_current(void);
void fascon_plat_task_yield(uint32_t slot, f20_id_t id);
void fascon_plat_task_resume(uint32_t slot, f20_id_t id);
FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id);
FASCON_NORETURN void fascon_plat_task_exit(void);
/* ───── ユーザー空間（EL0）: user_mem / syscall / trap ─────
 *   user_register    : ユーザーメモリ（スロットごとに size バイト、2 MiB 境界）と、EL0 から見える仮想番地を登録する
 *                      registers the user memory (size bytes per slot, 2 MiB aligned) and its virtual address at EL0
 *   task_create_user : EL0 のタスクを用意する。そのスロットのユーザーメモリを 0 にし、全レジスタ 0 で entry へ入る文脈を作る
 *                      prepares an EL0 task: zeroes the user memory of its slot and builds a context entering entry with
 *                      every register zero
 *   user_window      : いまのタスクのユーザーの (番地, 長さ) を検証し、カーネルの番地で見た窓にする（外れたら false）
 *                      validates a user (address, length) of the current task and turns it into a window at its kernel
 *                      address (false when out of range)
 *   syscall_register : 番号 → 入口の表（count 個）。入口は保存フレームの x0〜x7 を受け取る
 *                      the table from numbers to entries; an entry receives x0 to x7 of the saved frame
 *   trap_register    : EL0 の例外（svc 以外）のハンドラ。戻ると、そのタスクは終了する
 *                      the handler of EL0 exceptions other than svc; the task is ended when it returns */
typedef int64_t (*FasconSyscallFn)(const uint64_t *regs);
void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots);
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void));
bool fascon_plat_user_window(uint64_t addr, uint64_t len, f20_slice_t *out);
void fascon_plat_syscall_register(const FasconSyscallFn *table, uint32_t count);
void fascon_plat_trap_register(void (*handler)(uint64_t esr, uint64_t far, uint64_t pc));
/* 検証済みの 2 つの区間が重なるか（長さ 0 は重ならない） / whether two validated ranges overlap (empty ranges never do) */
static inline FASCON_MAYBE_UNUSED bool f20_user_overlap(uint64_t a, uint64_t an, uint64_t b, uint64_t bn) {
    return an != 0u && bn != 0u && a < b + bn && b < a + an;
}
static inline void fascon__irq_restore_p(const uint64_t *state) { fascon_plat_irq_restore(*state); }

/* ───── マルチコアと割り込み・タスク・ユーザー空間の統合: 抽象プラットフォーム API（すべての BSP が同じ名前で実装する）─────
 *       the integration of multi-core with interrupts, tasks and user space: the abstract platform API (every BSP
 *       implements the same names; the compiler only emits calls to these, never architecture-specific code)
 *
 *   critical_enter / exit : 閉じた区間（マルチコア）。① ローカルの割り込みを禁止（DAIF.I / EFLAGS.IF を退避）→ ② スピン
 *                           ロックを取る。出るときは逆順（ロックを放す → 割り込みの状態を戻す）。割り込みハンドラが同じ
 *                           コアで同じロックを取りに来て行き詰まることが、構造上起きない。既定の実装は実装節（弱い定義）
 *                           a closed section on multi-core: (1) mask local interrupts (save DAIF.I / EFLAGS.IF), then
 *                           (2) take the spinlock; leave in reverse order (release the lock, then restore the interrupt
 *                           state). An interrupt handler can never deadlock on the lock its own core holds. The default
 *                           is in the implementation section (weak definitions)
 *   vm_*                  : 4 KiB のページとタスクごとのアドレス空間（AArch64 = ASID 付きの TTBR0、x86_64 = PCID 付きの CR3）。
 *                           切り替え（vm_switch）では TLB を掃かない。掃くのはページを外す・共有をやめるときだけで、その
 *                           ASID を使っているコアへだけ（自分なら局所の TLBI、ほかのコアは IPI）
 *                           4 KiB pages and per-task address spaces (AArch64: TTBR0 with an ASID, x86_64: CR3 with a PCID).
 *                           vm_switch never flushes the TLB; flushes happen only when a page is unmapped or stops being
 *                           shared, and only on the cores using that ASID (a local TLBI on this core, IPIs to others)
 *   copy_from / to_user   : ユーザー空間との 1 回だけの写し（AArch64: LDTRB / STTRB + 例外表、x86_64: SMAP の STAC/CLAC +
 *                           例外表）。写せなかったバイト数を返す（0 = 成功）
 *                           one copy across the user boundary (AArch64: LDTRB / STTRB + exception table, x86_64:
 *                           SMAP STAC/CLAC + exception table); returns the bytes NOT copied (0 = success)
 *   panic_broadcast       : パニックしたコアが、ほかの全コアの割り込みと実行を止める（AArch64: GIC の SGI、x86_64: NMI の
 *                           IPI）。DMA や MMIO を壊し続けるコアを残さない
 *                           the panicking core stops interrupts and execution on every other core (AArch64: a GIC SGI,
 *                           x86_64: an NMI IPI), so no core keeps scribbling over DMA buffers or MMIO */
/* ビットロック（スピン。割り込みは呼び出し側が止める） / a bit lock (spins; the caller masks interrupts) */
static inline FASCON_MAYBE_UNUSED void f20_bitlock(volatile uint32_t *l) {
    while (__atomic_exchange_n(l, 1u, __ATOMIC_ACQUIRE) != 0u) {
        while (__atomic_load_n(l, __ATOMIC_RELAXED) != 0u) fascon_cpu_relax();
    }
}
static inline FASCON_MAYBE_UNUSED void f20_bitunlock(volatile uint32_t *l) { __atomic_store_n(l, 0u, __ATOMIC_RELEASE); }
uint64_t fascon_plat_critical_enter(void);
void fascon_plat_critical_exit(uint64_t state);
void fascon_plat_panic_broadcast(void);
/* ほかのコアのパニックに気づいたコアが止まる場所（待ちループ・出力の入口から）。全コア停止を持つ BSP は「止まったコア」と
 * して数える（その後で SGI / NMI が来ても 2 度は数えない）。既定は回り続けるだけ
 * where a core that noticed the panic of another core stops (from waiting loops and the console). BSPs with the
 * all-core stop count it as a stopped core (and do not count it twice when the SGI / NMI arrives later); the default
 * just spins */
FASCON_NORETURN void fascon_plat_core_parked(void);
uint64_t fascon_plat_copy_from_user(void *dst, uint64_t uaddr, uint64_t n);
uint64_t fascon_plat_copy_to_user(uint64_t uaddr, const void *src, uint64_t n);
#define F20_VM_READ 0x1u  /* 読める / readable */
#define F20_VM_WRITE 0x2u /* 書ける / writable */
#define F20_VM_EXEC 0x4u  /* 実行できる / executable */
#define F20_VM_USER 0x8u  /* EL0 / Ring 3 から触れる / accessible from EL0 / ring 3 */
#define F20_VM_PAGE 4096u
/* アドレス空間を（空にして）用意する / prepare (an empty) address space */
bool fascon_plat_vm_space(uint32_t asid);
/* 4 KiB のページを 1 枚写す（va・pa は 4 KiB 境界） / map one 4 KiB page (va and pa 4 KiB aligned) */
bool fascon_plat_vm_map(uint32_t asid, uint64_t va, uint64_t pa, uint32_t flags);
/* ページを外し、その ASID を使うコアの TLB からも消す。外した物理番地（無ければ 0）を返す / unmap a page and remove it from
 * the TLBs of the cores using the ASID; returns the physical address that was mapped (0 when none) */
uint64_t fascon_plat_vm_unmap(uint32_t asid, uint64_t va);
/* このコアのアドレス空間を切り替える（TLB は掃かない） / switch the address space of this core (no TLB flush) */
void fascon_plat_vm_switch(uint32_t asid);
/* cpu_mask のコアの TLB から (asid, va) を消す（va = UINT64_MAX ならその ASID 全部） / remove (asid, va) from the TLBs of
 * the cores in cpu_mask (va = UINT64_MAX: the whole ASID) */
void fascon_plat_vm_flush(uint32_t asid, uint64_t va, uint64_t cpu_mask);
/* アドレス空間を捨てる（ページの写しを全部外し、その ASID の TLB を掃く） / drop an address space (unmap everything and
 * flush that ASID) */
void fascon_plat_vm_drop(uint32_t asid);
/* マルチコアのシステムコールの影の写しの上限（own ヒープの最大のブロック。超える窓は -7 = E2BIG）
 * the limit of a shadow copy in multi-core system calls (the largest own-heap block; longer windows give -7 = E2BIG) */
#ifndef FASCON2_SYSCALL_SHADOW_MAX
#define FASCON2_SYSCALL_SHADOW_MAX 0xFFF0u
#endif

/* ───── コアごとのスケジューラの状態（f20_cpu_ctx_t の中。1 コアのプログラムでは 1 つ）─────
 *       per-core scheduler state (inside f20_cpu_ctx_t; one instance in single-core programs)
 *   task_arena の「いまのタスク」と実行キュー（切り替えの予約・自発的な切り替えの行き先）をコアごとに持つ。BSP は
 *   fascon_cpu_sched(コア) で自分のコアの記録を引く（タスクはコアの間を移らない: task_arena はコアごとの実体）
 *   the "current task" of task_arena and the run queue (the pending switch, the target of a voluntary switch) are kept
 *   per core; a BSP looks up the record of its own core with fascon_cpu_sched(core) (tasks never migrate between cores:
 *   task_arena has one instance per core) */
typedef struct {
    int32_t cur;       /* いまのタスクのスロット（-1 = タスクの外） / slot of the current task (-1 = outside tasks) */
    int32_t pending;   /* 割り込みの出口での切り替え先（resume） / switch target at interrupt exit (resume) */
    int32_t svc_to;    /* 自発的な切り替え先（yield_to / enter / exit） / target of a voluntary switch */
    int32_t svc_save;  /* 切り替えで元の文脈を保存するか / whether the old context is saved */
    uint32_t in_irq;   /* 割り込みハンドラの中 / inside an interrupt handler */
    uint32_t gic_on;   /* このコアの割り込みコントローラの窓口を開いた / the interrupt-controller interface of this core is on */
    uint32_t live;     /* このコアの生きているタスクの数 / live tasks on this core */
    uint32_t asid_bits;
    uint32_t ready;    /* 初期化済み（cur などを -1 にした） / initialised (cur and friends set to -1) */
    uint64_t switches; /* 切り替えの回数 / number of switches */
    uint64_t ticks;    /* タイマー割り込みの回数 / timer interrupts taken */
    uint64_t faults;   /* 要求時ページングで写したページ / pages mapped on demand */
    void *idle;        /* タスクが無いときの文脈 / the context used when no task is left */
} f20_sched_t;
f20_sched_t *fascon_cpu_sched(uint32_t core);

/* 閉じた区間: ブロックのどの出口（末尾・break・continue・return）でも割り込みの状態を戻す（cleanup 属性）
 * a closed section: the interrupt state is restored at every exit of the block (end, break, continue, return) */
#if defined(FASCON2_SMP)
/* マルチコア: 閉じた区間は「割り込み禁止 → ロック」（fascon_plat_critical_enter）。並列走査（par()）は読み手として入る
 * multi-core: a closed section is "interrupts masked, then the lock" (fascon_plat_critical_enter); parallel scans
 * (par()) enter as readers */
uint64_t fascon_smp_write_enter(void);
void fascon_smp_write_exit(uint64_t state);
uint64_t fascon_smp_read_enter(void);
void fascon_smp_read_exit(uint64_t state);
static inline void fascon__smp_write_exit_p(const uint64_t *state) { fascon_plat_critical_exit(*state); }
static inline void fascon__smp_read_exit_p(const uint64_t *state) { fascon_smp_read_exit(*state); }
#define F20_CRITICAL(v) const uint64_t v __attribute__((cleanup(fascon__smp_write_exit_p), unused)) = fascon_plat_critical_enter()
#define F20_READ_SECTION(v) const uint64_t v __attribute__((cleanup(fascon__smp_read_exit_p), unused)) = fascon_smp_read_enter()
#else
#define F20_CRITICAL(v) const uint64_t v __attribute__((cleanup(fascon__irq_restore_p), unused)) = fascon_plat_irq_save()
#define F20_READ_SECTION(v) const uint64_t v __attribute__((cleanup(fascon__irq_restore_p), unused)) = fascon_plat_irq_save()
#endif

/* ───────────── マルチコア（SMP） / multi-core ─────────────
 *   spawn_on    : コア core（1 〜 cpu_count() - 1）で fn(arg) を走らせる。そのコアが無い・まだ前の関数を走らせているなら
 *                 パニック（0x0012）
 *                 runs fn(arg) on core `core` (1 to cpu_count() - 1); a panic (0x0012) when there is no such core or it
 *                 is still running an earlier function
 *   cpu_count   : 動かせるコアの数（1 コアのプラットフォームでは 1） / the number of usable cores (1 on single-core platforms)
 * ふつうのアリーナ・フレームリージョン・own ヒープはコアごとの実体なので、1 コアのときと同じコード（ロックなし・
 * アトミック命令なし）でアクセスする。コアの間を渡るのは chan（ロックフリー）と shared arena（critical の中だけ）。
 * Plain arenas, the frame region and the own heap have one instance per core, so they are accessed by the single-core
 * code (no locks, no atomic instructions). Only chan (lock-free) and shared arenas (inside critical) cross cores. */
/* コアの数の上限（--target hpc では生成 C が計算ノードの論理コアの数を先に定義する） / the core limit (under --target hpc
 * the generated C defines the compute node's logical core count first) */
#ifndef FASCON2_MAX_CPUS
#define FASCON2_MAX_CPUS 4u
#endif
#define FASCON_PANIC_SMP 0x0012u /* spawn_on: コアが無い・使用中 / spawn_on: no such core, or it is busy */
void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg);
uint32_t fascon_plat_cpu_count(void);
#if defined(FASCON2_SMP)
#define F20_PERCPU __thread
#else
#define F20_PERCPU
#endif
#define F20_PERCPU_STORAGE __attribute__((aligned(F20_DESTRUCTIVE_SIZE)))
/* ほかのコアが止まった（パニック）なら、このコアも止まる。待ちループの中から呼ぶ
 * stop this core too when another core has panicked; called from waiting loops */
void fascon_smp_relax(void);
/* コアの起動時に 1 回: このコアの番号と状態を登録する / once when a core starts: registers its number and state */
void fascon_smp_core_init(uint32_t core);

/* チャネル: 容量が 2 の累乗の、ロックフリーなリングバッファ（送り手・受け手は何本でもよい）。
 *   セルごとに番号 seq を持つ。送り手は「seq == 末尾」のセルを CAS で取り、値を書いてから seq を release で進める。
 *   受け手は「seq == 先頭 + 1」のセルを CAS で取り、値を読んでから seq を 1 周ぶん進める。値の読み書きは、番号の
 *   release → acquire で順序が付く。番号は「セルの添字との差」で持つので、.bss の 0 のままで空のチャネルになる。
 *   先頭・末尾・セルの表は別々の 128 バイト境界に置く（送り手と受け手が同じキャッシュラインを取り合わない）。
 * A channel: a lock-free ring buffer with a power-of-two capacity (any number of senders and receivers).
 *   Every cell carries a sequence number. A sender claims the cell whose seq equals the tail with a CAS, writes the
 *   value, then advances seq with release order; a receiver claims the cell whose seq equals head + 1, reads the value,
 *   then advances seq by one lap. The value accesses are ordered by the release → acquire of the number. Numbers are
 *   stored as differences from the cell index, so an all-zero .bss is an empty channel. Head, tail and the cells live
 *   on separate 128-byte boundaries (senders and receivers never fight over one cache line). */
#define F20_DEFINE_CHAN(name, T, CAP)                                                                                   \
    static struct F20_ALIGN128 {                                                                                        \
        F20_ALIGN128 uint64_t tail;                                                                                     \
        uint8_t pad0_[F20_DESTRUCTIVE_SIZE - sizeof(uint64_t)];                                                         \
        F20_ALIGN128 uint64_t head;                                                                                     \
        uint8_t pad1_[F20_DESTRUCTIVE_SIZE - sizeof(uint64_t)];                                                         \
        F20_ALIGN128 struct {                                                                                           \
            uint64_t seq;                                                                                               \
            T v;                                                                                                        \
        } cell[CAP];                                                                                                    \
    } chan_##name;                                                                                                      \
    _Static_assert(((CAP) & ((CAP) - 1u)) == 0u, "chan " #name ": the capacity must be a power of two");                \
    static inline FASCON_MAYBE_UNUSED bool chan_##name##_try_send(T v) {                                                \
        uint64_t pos = __atomic_load_n(&chan_##name.tail, __ATOMIC_RELAXED);                                            \
        for (;;) {                                                                                                      \
            const uint64_t i = pos & ((CAP) - 1u);                                                                      \
            const uint64_t seq = __atomic_load_n(&chan_##name.cell[i].seq, __ATOMIC_ACQUIRE) + i;                       \
            const int64_t dif = (int64_t)(seq - pos);                                                                   \
            if (dif == 0) {                                                                                             \
                if (__atomic_compare_exchange_n(&chan_##name.tail, &pos, pos + 1u, true, __ATOMIC_RELAXED, __ATOMIC_RELAXED)) { \
                    chan_##name.cell[i].v = v;                                                                          \
                    __atomic_store_n(&chan_##name.cell[i].seq, pos + 1u - i, __ATOMIC_RELEASE);                         \
                    return true;                                                                                        \
                }                                                                                                       \
            } else if (dif < 0) {                                                                                       \
                return false; /* 満杯 / full */                                                                         \
            } else {                                                                                                    \
                pos = __atomic_load_n(&chan_##name.tail, __ATOMIC_RELAXED);                                             \
            }                                                                                                           \
        }                                                                                                               \
    }                                                                                                                   \
    static inline FASCON_MAYBE_UNUSED bool chan_##name##_try_recv(T *out) {                                             \
        uint64_t pos = __atomic_load_n(&chan_##name.head, __ATOMIC_RELAXED);                                            \
        for (;;) {                                                                                                      \
            const uint64_t i = pos & ((CAP) - 1u);                                                                      \
            const uint64_t seq = __atomic_load_n(&chan_##name.cell[i].seq, __ATOMIC_ACQUIRE) + i;                       \
            const int64_t dif = (int64_t)(seq - (pos + 1u));                                                            \
            if (dif == 0) {                                                                                             \
                if (__atomic_compare_exchange_n(&chan_##name.head, &pos, pos + 1u, true, __ATOMIC_RELAXED, __ATOMIC_RELAXED)) { \
                    *out = chan_##name.cell[i].v;                                                                       \
                    __atomic_store_n(&chan_##name.cell[i].seq, pos + (CAP) - i, __ATOMIC_RELEASE);                      \
                    return true;                                                                                        \
                }                                                                                                       \
            } else if (dif < 0) {                                                                                       \
                return false; /* 空 / empty */                                                                          \
            } else {                                                                                                    \
                pos = __atomic_load_n(&chan_##name.head, __ATOMIC_RELAXED);                                             \
            }                                                                                                           \
        }                                                                                                               \
    }                                                                                                                   \
    static inline FASCON_MAYBE_UNUSED void chan_##name##_send(T v) {                                                    \
        while (!chan_##name##_try_send(v)) fascon_smp_relax();                                                          \
    }                                                                                                                   \
    static inline FASCON_MAYBE_UNUSED T chan_##name##_recv_wait(void) {                                                 \
        T v;                                                                                                            \
        while (!chan_##name##_try_recv(&v)) fascon_smp_relax();                                                         \
        return v;                                                                                                       \
    }                                                                                                                   \
    struct chan_##name##_semi_

FASCON_NORETURN void fascon_panic(uint32_t code, const char *msg, const char *site);
void fascon_print_i64(int64_t v);
void fascon_print_u64(uint64_t v);
void fascon_put_i64(int64_t v);
void fascon_put_u64(uint64_t v);
/* 浮動小数点（f32 / f64）の出力: 往復できる最短の 10 進（Rust の {:?} と同じ形: 1.0・0.1・1e16・1.5e-7・NaN・inf・-0.0）。
 * 実装は FASCON2_FLOAT のときだけ / float output: the shortest decimal that reads back exactly (the shape of Rust's
 * {:?}: 1.0, 0.1, 1e16, 1.5e-7, NaN, inf, -0.0); implemented only under FASCON2_FLOAT */
void fascon_put_f32(float v);
void fascon_put_f64(double v);

/* ───── 浮動小数点の補助 / float helpers ───── */
/* ビットの並み（to_bits / from_bits）。memcpy なので型の別名の規則に触れない / bit patterns (to_bits / from_bits) through
 * memcpy, so the aliasing rules are never broken */
static inline FASCON_MAYBE_UNUSED uint32_t f20_f32_to_bits(float v) {
    uint32_t b;
    __builtin_memcpy(&b, &v, sizeof b);
    return b;
}
static inline FASCON_MAYBE_UNUSED uint64_t f20_f64_to_bits(double v) {
    uint64_t b;
    __builtin_memcpy(&b, &v, sizeof b);
    return b;
}
static inline FASCON_MAYBE_UNUSED float f20_f32_from_bits(uint32_t b) {
    float v;
    __builtin_memcpy(&v, &b, sizeof v);
    return v;
}
static inline FASCON_MAYBE_UNUSED double f20_f64_from_bits(uint64_t b) {
    double v;
    __builtin_memcpy(&v, &b, sizeof v);
    return v;
}
static inline FASCON_MAYBE_UNUSED bool f20_f32_is_nan(float v) { return (f20_f32_to_bits(v) & 0x7FFFFFFFu) > 0x7F800000u; }
static inline FASCON_MAYBE_UNUSED bool f20_f64_is_nan(double v) { return (f20_f64_to_bits(v) & UINT64_C(0x7FFFFFFFFFFFFFFF)) > UINT64_C(0x7FF0000000000000); }
static inline FASCON_MAYBE_UNUSED bool f20_f32_is_finite(float v) { return (f20_f32_to_bits(v) & 0x7F800000u) != 0x7F800000u; }
static inline FASCON_MAYBE_UNUSED bool f20_f64_is_finite(double v) { return (f20_f64_to_bits(v) & UINT64_C(0x7FF0000000000000)) != UINT64_C(0x7FF0000000000000); }
static inline FASCON_MAYBE_UNUSED float f20_f32_abs(float v) { return f20_f32_from_bits(f20_f32_to_bits(v) & 0x7FFFFFFFu); }
static inline FASCON_MAYBE_UNUSED double f20_f64_abs(double v) { return f20_f64_from_bits(f20_f64_to_bits(v) & UINT64_C(0x7FFFFFFFFFFFFFFF)); }
/* 浮動小数点 → 整数の飽和変換（Rust の `as` と同じ: 範囲外は端の値、NaN は 0）。C の範囲外の変換は未定義動作なので、
 * 範囲を先に比べる（f32 は double へ正確に広げてから） / saturating float to integer conversion (Rust's `as`: out-of-range
 * values clamp to the bounds, NaN becomes 0); C's out-of-range conversion is undefined, so the range is compared first (f32
 * widened to double exactly) */
static inline FASCON_MAYBE_UNUSED int64_t f20_f2s(double v, int64_t lo, int64_t hi) {
    if (v != v) return 0;
    if (v <= (double)lo) return lo;
    if (v >= (double)hi) return hi;
    return (int64_t)v;
}
static inline FASCON_MAYBE_UNUSED uint64_t f20_f2u(double v, uint64_t hi) {
    if (!(v > 0.0)) return 0u; /* NaN・負・0 / NaN, negative, zero */
    if (v >= (double)hi) return hi;
    return (uint64_t)v;
}
void fascon_put_hex(uint64_t v);
void fascon_put_bytes(const f20_slice_t *s);
#define fascon_halt() fascon_plat_halt()
#define fascon_exit(c) fascon_plat_exit(c)

/* 文字列（コンパイル時のリテラルだけ・Kernel リージョン） / strings (compile-time literals only, Kernel region) */
typedef struct {
    const char *ptr;
    uint32_t len;
} FasconStr;
#define FASCON_STR(lit) ((FasconStr){(lit), (uint32_t)(sizeof(lit) - 1u)})
void fascon_print_str(FasconStr s);
void fascon_put_str(FasconStr s);
static inline bool fascon_str_eq(FasconStr a, FasconStr b) {
    if (a.len != b.len) return false;
    for (uint32_t i = 0; i < a.len; i++)
        if (a.ptr[i] != b.ptr[i]) return false;
    return true;
}

FASCON_NORETURN void fascon_rt_arith_panic(const char *what);

/* ───────────── 検査付き整数演算 / checked integer arithmetic ───────────── */
#ifndef FASCON_RT_H
#define FASCON_DEF_ARITH(name, T, MINV, BITS)                                                         \
    static inline T fascon_add_##name(T a, T b) {                                                     \
        T r;                                                                                          \
        if (FASCON_UNLIKELY(__builtin_add_overflow(a, b, &r))) fascon_rt_arith_panic("add overflow"); \
        return r;                                                                                     \
    }                                                                                                 \
    static inline T fascon_sub_##name(T a, T b) {                                                     \
        T r;                                                                                          \
        if (FASCON_UNLIKELY(__builtin_sub_overflow(a, b, &r))) fascon_rt_arith_panic("sub overflow"); \
        return r;                                                                                     \
    }                                                                                                 \
    static inline T fascon_mul_##name(T a, T b) {                                                     \
        T r;                                                                                          \
        if (FASCON_UNLIKELY(__builtin_mul_overflow(a, b, &r))) fascon_rt_arith_panic("mul overflow"); \
        return r;                                                                                     \
    }                                                                                                 \
    static inline T fascon_div_##name(T a, T b) {                                                     \
        if (FASCON_UNLIKELY(b == 0)) fascon_rt_arith_panic("division by zero");                      \
        if (FASCON_UNLIKELY(MINV != 0 && a == (T)MINV && b == (T)-1)) fascon_rt_arith_panic("div overflow"); \
        return (T)(a / b);                                                                            \
    }                                                                                                 \
    static inline T fascon_rem_##name(T a, T b) {                                                     \
        if (FASCON_UNLIKELY(b == 0)) fascon_rt_arith_panic("remainder by zero");                     \
        if (FASCON_UNLIKELY(MINV != 0 && a == (T)MINV && b == (T)-1)) return 0;                       \
        return (T)(a % b);                                                                            \
    }                                                                                                 \
    static inline T fascon_shl_##name(T a, uint32_t s) {                                              \
        if (FASCON_UNLIKELY(s >= BITS)) fascon_rt_arith_panic("shift overflow");                     \
        return (T)((uint64_t)a << s);                                                                 \
    }                                                                                                 \
    static inline T fascon_shr_##name(T a, uint32_t s) {                                              \
        if (FASCON_UNLIKELY(s >= BITS)) fascon_rt_arith_panic("shift overflow");                     \
        return (T)(a >> s);                                                                           \
    }                                                                                                 \
    static inline T fascon_neg_##name(T a) {                                                          \
        T r;                                                                                          \
        if (FASCON_UNLIKELY(__builtin_sub_overflow((T)0, a, &r))) fascon_rt_arith_panic("neg overflow"); \
        return r;                                                                                     \
    }
FASCON_DEF_ARITH(i8, int8_t, INT8_MIN, 8)
FASCON_DEF_ARITH(i16, int16_t, INT16_MIN, 16)
FASCON_DEF_ARITH(i32, int32_t, INT32_MIN, 32)
FASCON_DEF_ARITH(i64, int64_t, INT64_MIN, 64)
FASCON_DEF_ARITH(isize, intptr_t, INTPTR_MIN, (sizeof(intptr_t) * 8))
FASCON_DEF_ARITH(u8, uint8_t, 0, 8)
FASCON_DEF_ARITH(u16, uint16_t, 0, 16)
FASCON_DEF_ARITH(u32, uint32_t, 0, 32)
FASCON_DEF_ARITH(u64, uint64_t, 0, 64)
FASCON_DEF_ARITH(usize, uintptr_t, 0, (sizeof(uintptr_t) * 8))
#endif /* !FASCON_RT_H */

/* ───────────── 添字の検査 / index checks ───────────── */

FASCON_NORETURN void fascon_bounds_panic(uint64_t index, uint64_t len, const char *site);

/* 符号なしの添字: v < len を確かめて返す / unsigned index: check v < len and return it */
static inline uintptr_t fascon_idx_u(uint64_t v, uint64_t len, const char *site) {
    if (FASCON_UNLIKELY(v >= len)) fascon_bounds_panic(v, len, site);
    return (uintptr_t)v;
}
/* 符号付きの添字: 0 <= v < len / signed index: 0 <= v < len */
static inline uintptr_t fascon_idx_s(int64_t v, uint64_t len, const char *site) {
    if (FASCON_UNLIKELY(v < 0 || (uint64_t)v >= len)) fascon_bounds_panic((uint64_t)v, len, site);
    return (uintptr_t)v;
}
static inline uint8_t fascon_str_byte(FasconStr s, uint64_t i, const char *site) {
    return (uint8_t)s.ptr[fascon_idx_u(i, s.len, site)];
}

/* ───────────── 32 ビット Id と世代タグ付きアリーナ / 32-bit Ids and generational arenas ─────────────
 *
 *   f20_id_t (uint32_t) = [ 世代 generation : 32 - K | 添字 index : K ]
 *
 *   K はアリーナごとにコンパイラが容量から決める（K = ceil(log2(容量))、1〜24）。ソースの文法は変わらない。
 *   K is chosen per arena by the compiler from the capacity (K = ceil(log2(capacity)), 1..24); the syntax does not change.
 *     容量 64            → K = 6 : 世代 26 ビット = 1 スロット 67,108,863 回 × 64 = アリーナ全体で約 42.9 億回
 *     容量 32,769〜65,536 → K = 16: 世代 16 ビット | 添字 16 ビット
 *     どの容量でも「容量 × (2^(32-K) - 1)」≥ 約 21.5 億回（容量が 2 の累乗なら約 42.9 億回）
 *     for every capacity, capacity × (2^(32-K) - 1) is at least ~2.15e9 (~4.29e9 for powers of two)
 *
 *   - 表の大きさは常に 2^K スロット + 番兵 1（容量を超える分は配らない詰め物）。添字は id & マスクで取り出すだけなので、
 *     どんな値の Id でも表の外は指せない（境界外アクセスは構造上起きず、比較命令も分岐も要らない）。
 *     the table always holds 2^K slots + 1 sentinel (slots past the capacity are padding that is never handed out).
 *     The index is just id & mask, so no Id value of any kind can point outside the table: out-of-bounds access
 *     cannot happen by construction, with no compare and no branch.
 *   - 世代は 1〜2^(32-K)-1。0 は「無効」（Option<Id> の None = 0）。詰め物と未使用のスロットの世代は 0 のまま。
 *     generations run 1..2^(32-K)-1; 0 is "invalid" (Option<Id>::None = 0). Padding and unused slots keep generation 0.
 *   - 解放でスロットを 0 で埋めて世代を +1。使い切ったスロットは引退させ、二度と配らない（世代は一周しない = ABA なし）。
 *     freeing zero-fills the slot and bumps its generation; an exhausted slot retires and is never handed out again
 *     (generations never wrap, so there is no ABA).
 *   - 死んだスロットは必ず 0。だから「生存スロットの合計」は生存印を読まず、先頭から分岐なしで足すだけで求まる。
 *     dead slots are always zero, so a sum over live slots is a branch-free sum from the start, without reading the live marks.
 *   - すべて 0 で初期化された状態（.bss）がそのまま「空のアリーナ」。 / an all-zero (.bss) state is an empty arena.
 */
/* f20_id_t はヘッダの先頭で定義済み（FASCON2_ID64 なら 64 ビット） / f20_id_t is defined at the top of this header (64 bits
 * under FASCON2_ID64) */

/* 世代の格納幅は 32-K ビットが収まる最小の型（K = 24 → 8 ビット、16〜23 → 16 ビット、それ以下 → 32 ビット）。
 * 大きな表ほど gen[] が細くなり、ばらばらに引いてもキャッシュに残る（容量 6 万のアリーナで gen[] は 128 KiB）。
 * generations are stored in the narrowest type that holds 32-K bits (K = 24 → 8 bits, 16..23 → 16 bits, else 32 bits):
 * the bigger the table, the thinner gen[] gets, so it stays in cache under scattered access (128 KiB for 60,000 slots). */
typedef uint8_t f20_gen8_t;
typedef uint16_t f20_gen16_t;
typedef uint32_t f20_gen32_t;
typedef f20_gen32_t f20_gen_t; /* いちばん広い世代 / the widest generation */

#define F20_ALIGN16 __attribute__((aligned(16)))
#define F20_ALIGN64 __attribute__((aligned(64))) /* キャッシュライン / a cache line */
#define F20_RESTRICT __restrict__

/* キャッシュラインと偽共有 / cache lines and false sharing
 *   F20_CACHE_LINE        : 64 B。x86-64・Cortex-A・Neoverse の L1 ライン / the L1 line of x86-64, Cortex-A and Neoverse
 *   F20_DESTRUCTIVE_SIZE  : 128 B。別のコアが書く物どうしを離す距離。Intel の隣接ラインプリフェッチは 64 B を 2 本組で取り、
 *                           Apple M シリーズのラインは 128 B なので、64 B 離しただけでは偽共有が残る（C++17 の
 *                           hardware_destructive_interference_size、crossbeam の CachePadded と同じ値）
 *                           how far apart data written by different cores must be: Intel's adjacent-line prefetcher
 *                           pairs 64-byte lines and Apple M-series lines are 128 bytes, so 64 bytes is not enough
 *                           (the same value as hardware_destructive_interference_size in practice and crossbeam's CachePadded) */
#define F20_CACHE_LINE 64u
#define F20_DESTRUCTIVE_SIZE 128u
#define F20_ALIGN128 __attribute__((aligned(128)))

/* ビット走査（分岐なし） / bit scans (branch-free)
 *   f20_ctz64(m)  : 最下位の 1 の位置（m != 0 のとき）。次の生存スロットを 1 命令で求める
 *                   the position of the lowest set bit (m != 0): the next live slot in one instruction
 *                   x86-64: TZCNT（BMI1、-mbmi / -march=x86-64-v3。無ければ同じ結果の BSF）
 *                   AArch64: RBIT + CLZ（各 1 サイクル）。FEAT_CSSC（-march=armv8.9-a / armv9.4-a）なら CTZ 1 命令
 *                   （ARM の BRK はブレークポイント命令で、ビット走査ではない）
 *                   x86-64: TZCNT (BMI1, -mbmi / -march=x86-64-v3; otherwise BSF, same result for m != 0)
 *                   AArch64: RBIT + CLZ (one cycle each); with FEAT_CSSC (-march=armv8.9-a / armv9.4-a) a single CTZ
 *                   (ARM's BRK is the breakpoint instruction, not a bit scan)
 *   f20_blsr64(m) : 最下位の 1 を消す。x86-64 は BLSR 1 命令、AArch64 は SUB + AND / clear the lowest set bit:
 *                   one BLSR on x86-64, SUB + AND on AArch64 */
static inline uint32_t f20_ctz64(uint64_t m) { return (uint32_t)__builtin_ctzll(m); }
static inline uint64_t f20_blsr64(uint64_t m) { return m & (m - 1u); }
static inline uint32_t f20_popcount64(uint64_t m) { return (uint32_t)__builtin_popcountll(m); }
/* m の最下位の 1 とそれより下を消す（その次から先の 1 だけ残す） / clear the lowest set bit and everything below it */
static inline uint64_t f20_above_lowest64(uint64_t m) { return ~(((m & (UINT64_C(0) - m)) << 1) - 1u); }

/* 分岐なしの条件付き選択（k は 0 か 1） / a conditional select without a branch (k is 0 or 1)
 *   clang: ?: に __builtin_unpredictable を付ける → AArch64 は CSEL、x86-64 は CMOV（1 命令）
 *   GCC  : ?: は「予測が効く」と見なされると分岐に戻されるので、全ビット 1 / 0 のマスクで書く → AND / BIC / ORR（分岐なし）
 *   どちらもジャンプは出ない（tests/asm で 4 つのコンパイラの内側のループを確かめている）
 *   clang: ?: with __builtin_unpredictable → CSEL on AArch64, CMOV on x86-64 (one instruction)
 *   GCC  : turns ?: back into a branch when it guesses the branch is predictable, so an all-ones / all-zeros mask is
 *          used instead → AND / BIC / ORR (no branch)
 *   neither emits a jump (checked on the inner loops of four compilers) */
#if defined(__clang__)
#define F20_SEL64(k, a, b) (__builtin_unpredictable(k) ? (uint64_t)(a) : (uint64_t)(b))
#define F20_SEL32(k, a, b) (__builtin_unpredictable(k) ? (uint32_t)(a) : (uint32_t)(b))
#else
#define F20_SEL64(k, a, b) ((((uint64_t)(a)) & (UINT64_C(0) - (uint64_t)(k))) | (((uint64_t)(b)) & ~(UINT64_C(0) - (uint64_t)(k))))
#define F20_SEL32(k, a, b) ((((uint32_t)(a)) & (0u - (uint32_t)(k))) | (((uint32_t)(b)) & ~(0u - (uint32_t)(k))))
#endif

/* 最小 / 最大の更新 / updating a running minimum / maximum
 *   k = 生存ビット AND 条件 AND (v < 今の最小) はいつも分岐なしで作る（条件は予測できないことが多い）。
 *   その k での更新は、既定では「めったに真にならない分岐」1 本にする: 走査が進むほど新しい最小は出にくく
 *   （ばらばらな値で平均 ln n 回）、予測がほぼ外れないので、毎スロット CSEL / CMOV で 2 つの値を選び直すより速い
 *   （bench/RESULTS.md: pick_next 0.7〜0.9 ns 対 1.5〜2.6 ns）。
 *   FASCON2_PICK_BRANCHLESS を定義すると更新も F20_SEL64 / F20_SEL32（CSEL / CMOV / マスク）になり、データに依存する
 *   分岐が一本もなくなる（実行時間をデータに依存させたくないコード向け）
 *   k = live bit AND filter AND (v < current minimum) is always computed without a branch (filters are often
 *   unpredictable). By default the update on k is one rarely-taken branch: new minima get rarer as the scan proceeds
 *   (about ln n times for values in random order), so it almost never mispredicts and is faster than re-selecting two
 *   values with CSEL / CMOV on every slot (bench/RESULTS.md: pick_next 0.7-0.9 ns against 1.5-2.6 ns).
 *   Define FASCON2_PICK_BRANCHLESS to make the update F20_SEL64 / F20_SEL32 too (CSEL / CMOV / masks): then no branch
 *   depends on the data at all (for code whose timing must not depend on the data) */
#if defined(FASCON2_PICK_BRANCHLESS)
#define F20_PICK(k, T, bv, v, iv, idx)              \
    do {                                            \
        (bv) = (T)F20_SEL64((k), (v), (bv));        \
        (iv) = F20_SEL32((k), (idx), (iv));         \
    } while (0)
#else
#define F20_PICK(k, T, bv, v, iv, idx)              \
    do {                                            \
        if (FASCON_UNLIKELY(k)) {                   \
            (bv) = (v);                             \
            (iv) = (idx);                           \
        }                                           \
    } while (0)
#endif

#define F20_SLOTS(K) (1u << (K))                                      /* 表のスロット数 / slots in the table */
#define F20_IDX_MASK(K) ((uint32_t)((1u << (K)) - 1u))                /* 添字のマスク / index mask */
#if defined(FASCON2_ID64)
/* --target generic-os: f20_id_t = uint64_t [上位 32 = 世代 | 下位 32 = 添字]。シフトはすべて uint64_t（1ULL << K の幅）で
 * 行う。表の大きさ（2^K）・番兵・マスクの仕組みは 32 ビットのときと同じで、世代はいつも 32 ビット（gen[] は uint32_t）
 * --target generic-os: f20_id_t = uint64_t [upper 32 = generation | lower 32 = index]; every shift is done in uint64_t
 * (1ULL << K wide). The table size (2^K), the sentinel and the masking work as with 32 bits; generations are always
 * 32 bits (gen[] is uint32_t) */
#define F20_GEN_MAX(K) ((uint32_t)0xFFFFFFFFu + 0u * (uint32_t)(K))                              /* 最後の世代 / last generation */
#define F20_ID_INDEX(id, K) ((uint32_t)((uint64_t)(id) & (uint64_t)F20_IDX_MASK(K)))              /* and 1 命令 / one AND */
/* K は 64 ビット Id では使わない（0 * K で参照だけして、使わない引数の警告を出さない） / K is unused with 64-bit Ids (0 * K
 * only references it, so no unused-parameter warning) */
#define F20_ID_GEN(id, K) ((uint32_t)((uint64_t)(id) >> 32) + 0u * (uint32_t)(K))                 /* lsr 1 命令 / one LSR */
#define F20_ID_MAKE(g, i, K) ((f20_id_t)((((uint64_t)(uint32_t)(g) << 32) | (uint64_t)(uint32_t)(i)) + 0u * (uint64_t)(K))) /* bfi 1 命令 / one BFI */
/* 添字の上位（K ビットより上・32 ビット未満）が 0 か: 外から来た生の値の正規形の検査 / whether the index bits above K are
 * zero: the canonical-form check for raw values from outside */
#define F20_ID_CANON(id, K) ((uint32_t)(((uint64_t)(id) & UINT64_C(0xFFFFFFFF)) >> (K)) == 0u)
#else
#define F20_GEN_MAX(K) ((uint32_t)(0xFFFFFFFFu >> (K)))               /* 最後の世代 / last generation */
#define F20_ID_INDEX(id, K) ((uint32_t)(id) & F20_IDX_MASK(K))        /* and 1 命令 / one AND */
#define F20_ID_GEN(id, K) ((uint32_t)(id) >> (K))                     /* lsr 1 命令 / one LSR */
#define F20_ID_MAKE(g, i, K) (((uint32_t)(g) << (K)) | (uint32_t)(i)) /* orr + lsl 1 命令 / one ORR with LSL */
#define F20_ID_CANON(id, K) 1u
#endif
#define F20_LIVE 0xFFFFFFFFu                                          /* link[] の生存印 / live mark in link[] */

/* 遅延フォールト（ラッチ）: 生存を証明できない Id の世代が合わなかったときや、合計があふれたときに立てるビット
 * deferred faults (the latch): bits set when an unproven Id has the wrong generation or a sum overflows */
#define F20_FAULT_STALE 0x1u
#define F20_FAULT_ARITH 0x2u

/* アリーナの管理情報 / bookkeeping of one arena */
typedef struct F20_ALIGN16 {
    uint32_t next_unused; /* まだ一度も使っていないスロットの先頭（走査の上限） / first never-used slot (the scan bound) */
    uint32_t free_head;   /* 再利用待ち FIFO（添字 + 1、0 = 空） / reuse FIFO (index + 1, 0 = empty) */
    uint32_t free_tail;
    uint32_t live;        /* 生存スロット数 / live slots */
    uint32_t retired;     /* 引退したスロット数 / retired slots */
    uint32_t pad_;
    uint64_t allocs;      /* 確保の累計 / total allocations */
} f20_arena_state_t;

/* スロットとアリーナの型（要素型ごとにコンパイラが 1 回ずつ作る）
 *   f20_slot_t(tag)  : 16 バイト境界にそろえたスロット（ベクトル命令の読み込みがそろう）。要素の構造体に
 *                      #[repr(align(N))] があれば N バイト境界（N = 64 で 1 スロット 1 ライン、128 で隣接ラインとも分離）
 *   f20_arena_t(tag) : st | live[] | gen[2^K] | link[2^K] | slot[2^K + 1]（SoA。各配列は 128 バイト境界から始まる）
 *                      slot[2^K] は番兵: 生存を証明できない Id の世代が合わなかったとき、書き込みはここへ逸れる
 *                      （本物のスロットは汚れない。番兵は走査にも総和にも入らない）
 * slot and arena types (the compiler stamps one pair out per element type)
 *   f20_slot_t(tag)  : a slot aligned to 16 bytes (vector loads stay aligned); N bytes when the element struct has
 *                      #[repr(align(N))] (N = 64: one slot per line; 128: apart from the adjacent line too)
 *   f20_arena_t(tag) : st | live[] | gen[2^K] | link[2^K] | slot[2^K + 1] (SoA; every array starts on a 128-byte boundary)
 *                      slot[2^K] is the sentinel: when an unproven Id has the wrong generation, writes go there
 *                      (no real slot is touched; the sentinel is never scanned or summed)
 *
 * 配置の理由 / why this layout
 *   - 小さく熱い管理情報（st・live・gen）を前に、大きなスロットの列を後ろに置く。表が 2 MiB 境界から始まるので、
 *     管理情報とスロットの大部分が最初の 2 MiB ページに入る。slot[] を先頭に置く形も試したが、ばらばらの順の連鎖が
 *     GCC で 20〜30 % 遅くなった（gen[] がスロットから 2 MiB 離れる）ので採らなかった（大きなページが効いていることを
 *     確かめる前の測定）
 *     the small, hot bookkeeping (st, live, gen) goes first and the big slot array last. The table starts on a 2 MiB
 *     boundary, so the bookkeeping and most of the slots share the first 2 MiB page. Putting slot[] first was tried as
 *     well: scattered chains got 20-30 % slower with GCC (gen[] then sits 2 MiB away from the slots), so it was not kept
 *     (measured before huge pages were confirmed to be in effect)
 *   - st（確保・解放のたびに書く管理情報）は 128 バイトのブロックに 1 個だけ。世代の検査（gen[] を読む）や走査
 *     （live[] と slot[] を読む）をする別のコアのラインを、確保の書き込みで無効化しない
 *     st (written by every alloc / free) has a 128-byte block to itself, so allocation never invalidates the lines that
 *     other cores read for generation checks (gen[]) or scans (live[], slot[])
 *   - live[]: 1 スロット 1 ビットの生存ビットマップ（64 スロット = uint64_t 1 語）。走査は 1 語を読み、次の生存スロットを
 *     ctz で求める。65,536 スロットでも 8 KiB なので L1 に収まる。生存印をスロットと同じラインに置かないので、死んだ
 *     スロットのラインは読まずに済む
 *     live[]: a liveness bitmap, one bit per slot (64 slots = one uint64_t); scans read one word and find the next live
 *     slot with ctz. 8 KiB even for 65,536 slots, so it stays in L1, and dead slots' lines are never touched for liveness
 *   - マルチコアでの分担（1.0 の BSP は CPU0 だけ）: 512 スロット単位でコアに割り当てると、live[]（64 B）・gen[]・link[]・
 *     slot[] のどの配列でも担当範囲が 128 バイト境界で切れ、どのラインも 1 つのコアしか書かない（F20_SHARD_SLOTS）
 *     sharing between cores (the 1.0 BSP runs CPU0 only): give each core whole 512-slot shards; then in live[] (64 B),
 *     gen[], link[] and slot[] every shard boundary is a 128-byte boundary and each line is written by one core only */
#define F20_SENTINEL(K) F20_SLOTS(K) /* 番兵スロットの添字 / index of the sentinel slot */
#define f20_slot_t(tag) f20_slot_##tag##_t
#define f20_arena_t(tag) f20_arena_##tag##_t
#define F20_GEN_BITS(K) (32u - (K)) /* 世代のビット数 / generation bits */
#define F20_LIVE_WORDS(K) ((F20_SLOTS(K) + 63u) >> 6)    /* 生存ビットマップの語数 / words in the live bitmap */
#define F20_LIVE_WORDS_USED(nu) (((uint32_t)(nu) + 63u) >> 6) /* next_unused までを覆う語数 / words covering next_unused */
#define F20_SHARD_SLOTS 512u /* コアに割り当てる単位（どの配列でも 128 バイトの倍数） / per-core shard (a multiple of 128 B in every array) */
#define F20_DEFINE_SLOT(tag, Elem) \
    typedef struct F20_ALIGN16 {   \
        Elem v;                    \
    } f20_slot_##tag##_t
#define F20_DEFINE_TABLE(tag, K, GenT)                                                \
    typedef struct F20_ALIGN128 {                                                     \
        F20_ALIGN128 f20_arena_state_t st;                                            \
        uint8_t st_pad_[F20_DESTRUCTIVE_SIZE - sizeof(f20_arena_state_t)];            \
        F20_ALIGN128 uint64_t live[F20_LIVE_WORDS(K)];                                \
        F20_ALIGN128 GenT gen[F20_SLOTS(K)];                                          \
        F20_ALIGN128 uint32_t link[F20_SLOTS(K)];                                     \
        F20_ALIGN128 f20_slot_##tag##_t slot[F20_SLOTS(K) + 1u];                      \
    } f20_arena_##tag##_t
#define F20_DEFINE_ARENA(tag, Elem, K, GenT) \
    F20_DEFINE_SLOT(tag, Elem);              \
    F20_DEFINE_TABLE(tag, K, GenT)

/* アリーナの置き場所 / where an arena lives
 *   ELF（ベアメタルと Linux）: .bss.fascon_arena セクション。1 MiB 以上の表は 2 MiB 境界（AArch64 の 4 KiB 粒度の
 *   レベル 2 ブロック・x86-64 のラージページ 1 枚の大きさ）。BSP のリンカスクリプトはこのセクションを 2 MiB 境界の
 *   .fascon_arena にまとめ、PC 上の実行では madvise(MADV_HUGEPAGE) を掛ける
 *   ELF (bare metal and Linux): the .bss.fascon_arena section; tables of 1 MiB or more get a 2 MiB boundary (one AArch64
 *   level-2 block with the 4 KiB granule, one x86-64 large page). The BSP linker script gathers the section into a
 *   2 MiB-aligned .fascon_arena; runs on a Linux PC apply madvise(MADV_HUGEPAGE)
 *   Mach-O・PE（macOS・Windows の PC 実行）: 128 バイト境界だけ（どちらの形式も 2 MiB の整列を持てない）
 *   Mach-O and PE (PC runs on macOS / Windows): 128-byte alignment only (neither format can carry 2 MiB alignment) */
#define F20_HUGE_PAGE 0x200000u
#define F20_HUGE_MIN 0x100000u
#if defined(__ELF__) && defined(FASCON2_HPC)
/* --target hpc: 大きさによらずすべての表を 2 MiB 境界に（PC の実行層が MADV_HUGEPAGE を掛ける） / --target hpc: every table
 * on a 2 MiB boundary whatever its size (the PC layer applies MADV_HUGEPAGE) */
#define F20_ARENA_STORAGE(T) __attribute__((section(".bss.fascon_arena"), aligned(F20_HUGE_PAGE)))
#elif defined(__ELF__)
#define F20_ARENA_STORAGE(T) __attribute__((section(".bss.fascon_arena"), aligned(sizeof(T) >= F20_HUGE_MIN ? F20_HUGE_PAGE : F20_DESTRUCTIVE_SIZE)))
#else
#define F20_ARENA_STORAGE(T) __attribute__((aligned(F20_DESTRUCTIVE_SIZE)))
#endif
/* gpu_arena の置き場所: .bss.fascon_gpu（ELF）の 2 MiB 境界。装置と共有するので大きさによらず境界をそろえ、ピン留め
 * （Unified Memory / Pinned Host Memory の登録）がページをほかの表と分け合わないようにする。PE / Mach-O は 4 KiB 境界
 * where a gpu_arena lives: a 2 MiB boundary in .bss.fascon_gpu (ELF). It is shared with the device, so the boundary does
 * not depend on the size and pinning (registering Unified Memory / Pinned Host Memory) never shares a page with another
 * table. 4 KiB boundaries on PE / Mach-O */
#if defined(__ELF__)
#define F20_GPU_STORAGE(T) __attribute__((section(".bss.fascon_gpu"), aligned(F20_HUGE_PAGE)))
#else
#define F20_GPU_STORAGE(T) __attribute__((aligned(4096)))
#endif

/* 生成 C が出すアリーナ表（BSP が起動時に TLB の覆い方を表示し、PC の実行層が大きなページを頼むのに使う）
 * the arena table emitted by generated C (the BSP prints how the TLB covers it; the PC layer asks for huge pages) */
typedef struct {
    const char *name;
    const void *base;
    uintptr_t size;
    uint32_t slots;      /* 2^K + 1（番兵込み） / 2^K + 1 (with the sentinel) */
    uint32_t slot_bytes;
} FasconArenaRegion;
extern const FasconArenaRegion fascon2_arena_regions[];
extern const uint32_t fascon2_arena_region_count;

/* 確保・解放・検証の本体（世代の幅ごとに 1 組） / alloc, free and validate (one set per generation width)
 *   acquire : 空きスロットを取って Id を返す（満杯なら 0 = None） / take a free slot, return its Id (0 = None when full)
 *   release : スロットを返却して世代を進める（中身はすでに 0 で埋めてあること）。最後の世代なら引退
 *             return a slot and advance its generation (contents already zero-filled); retire it after the last one
 *   validate: 外から来た生の値（システムコールの引数など）を Id にする。分岐なし（無効なら 0 = None）
 *             turn a raw value from outside (e.g. a syscall argument) into an Id, branch-free (0 = None when invalid) */
#define F20_ARENA_OPS(W)                                                                                               \
    static inline f20_id_t f20_arena_acquire_g##W(f20_arena_state_t *F20_RESTRICT st, f20_gen##W##_t *F20_RESTRICT gen, \
                                                  uint32_t *F20_RESTRICT link, uint64_t *F20_RESTRICT live, uint32_t cap, \
                                                  uint32_t K) {                                                        \
        uint32_t i;                                                                                                    \
        if (st->free_head != 0u) {                                                                                     \
            i = st->free_head - 1u;                                                                                    \
            st->free_head = link[i];                                                                                   \
            if (st->free_head == 0u) st->free_tail = 0u;                                                               \
        } else if (st->next_unused < cap) {                                                                            \
            i = st->next_unused++;                                                                                     \
            gen[i] = 1u;                                                                                               \
        } else {                                                                                                       \
            return 0u;                                                                                                 \
        }                                                                                                              \
        link[i] = F20_LIVE;                                                                                            \
        live[i >> 6] |= UINT64_C(1) << (i & 63u); /* 生存ビット / live bit */                                         \
        st->live++;                                                                                                    \
        st->allocs++;                                                                                                  \
        return F20_ID_MAKE(gen[i], i, K);                                                                              \
    }                                                                                                                  \
    static inline void f20_arena_release_g##W(f20_arena_state_t *F20_RESTRICT st, f20_gen##W##_t *F20_RESTRICT gen,    \
                                              uint32_t *F20_RESTRICT link, uint64_t *F20_RESTRICT live, uint32_t i,    \
                                              uint32_t K) {                                                            \
        st->live--;                                                                                                    \
        link[i] = 0u;                                                                                                  \
        live[i >> 6] &= ~(UINT64_C(1) << (i & 63u));                                                                   \
        if ((uint32_t)gen[i] == F20_GEN_MAX(K)) {                                                                      \
            gen[i] = 0u; /* 世代を使い切った: 引退（どの Id とも一致しない） / exhausted: retired (matches no Id) */      \
            st->retired++;                                                                                             \
            return;                                                                                                    \
        }                                                                                                              \
        gen[i] = (f20_gen##W##_t)(gen[i] + 1u);                                                                        \
        if (st->free_tail == 0u) {                                                                                     \
            st->free_head = st->free_tail = i + 1u;                                                                    \
        } else {                                                                                                       \
            link[st->free_tail - 1u] = i + 1u;                                                                         \
            st->free_tail = i + 1u;                                                                                    \
        }                                                                                                              \
    }                                                                                                                  \
    static inline f20_id_t f20_arena_validate_g##W(const f20_gen##W##_t *F20_RESTRICT gen,                            \
                                                   const uint32_t *F20_RESTRICT link, uint32_t cap, uint32_t K,         \
                                                   f20_id_t raw) {                                                     \
        const uint32_t i = F20_ID_INDEX(raw, K), g = F20_ID_GEN(raw, K);                                               \
        const uint32_t ok = (uint32_t)(i < cap) & (uint32_t)(g != 0u) & (uint32_t)(link[i] == F20_LIVE) &              \
                            (uint32_t)((uint32_t)gen[i] == g) & (uint32_t)F20_ID_CANON(raw, K);                        \
        return ok ? raw : (f20_id_t)0u;                                                                                \
    }
F20_ARENA_OPS(8)
F20_ARENA_OPS(16)
F20_ARENA_OPS(32)

/* 世代の配列の型で 3 組から選ぶ（呼び出し側は幅を気にしなくてよい） / pick one of the three sets by the type of gen[] */
#define f20_arena_acquire(st, gen, link, live, cap, K) \
    _Generic((gen), f20_gen8_t *: f20_arena_acquire_g8, f20_gen16_t *: f20_arena_acquire_g16, default: f20_arena_acquire_g32)(st, gen, link, live, cap, K)
#define f20_arena_release(st, gen, link, live, i, K) \
    _Generic((gen), f20_gen8_t *: f20_arena_release_g8, f20_gen16_t *: f20_arena_release_g16, default: f20_arena_release_g32)(st, gen, link, live, i, K)
#define f20_arena_validate(gen, link, cap, K, raw)                                                                       \
    _Generic((gen), f20_gen8_t *: f20_arena_validate_g8, const f20_gen8_t *: f20_arena_validate_g8,                     \
             f20_gen16_t *: f20_arena_validate_g16, const f20_gen16_t *: f20_arena_validate_g16, default: f20_arena_validate_g32)(gen, link, cap, K, raw)

/* ───────────── バンプアリーナ（bump_arena） / bump arenas (bump_arena) ─────────────
 *
 *   bump_arena 名前: 型[容量];  — 呼び出しリージョンごとに O(1) で一括解放するアリーナ
 *   bump_arena name: T[capacity]; — an arena released in bulk, in O(1), per call region
 *
 *   - 確保 = top を 1 進める（比較 1 回 + 格納）。free は無い / allocation = advance top by one (one compare + stores); no free
 *   - 解放 = 確保した関数（呼び出しリージョン）が戻るとき `top = 入口の印` の 1 回の格納でまとめて
 *     release = one store `top = mark at entry` when the allocating function (the call region) returns
 *   - Id は 64 ビット [世代 64-K | 添字 K]。スロットを確保するたびにそのスロットの世代を +1 する（1 スロット 2^(64-K) - 1 回まで。
 *     使い切ったスロットからは確保しない = 世代は一周しない = ABA なし）
 *     Ids are 64 bits [generation 64-K | index K]; every allocation of a slot bumps its generation (up to 2^(64-K) - 1 per
 *     slot; an exhausted slot is never handed out again = generations never wrap = no ABA)
 *   - 検査は「添字 < top かつ 世代が一致」。巻き戻した後の古い Id は top の外か世代違いなので None（番兵へ逸れてラッチ）
 *     the check is "index < top and the generation matches"; after a rewind a stale Id is beyond top or of another
 *     generation, so it is None (writes divert to the sentinel and latch)
 *   - すべて 0 の状態（.bss）がそのまま空のアリーナ / the all-zero (.bss) state is an empty arena
 *
 *   f20_arena_t(tag) : st | gen[2^K]（64 ビット） | slot[2^K + 1]（番兵込み。各配列は 128 バイト境界から） */
typedef uint64_t f20_bid_t;
#define F20_BIDX_MASK(K) ((UINT64_C(1) << (K)) - 1u)                                  /* 添字のマスク / index mask */
#define F20_BID_INDEX(id, K) ((uint32_t)((uint64_t)(id) & F20_BIDX_MASK(K)))          /* and 1 命令 / one AND */
#define F20_BID_GEN(id, K) ((uint64_t)(id) >> (K))                                    /* lsr 1 命令 / one LSR */
#define F20_BID_MAKE(g, i, K) (((uint64_t)(g) << (K)) | (uint64_t)(uint32_t)(i))      /* orr + lsl / ORR with LSL */
#define F20_BGEN_MAX(K) (UINT64_C(0xFFFFFFFFFFFFFFFF) >> (K))                         /* 最後の世代 / last generation */
typedef struct F20_ALIGN16 {
    uint32_t top;    /* 次に配るスロット = 生きているスロット数 / the next slot handed out = the number of live slots */
    uint32_t peak;   /* いちばん深かった top / the deepest top seen */
    uint64_t allocs; /* 確保の累計 / total allocations */
} f20_bump_state_t;
#define F20_DEFINE_BUMP(tag, K)                                                        \
    typedef struct F20_ALIGN128 {                                                      \
        F20_ALIGN128 f20_bump_state_t st;                                              \
        uint8_t st_pad_[F20_DESTRUCTIVE_SIZE - sizeof(f20_bump_state_t)];              \
        F20_ALIGN128 uint64_t gen[F20_SLOTS(K)];                                       \
        F20_ALIGN128 f20_slot_##tag##_t slot[F20_SLOTS(K) + 1u];                       \
    } f20_arena_##tag##_t
/* 外から来た生の 64 ビット値を Id にする（分岐なし。無効なら 0 = None） / turn a raw 64-bit value into an Id (branch-free) */
static inline FASCON_MAYBE_UNUSED f20_bid_t f20_bump_validate(const uint64_t *F20_RESTRICT gen, uint32_t top, uint32_t K, uint64_t raw) {
    const uint32_t i = F20_BID_INDEX(raw, K);
    const uint64_t g = F20_BID_GEN(raw, K);
    const uint32_t ok = (uint32_t)(i < top) & (uint32_t)(g != 0u) & (uint32_t)(gen[i] == g);
    return ok ? raw : 0u;
}

/* ───── 列（集計の列・絞り込みのビット列・選択の鍵の列） / columns (aggregate columns, filter bit columns, pick-key columns) ─────
 *
 * ■ なぜ列を足すのか / why columns
 *   スロットを先頭から読む走査は、32 B のタスクで 8 B の vruntime だけを足すときも 32 B をキャッシュへ運ぶ。走査が何を読むかは
 *   コンパイル時に分かるので、コンパイラは `for id in @a` の集計ごとに、その集計が読む値だけを密に並べた列を表に足す
 *   （ソースに注釈は要らない。--columns off で 1.0.0 と同じ表に戻る）。走査は列だけを読む:
 *     総和・条件付き総和 → 集計の列  agg[i] = 条件 ? 値 : 0          （u64 の値は 8 B、32 ビット以下は 4 B / スロット）
 *     条件付きの個数     → ビット列  fb の i ビット目 = 生存 && 条件  （1 ビット / スロット。個数は popcount の和）
 *     最小 / 最大の選択  → 鍵の列    key[i] = (生存 && 条件) ? enc(値) : F20_KEY_NONE （8 B / スロット。走査は符号付き最小だけ）
 *   A scan that reads slots from the start pulls all 32 bytes of a 32-byte task into the cache even when it only adds
 *   the 8-byte vruntime. What a scan reads is known at compile time, so for every `for id in @a` reduction the compiler
 *   adds a column holding exactly the values that reduction reads (no annotation in the source; --columns off gives the
 *   1.0.0 table back). Scans then read the columns only:
 *     sums and filtered sums → aggregate column agg[i] = filter ? value : 0   (8 B for u64 values, 4 B for 32 bits or fewer)
 *     filtered counts        → filter bit column: bit i = live && filter     (1 bit per slot; the count is a popcount sum)
 *     min / max picks        → pick-key column key[i] = (live && filter) ? enc(value) : F20_KEY_NONE (8 B; the scan is a signed min)
 *
 * ■ 列はスロットの写し（スロットが正本） / columns mirror the slots (the slot stays authoritative)
 *   @a.get(id) が要素へのポインタを返すモデルはそのまま。列は次の場所でだけ書く（コンパイラが arena_<名前>_mirror を出す）:
 *     1. alloc（値を書いたあと）  2. free / remove（集計 0・ビット 0・鍵 NONE）  3. Id での書き込み（列が読むフィールドに
 *     届く道筋のときだけ）  4. &mut の借用の終わり（get_mut の if let / match のスコープの出口。break / continue / return
 *     も含む）と、&mut @a[id] を渡した呼び出しの直後
 *   4 が正しいのは、&mut の参照が生きている間、そのアリーナを読む式・呼び出しと Id での書き込みを F2502 が禁止している
 *   から。借用中の列の古さは誰にも観測できない。
 *   The pointer-returning @a.get(id) model is unchanged. Columns are written only by the arena_<name>_mirror code the
 *   compiler emits: 1. alloc, 2. free / remove (aggregates 0, bits 0, keys NONE), 3. writes by Id whose path reaches a
 *   field a column reads, 4. the end of a &mut borrow (every exit of a get_mut if let / match scope, break / continue /
 *   return included) and right after a call that received &mut @a[id]. (4) is sound because F2502 rejects every read
 *   of the arena, every call that reads it and every write by Id while a &mut reference into it is live.
 *
 * ■ 走査の範囲と番兵 / scan range and sentinels
 *   走査は [0, N)、N = next_unused を 64 に切り上げた値（生存ビットマップの語の境目、N ≤ max(2^K, 64)）。列の番兵は
 *   F20_COL_SENTINEL(K) = max(2^K, 64) で、どの走査も読まない。生存を証明できない Id の書き込みはスロットの番兵へ逸れ、
 *   その写しは F20_COL_OF_SLOT で列の番兵へ行く（分岐なし）。[next_unused, N) は一度も使われていないので 0（鍵の列は
 *   その語を初めて使う alloc が NONE で埋める）。
 *   Scans cover [0, N), N = next_unused rounded up to 64 (live-bitmap word boundaries, N ≤ max(2^K, 64)). The column
 *   sentinel F20_COL_SENTINEL(K) = max(2^K, 64) is outside every scan; a write through an unproven Id goes to the slot
 *   sentinel and its mirror to the column sentinel via F20_COL_OF_SLOT (no branch). [next_unused, N) was never used and
 *   is 0 (key columns: the alloc that first uses a word fills it with NONE). */
#define F20_COL_SENTINEL(K) (((K) >= 6u) ? F20_SLOTS(K) : 64u) /* 列の番兵の添字（走査の外） / column sentinel (outside every scan) */
#define F20_COL_LEN(K) (F20_COL_SENTINEL(K) + 1u)               /* 値の列の要素数 / elements in a value column */
#define F20_COL_WORDS(K) ((F20_COL_SENTINEL(K) >> 6) + 1u)     /* ビット列の語数（番兵の語込み） / words in a bit column (sentinel word included) */
#define F20_COL_SCAN_END(nu) ((((uint32_t)(nu)) + 63u) & ~63u)  /* 走査の終わり N / scan end N */
/* スロットの添字 → 列の添字（スロットの番兵 2^K → 列の番兵。K ≥ 6 なら同じ値で、計算は消える）
 * slot index → column index (the slot sentinel 2^K → the column sentinel; for K ≥ 6 they coincide and this folds away) */
#define F20_COL_OF_SLOT(i, K) ((uint32_t)(i) + ((uint32_t)(i) >> (K)) * (F20_COL_SENTINEL(K) - F20_SLOTS(K)))
/* ビット列の i ビット目を b（0 / 1）にする（分岐なし） / set bit i of a bit column to b (0 / 1), branch-free */
#define F20_BIT_PUT(w, i, b) ((w)[(uint32_t)(i) >> 6] = ((w)[(uint32_t)(i) >> 6] & ~(UINT64_C(1) << ((uint32_t)(i) & 63u))) | ((uint64_t)((b) != 0) << ((uint32_t)(i) & 63u)))

/* 選択の鍵 / pick keys
 *   最小: enc(v)、最大: ~enc(v)（~ は int64 の順序を逆にする）。enc は u64 なら v ^ 2^63（u64 の順序 → i64 の順序）、それ以外の
 *   整数は int64 への拡大。空き・条件を満たさないスロットは F20_KEY_NONE = INT64_MAX。u64 / i64 では NONE と同じ鍵を持つ値が
 *   1 つだけあるが（最小なら最大値、最大なら最小値）、その値は `<` / `>` の比較を決して満たさないので結果は変わらない
 *   （`<=` / `>=` の 64 ビットの選択は鍵にせず、1.0.0 の走査のまま）
 *   min: enc(v), max: ~enc(v) (~ reverses the int64 order); enc is v ^ 2^63 for u64 (u64 order → i64 order) and a
 *   widening to int64 for every other integer. Empty and filtered-out slots hold F20_KEY_NONE = INT64_MAX. For u64 / i64
 *   exactly one value shares NONE's key (the largest for a min, the smallest for a max), and that value never satisfies
 *   `<` / `>`, so the result is the same (64-bit `<=` / `>=` picks are not keyed and keep the 1.0.0 scan) */
#define F20_KEY_NONE INT64_MAX
#define F20_KEY_U64(v) ((int64_t)((uint64_t)(v) ^ UINT64_C(0x8000000000000000)))
#define F20_UNKEY_U64(k) ((uint64_t)(k) ^ UINT64_C(0x8000000000000000))

/* ベクトルの幅（ターゲットの本来の幅。ハードウェアより広いベクトルは GCC が 1 レーンずつのスカラに分解してしまう: SSE2 で
 * 32 B のベクトルにした個数は 7 倍遅かった）。F20_VECTOR_BYTES を定義すれば固定できる。GCC / clang のベクトル拡張だけで書く
 * （組み込み関数のヘッダは使わない。SIMD のないターゲットではふつうのスカラのコードになる）
 * vector width (the target's native width; vectors wider than the hardware make GCC split them into per-lane scalar
 * code: a count with 32-byte vectors was 7x slower on SSE2). Define F20_VECTOR_BYTES to force one. Written with the
 * GCC / clang vector extension only (no intrinsics headers; plain scalar code on targets without SIMD) */
#ifndef F20_VECTOR_BYTES
#if defined(__AVX512F__) && defined(__AVX512VL__)
#define F20_VECTOR_BYTES 64
#elif defined(__AVX2__)
#define F20_VECTOR_BYTES 32
#else
#define F20_VECTOR_BYTES 16 /* SSE2 / SSE4.2 / NEON / SIMD なし / SSE2 / SSE4.2 / NEON / no SIMD */
#endif
#endif
#define F20_L64 (F20_VECTOR_BYTES / 8) /* u64 のレーン数 / u64 lanes */
typedef uint64_t f20_u64v __attribute__((vector_size(F20_VECTOR_BYTES)));
typedef int64_t f20_i64v __attribute__((vector_size(F20_VECTOR_BYTES)));
/* 列からの読み込み: 列は 128 B 境界から始まり、添字はベクトルの倍数なので、いつも整列した読み込み（-mstrict-align でも可）
 * column loads: columns start on 128 B and offsets are vector multiples, so loads are always aligned (fine with -mstrict-align) */
#define F20_LDV(T, p) (*(const T *)(const void *)(p))
#define F20_SPLAT(T, x) ((T){0} + (x))

/* 符号付き 64 ビットのレーンごとの最小（分岐なし）: clang は __builtin_elementwise_min、GCC は比較のマスクで選ぶ
 * （AVX-512VL: VPMINSQ、AVX2: VPCMPGTQ + VPBLENDVB、SSE4.2: PCMPGTQ + PBLENDVB、AArch64: CMGT + BSL）
 * signed 64-bit lane-wise min, branch-free (clang: __builtin_elementwise_min; GCC: a compare-mask select) */
#if defined(__clang__) && defined(__has_builtin)
#if __has_builtin(__builtin_elementwise_min)
#define F20_HAVE_ELEMENTWISE_MIN 1
#endif
#endif
#if defined(F20_HAVE_ELEMENTWISE_MIN)
#define F20_MIN_I64V(a, b) __builtin_elementwise_min((a), (b))
#else
#define F20_MIN_I64V(a, b) ({ const f20_i64v _ma = (a), _mb = (b); const f20_i64v _ml = (f20_i64v)(_mb < _ma); (_ma & ~_ml) | (_mb & _ml); })
#endif
/* レーンの入れ替え（clang と GCC 12 以降は __builtin_shufflevector、それより前の GCC は __builtin_shuffle）
 * lane shuffles (clang and GCC 12+: __builtin_shufflevector; older GCC: __builtin_shuffle) */
#if defined(__has_builtin)
#if __has_builtin(__builtin_shufflevector)
#define F20_SHUF_I64V(x, ...) __builtin_shufflevector((x), (x), __VA_ARGS__)
#endif
#endif
#if !defined(F20_SHUF_I64V) && defined(__GNUC__) && !defined(__clang__)
#define F20_SHUF_I64V(x, ...) __builtin_shuffle((x), (f20_i64v){__VA_ARGS__})
#endif

/* レーンをまとめる（文の式のマクロ: ベクトルを値で関数に渡さないので、AVX なしの x86-64 でも ABI の注意が出ない）
 * lane reductions (statement-expression macros: no vector is passed by value, so no ABI note on x86-64 without AVX) */
#define F20_HSUM_U64V(x) ({ const f20_u64v _hv = (x); uint64_t _hs = 0u; for (int _hk = 0; _hk < F20_L64; _hk++) _hs += _hv[_hk]; _hs; })
#define F20_HOR_U64V(x) ({ const f20_u64v _hv = (x); uint64_t _hs = 0u; for (int _hk = 0; _hk < F20_L64; _hk++) _hs |= _hv[_hk]; _hs; })
/* レーンの最小: シャッフルで半分ずつ重ねる（log2(レーン数) 段、分岐なし）。シャッフルの無いコンパイラはスカラで
 * min over lanes: fold halves with shuffles (log2(lanes) steps, no branch); scalar where no shuffle builtin exists */
#if defined(F20_SHUF_I64V) && F20_L64 == 2
#define F20_HMIN_I64V(x) ({ f20_i64v _h0 = (x); F20_MIN_I64V(_h0, F20_SHUF_I64V(_h0, 1, 0))[0]; })
#elif defined(F20_SHUF_I64V) && F20_L64 == 4
#define F20_HMIN_I64V(x) ({ f20_i64v _h0 = (x); f20_i64v _h1 = F20_MIN_I64V(_h0, F20_SHUF_I64V(_h0, 2, 3, 0, 1)); F20_MIN_I64V(_h1, F20_SHUF_I64V(_h1, 1, 0, 3, 2))[0]; })
#elif defined(F20_SHUF_I64V) && F20_L64 == 8
#define F20_HMIN_I64V(x)                                                                         \
    ({                                                                                           \
        f20_i64v _h0 = (x);                                                                      \
        f20_i64v _h1 = F20_MIN_I64V(_h0, F20_SHUF_I64V(_h0, 4, 5, 6, 7, 0, 1, 2, 3));            \
        f20_i64v _h2 = F20_MIN_I64V(_h1, F20_SHUF_I64V(_h1, 2, 3, 0, 1, 6, 7, 4, 5));            \
        F20_MIN_I64V(_h2, F20_SHUF_I64V(_h2, 1, 0, 3, 2, 5, 4, 7, 6))[0];                        \
    })
#else
#define F20_HMIN_I64V(x) ({ const f20_i64v _hv = (x); int64_t _hm = _hv[0]; for (int _hk = 1; _hk < F20_L64; _hk++) _hm = _hv[_hk] < _hm ? _hv[_hk] : _hm; _hm; })
#endif

/* 64 ビットの符号付き比較がベクトル 1 命令のターゲット（x86-64 SSE4.2 以上: PCMPGTQ、AArch64: CMGT）。それ以外（素の SSE2、
 * ARMv7、SIMD なし）では鍵の選択をスカラの 4 本の CMOV / CSEL の鎖で行う（SSE2 で 64 ビットの比較を組み立てるより速い）
 * targets with a one-instruction vector 64-bit signed compare (x86-64 SSE4.2+: PCMPGTQ, AArch64: CMGT); elsewhere
 * (plain SSE2, ARMv7, no SIMD) keyed picks run four scalar CMOV / CSEL chains (faster than emulating the compare) */
#if !defined(F20_VEC_I64_CMP)
#if defined(__SSE4_2__) || defined(__AVX__) || defined(__aarch64__)
#define F20_VEC_I64_CMP 1
#else
#define F20_VEC_I64_CMP 0
#endif
#endif

/* 読み込んだベクトルをレジスタに置く（GCC の x86 は 1 回の読み込みを 2 つの命令のメモリオペランドに畳み、同じ 16 B を
 * 2 回読む: SSE2 の走査がそれで読み込みに詰まった。中身のない asm で値をレジスタに固定する。ほかのコンパイラでは何もしない）
 * keep a loaded vector in a register (GCC on x86 folds one load into memory operands of two instructions and reads the
 * same 16 bytes twice; the SSE2 scans were load-bound because of it; an empty asm pins the value in a register; a
 * no-op for every other compiler) */
#if defined(__GNUC__) && !defined(__clang__) && defined(__SSE2__) && !defined(F20_NO_KEEP)
#define F20_KEEP(v) __asm__("" : "+x"(v))
#else
#define F20_KEEP(v) ((void)0)
#endif

/* 列の走査のソフトウェアのプリフェッチ: F20_PREFETCH_DIST（バイト）を定義すると、64 スロットの塊ごとにその先の 8 ラインを
 * 読む合図を出す（既定では出さない: ハードウェアのプリフェッチを持つ CPU では効果がない。コンパイラは --nontemporal=on の
 * 大きな表で定義する。ハードウェアのプリフェッチの弱いボードでは -DF20_PREFETCH_DIST=1024 などで足せる）
 * software prefetch for column scans: with F20_PREFETCH_DIST (bytes) defined, each 64-slot block hints the 8 lines that
 * far ahead (off by default: no gain on CPUs with hardware prefetchers; the compiler defines it for large tables under
 * --nontemporal=on; boards with weak hardware prefetch can add -DF20_PREFETCH_DIST=1024 or similar) */
#if defined(F20_PREFETCH_DIST) && F20_PREFETCH_DIST > 0
#define F20_PF_BLOCK(p, esz)                                                                                       \
    do {                                                                                                            \
        for (uint32_t pf_ = 0u; pf_ < 64u * (esz); pf_ += 64u) __builtin_prefetch((const char *)(p) + F20_PREFETCH_DIST + pf_, 0, 0); \
    } while (0)
#else
#define F20_PF_BLOCK(p, esz) ((void)0)
#endif

/* ビット長（0 なら 1: 上界としては安全側） / bit length (1 for 0, safe as an upper bound) */
static inline uint32_t f20_bitlen64(uint64_t x) { return 64u - (uint32_t)__builtin_clzll(x | 1u); }

/* 正確な u64 の総和（上位・下位 32 ビットを別々に 64 ビットで足す。2^32 個までなら回り込まない）。Σ ≥ 2^64 なら
 * F20_FAULT_ARITH（符号なしなので、逐次の検査付き加算と同じ判定）。f20_col_sum_u64 の OR の上界が足りないときだけ呼ぶ
 * the exact u64 sum (low and high 32-bit halves summed separately in 64 bits, exact up to 2^32 terms); Σ ≥ 2^64 sets
 * F20_FAULT_ARITH (for unsigned values the verdict of sequential checked additions). Called only when the OR bound of
 * f20_col_sum_u64 is not enough */
static FASCON_MAYBE_UNUSED __attribute__((noinline, cold)) uint64_t f20_col_sum_u64_exact(const uint64_t *F20_RESTRICT c, uint32_t n, uint32_t *F20_RESTRICT flt) {
    f20_u64v lo = {0}, hi = {0};
    const f20_u64v m32 = F20_SPLAT(f20_u64v, 0xFFFFFFFFu);
    for (uint32_t i = 0u; i < n; i += F20_L64) {
        const f20_u64v a = F20_LDV(f20_u64v, c + i);
        lo += a & m32;
        hi += a >> 32;
    }
    const uint64_t l = F20_HSUM_U64V(lo), h = F20_HSUM_U64V(hi) + (l >> 32);
    *flt |= (uint32_t)((h >> 32) != 0u) << 1; /* F20_FAULT_ARITH */
    return (h << 32) | (l & 0xFFFFFFFFu);
}

/* u64 の集計の列の総和（総和・条件付き総和の本体）
 *   [0, n) を 64 スロットの塊ごとに足す（n は 64 の倍数。死んだスロットと条件を満たさないスロットは列で 0）。塊の生存ビット
 *   マップの語が 0 なら塊ごと飛ばす（予測の当たる分岐 1 本 / 64 スロット。まばらな表で読む量が減る）。塊の中は 4 本の独立な
 *   ベクトル加算と OR（x86-64: VPADDQ / VPOR、AArch64: ADD.2D / ORR）。あふれの検査は走査から外す: OR のビット長 b と n の
 *   ビット長 q について b + q ≤ 64 なら、どの部分和も 2^64 未満なので素のベクトル加算がそのまま正確な和。そうでないときだけ
 *   正確な走査をもう 1 回する（めったに起きない、予測の当たる分岐 1 本）
 * sum of a u64 aggregate column (the body of sums and filtered sums)
 *   adds [0, n) per 64-slot block (n a multiple of 64; dead and filtered-out slots are 0 in the column); a block whose
 *   live-bitmap word is 0 is skipped (one predicted branch per 64 slots, so sparse tables read less). Inside a block:
 *   four independent vector adds and ORs. Overflow checks leave the scan: with b = bit length of the OR and q = bit
 *   length of n, b + q ≤ 64 means no partial sum reaches 2^64, so the plain vector sum is exact; only otherwise is the
 *   exact pass run (rare; one predicted branch) */
static inline FASCON_MAYBE_UNUSED uint64_t f20_col_sum_u64(const uint64_t *F20_RESTRICT c, const uint64_t *F20_RESTRICT live, uint32_t n, uint32_t *F20_RESTRICT flt) {
    f20_u64v s0 = {0}, s1 = {0}, s2 = {0}, s3 = {0}, o0 = {0}, o1 = {0};
    for (uint32_t base = 0u; base < n; base += 64u) {
        if (live[base >> 6] == 0u) continue; /* 64 スロットすべて死んでいる / all 64 slots dead */
        F20_PF_BLOCK(c + base, 8u);
        for (const uint64_t *q = c + base, *const e = q + 64; q < e; q += 4u * F20_L64) {
            f20_u64v a = F20_LDV(f20_u64v, q), b = F20_LDV(f20_u64v, q + F20_L64);
            f20_u64v d = F20_LDV(f20_u64v, q + 2u * F20_L64), x = F20_LDV(f20_u64v, q + 3u * F20_L64);
            F20_KEEP(a);
            F20_KEEP(b);
            F20_KEEP(d);
            F20_KEEP(x);
            s0 += a;
            s1 += b;
            s2 += d;
            s3 += x;
            o0 |= a | b;
            o1 |= d | x;
        }
    }
    uint64_t sum = F20_HSUM_U64V((s0 + s1) + (s2 + s3));
    if (FASCON_UNLIKELY(f20_bitlen64(F20_HOR_U64V(o0 | o1)) + f20_bitlen64(n) > 64u)) sum = f20_col_sum_u64_exact(c, n, flt);
    return sum;
}

/* 32 ビットの集計の列の総和（32 ビット以下の値。u8 / u16 も 4 B の列に置く）。u64 のレーンとして読み、下位・上位 32 ビットを
 * マスクとシフトで分けて足す（x86-64: VPAND / VPSRLQ / VPADDQ、AArch64: AND / USHR / ADD）。2^24 スロット × 2^32 未満なので
 * 64 ビットのレーンは回り込まない（あふれの検査は要らない）
 * sum of a 32-bit aggregate column (values of 32 bits or fewer; u8 / u16 also get 4-byte columns). Read as u64 lanes
 * and split into low / high 32 bits with a mask and a shift; 2^24 slots × values below 2^32 cannot wrap a 64-bit lane
 * (no overflow check needed) */
static inline FASCON_MAYBE_UNUSED uint64_t f20_col_sum_u32(const uint32_t *F20_RESTRICT c, const uint64_t *F20_RESTRICT live, uint32_t n) {
    f20_u64v s0 = {0}, s1 = {0}, s2 = {0}, s3 = {0};
    const f20_u64v m32 = F20_SPLAT(f20_u64v, 0xFFFFFFFFu);
    for (uint32_t base = 0u; base < n; base += 64u) {
        if (live[base >> 6] == 0u) continue;
        F20_PF_BLOCK(c + base, 4u);
        for (const uint32_t *q = c + base, *const e = q + 64; q < e; q += 4u * F20_L64) {
            f20_u64v a = F20_LDV(f20_u64v, q), b = F20_LDV(f20_u64v, q + 2u * F20_L64);
            F20_KEEP(a);
            F20_KEEP(b);
            s0 += a & m32;
            s1 += a >> 32;
            s2 += b & m32;
            s3 += b >> 32;
        }
    }
    return F20_HSUM_U64V((s0 + s1) + (s2 + s3));
}

/* ビット列の 1 の数（条件付きの個数の本体: 64 スロットに popcount 1 回。x86-64: POPCNT、AArch64: CNT + ADDV）
 * the number of set bits in a bit column (the body of filtered counts: one popcount per 64 slots) */
static inline FASCON_MAYBE_UNUSED uint64_t f20_col_count_bits(const uint64_t *F20_RESTRICT b, uint32_t nw) {
    uint64_t c0 = 0u, c1 = 0u;
    uint32_t w = 0u;
    for (; w + 2u <= nw; w += 2u) {
        c0 += (uint64_t)__builtin_popcountll(b[w]);
        c1 += (uint64_t)__builtin_popcountll(b[w + 1u]);
    }
    if (w < nw) c0 += (uint64_t)__builtin_popcountll(b[w]);
    return c0 + c1;
}

/* 鍵の列の最小（最小 / 最大の選択の本体）
 *   段 1: 64 スロットの塊ごとに鍵の最小（ベクトルの符号付き最小、4 本の独立な鎖）。塊の最小が今の最小より小さければ（last = 0:
 *         `<` なので同じ値なら先の塊が勝つ。last = 1: `<=` で後の塊が勝つ）、最小と塊を CMOV / CSEL で選び直す。
 *   段 2: 選んだ塊の 64 個から「鍵 == 最小」のビットを作り、最初の 1（TZCNT / RBIT+CLZ）か最後の 1（LZCNT / CLZ）を引く。
 *   逐次のループと同じ結果（`<` なら最小の値のうち添字が最も小さいもの、`<=` なら最も大きいもの）。*best は入力が今の最小
 *   （ループの前の値の鍵）で、見つかったときだけ新しい最小になる。戻り値は見つかった添字（なければ 0xFFFFFFFF）
 * minimum of a key column (the body of min / max picks)
 *   stage 1: per 64-slot block, the vector signed minimum of the keys (four independent chains); when the block minimum
 *            beats the running one (last = 0: `<`, the earlier block wins a tie; last = 1: `<=`, the later one wins), both
 *            the minimum and the block are re-selected with CMOV / CSEL.
 *   stage 2: in the chosen block, one bit per "key == minimum", then the first set bit (TZCNT / RBIT+CLZ) or the last
 *            one (LZCNT / CLZ). Same result as the sequential loop (`<`: the smallest index among the minima, `<=`: the
 *            largest). *best comes in as the current minimum (the key of the value before the loop) and changes only
 *            when something is found. Returns the index found (0xFFFFFFFF if none) */
static inline FASCON_MAYBE_UNUSED uint32_t f20_col_pick_key(const int64_t *F20_RESTRICT key, const uint64_t *F20_RESTRICT live, uint32_t n, int64_t *F20_RESTRICT best,
                                                            const int last) {
    int64_t b = *best;
    uint32_t blk = 0xFFFFFFFFu;
    for (uint32_t base = 0u; base < n; base += 64u) {
        if (live[base >> 6] == 0u) continue; /* 64 スロットすべて死んでいる（鍵はすべて NONE） / all dead (keys all NONE) */
#if F20_VEC_I64_CMP
        f20_i64v m0 = F20_LDV(f20_i64v, key + base), m1 = F20_LDV(f20_i64v, key + base + F20_L64);
        f20_i64v m2 = F20_LDV(f20_i64v, key + base + 2u * F20_L64), m3 = F20_LDV(f20_i64v, key + base + 3u * F20_L64);
        for (uint32_t j = base + 4u * F20_L64; j < base + 64u; j += 4u * F20_L64) {
            f20_i64v k0 = F20_LDV(f20_i64v, key + j), k1 = F20_LDV(f20_i64v, key + j + F20_L64);
            f20_i64v k2 = F20_LDV(f20_i64v, key + j + 2u * F20_L64), k3 = F20_LDV(f20_i64v, key + j + 3u * F20_L64);
            F20_KEEP(k0);
            F20_KEEP(k1);
            F20_KEEP(k2);
            F20_KEEP(k3);
            m0 = F20_MIN_I64V(m0, k0);
            m1 = F20_MIN_I64V(m1, k1);
            m2 = F20_MIN_I64V(m2, k2);
            m3 = F20_MIN_I64V(m3, k3);
        }
        const int64_t bm = F20_HMIN_I64V(F20_MIN_I64V(F20_MIN_I64V(m0, m1), F20_MIN_I64V(m2, m3)));
#else
        /* スカラ: 4 本の独立な最小の鎖。ポインタで進める（32 ビットの添字だと GCC がスロットごとに添字を計算し直す）
         * scalar: four independent chains, advanced by pointer (with 32-bit indices GCC recomputes the index per slot) */
        const int64_t *q = key + base;
        int64_t m0 = q[0], m1 = q[1], m2 = q[2], m3 = q[3];
        for (const int64_t *const e = q + 64; (q += 4) < e;) {
            m0 = q[0] < m0 ? q[0] : m0;
            m1 = q[1] < m1 ? q[1] : m1;
            m2 = q[2] < m2 ? q[2] : m2;
            m3 = q[3] < m3 ? q[3] : m3;
        }
        m0 = m1 < m0 ? m1 : m0;
        m2 = m3 < m2 ? m3 : m2;
        const int64_t bm = m2 < m0 ? m2 : m0;
#endif
        const uint32_t k = last ? (uint32_t)(bm <= b) : (uint32_t)(bm < b); /* 塊が勝ったか / did this block win */
        b = (int64_t)F20_SEL64(k, (uint64_t)bm, (uint64_t)b);              /* CMOV / CSEL */
        blk = F20_SEL32(k, base, blk);                                      /* CMOV / CSEL */
    }
    if (blk == 0xFFFFFFFFu) return 0xFFFFFFFFu;
    uint64_t hit = 0u;
    for (uint32_t j = 0u; j < 64u; j++) hit |= (uint64_t)(key[blk + j] == b) << j;
    *best = b;
    return blk + (last ? 63u - (uint32_t)__builtin_clzll(hit) : f20_ctz64(hit));
}

/* 鍵の列の 64 スロットの語を F20_KEY_NONE で埋める（新しい語を初めて使う alloc で 1 回: 64 回の確保に 1 回）
 * fill one 64-slot word of a key column with F20_KEY_NONE (once, by the alloc that first uses the word: 1 in 64 allocs) */
static inline FASCON_MAYBE_UNUSED void f20_col_key_fresh(int64_t *F20_RESTRICT key, uint32_t base) {
    for (uint32_t j = 0u; j < 64u; j++) key[base + j] = F20_KEY_NONE;
}

/* ───── 浮動小数点の選択の鍵（#[fast_math] の最小 / 最大） / float pick keys (#[fast_math] min / max) ─────
 *   F20_KEY_F32 / F20_KEY_F64: IEEE-754 の順序を保つ全単射。符号ビット 0（正）なら符号ビットだけ反転（bits ^ 0x8000_0000）、
 *   1（負）なら全ビット反転（~bits）。こうして得た符号なしの大小が浮動小数点の大小に一致するので、最上位ビットを戻して
 *   符号付き（int32_t / int64_t）の大小にする。f20_fkey_f32 / _f64 は鍵の列に入れる値: NaN は F20_KEY_NONE（死んだスロットと
 *   同じで選ばれない）、-0.0 は +0.0 にそろえてから変換（-0.0 < +0.0 は偽なので同じ鍵）、最大なら ~鍵（列はいつも最小を
 *   走査する）。f32 の鍵は 32 ビットの順序の鍵を整数の鍵の列と同じ int64 の列へ広げて置く（走査は f20_col_pick_key 1 つ）
 *   F20_KEY_F32 / F20_KEY_F64: an IEEE-754 order-preserving bijection. Sign bit 0 (positive) flips only the sign bit
 *   (bits ^ 0x8000_0000), sign bit 1 (negative) flips every bit (~bits); the unsigned order of the result is the float
 *   order, and flipping the top bit back gives the signed (int32_t / int64_t) order. f20_fkey_f32 / _f64 give the value
 *   stored in a key column: NaN is F20_KEY_NONE (like a dead slot, never picked), -0.0 is folded into +0.0 first
 *   (-0.0 < +0.0 is false, so they share a key), and a max stores ~key (columns are always scanned for the minimum). An
 *   f32 key is the 32-bit order key widened into the same int64 key column integers use (one scan: f20_col_pick_key) */
#define F20_KEY32_NONE INT32_MAX
#define F20_KEY_F32(b) ((int32_t)(((((uint32_t)(b)) & 0x80000000u) ? ~(uint32_t)(b) : ((uint32_t)(b) ^ 0x80000000u)) ^ 0x80000000u))
#define F20_KEY_F64(b) ((int64_t)(((((uint64_t)(b)) >> 63) ? ~(uint64_t)(b) : ((uint64_t)(b) ^ UINT64_C(0x8000000000000000))) ^ UINT64_C(0x8000000000000000)))
static inline FASCON_MAYBE_UNUSED int64_t f20_fkey_f32(float v, int max) {
    uint32_t b = f20_f32_to_bits(v);
    if ((b & 0x7FFFFFFFu) > 0x7F800000u) return F20_KEY_NONE; /* NaN */
    if (b == 0x80000000u) b = 0u;                              /* -0.0 → +0.0 */
    const int64_t k = (int64_t)F20_KEY_F32(b);
    return max ? ~k : k;
}
static inline FASCON_MAYBE_UNUSED int64_t f20_fkey_f64(double v, int max) {
    uint64_t b = f20_f64_to_bits(v);
    if ((b & UINT64_C(0x7FFFFFFFFFFFFFFF)) > UINT64_C(0x7FF0000000000000)) return F20_KEY_NONE; /* NaN */
    if (b == UINT64_C(0x8000000000000000)) b = 0u;                                              /* -0.0 → +0.0 */
    const int64_t k = F20_KEY_F64(b);
    return max ? ~k : k;
}


/* ───── 完全列化（True SoA）: 列が唯一の正本 / True SoA: the column is the only copy ─────
 *   フィールド f を要素のスロットから外し、列 ts_f[] だけに置く（書き込みは列への 1 回のストアだけ）。死んだスロットと一度も
 *   使われていないスロットの値は 0、古い Id の書き込みは列の番兵 F20_COL_SENTINEL(K) へ逸れる（どの走査も読まない）。
 *   条件付きの集計は、条件のビット列（生存 && 条件）か生存ビットマップを走査の時点のマスクにする（写しの列は作らない）
 *   the field f leaves the element slot and lives only in the column ts_f[] (a write is one store to the column). Dead
 *   and never-used slots hold 0; a write through a stale Id goes to the column sentinel F20_COL_SENTINEL(K), which no
 *   scan reads. Filtered reductions use the filter bit column (live && filter) or the live bitmap as a mask at scan
 *   time (no mirror column is kept) */
#if F20_L64 == 2
#define F20_LANE_SH64 {0u, 1u}
#elif F20_L64 == 4
#define F20_LANE_SH64 {0u, 1u, 2u, 3u}
#else
#define F20_LANE_SH64 {0u, 1u, 2u, 3u, 4u, 5u, 6u, 7u}
#endif
/* レーン l が w >> l を持つベクトル（塊ごとに 1 回。SSE2 は 2 回のシフト + 並べ替え、AVX2 以上は VPSRLVQ 1 命令）
 * a vector whose lane l holds w >> l (once per block; SSE2: two shifts + a shuffle, AVX2 and up: one VPSRLVQ) */
#define F20_LANE_BITS(w) (F20_SPLAT(f20_u64v, (w)) >> (f20_u64v)F20_LANE_SH64)
/* 下位ビットが 1 のレーンを全ビット 1 に（AND と 0 からの引き算: どの SIMD にもある 2 命令） / lanes whose low bit is set become
 * all ones (an AND and a subtraction from 0: two instructions every SIMD has) */
#define F20_LANE_MASK(wv) ((f20_u64v){0} - ((wv) & F20_SPLAT(f20_u64v, 1u)))

/* u64 のレーンに 2 つ並んだ 32 ビットの値: 下位 32 ビットはリトルエンディアンでは偶数番（2l）、ビッグエンディアンでは奇数番
 * two 32-bit values in a u64 lane: the low 32 bits hold the even one (2l) on little-endian, the odd one on big-endian */
#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
#define F20_W_LO(we, wo) (wo)
#define F20_W_HI(we, wo) (we)
#else
#define F20_W_LO(we, wo) (we)
#define F20_W_HI(we, wo) (wo)
#endif

/* ───── 浮動小数点の列の総和（#[fast_math]）/ float column sums (#[fast_math]) ─────
 *   4 本（マスク付きは 2 本）の独立なベクトルの累計: レーンごと・累計ごとに足してから最後にまとめるので、書いた順とは結合の
 *   順序が違う（#[fast_math] がそれを許す。属性のないループはこの関数を使わない）。死んだスロット・一度も使われていない
 *   スロットは 0.0 なので生存マスクは要らない（全部死んだ 64 スロットの塊は飛ばす）。マスク付きは条件のビット列の 1 の
 *   レーンだけを残す（AND）
 *   four (two when masked) independent vector accumulators: lanes and accumulators add separately and are combined at the
 *   end, so the association order differs from the written one (#[fast_math] allows it; loops without the attribute never
 *   call these). Dead and never-used slots are 0.0, so no live mask is needed (all-dead 64-slot blocks are skipped); the
 *   masked forms keep only the lanes whose filter bit is set (an AND) */
typedef float f20_f32v __attribute__((vector_size(F20_VECTOR_BYTES)));
typedef double f20_f64v __attribute__((vector_size(F20_VECTOR_BYTES)));
#define F20_L32F (F20_VECTOR_BYTES / 4) /* float のレーン数 / float lanes */
#define F20_HSUM_F32V(x) ({ const f20_f32v _hv = (x); float _hs = 0.0f; for (int _hk = 0; _hk < F20_L32F; _hk++) _hs += _hv[_hk]; _hs; })
#define F20_HSUM_F64V(x) ({ const f20_f64v _hv = (x); double _hs = 0.0; for (int _hk = 0; _hk < F20_L64; _hk++) _hs += _hv[_hk]; _hs; })
#define F20_LANE_MASK_DOWN(wl) ((f20_u64v){0} - ((wl) & F20_SPLAT(f20_u64v, 1u)))

static inline FASCON_MAYBE_UNUSED float f20_col_sum_f32(const float *F20_RESTRICT c, const uint64_t *F20_RESTRICT live, uint32_t n) {
    f20_f32v s0 = {0}, s1 = {0}, s2 = {0}, s3 = {0};
    for (uint32_t base = 0u; base < n; base += 64u) {
        if (live[base >> 6] == 0u) continue;
        F20_PF_BLOCK(c + base, 4u);
        for (const float *q = c + base, *const e = q + 64; q < e; q += 4u * F20_L32F) {
            f20_f32v a = F20_LDV(f20_f32v, q), b = F20_LDV(f20_f32v, q + F20_L32F);
            f20_f32v d = F20_LDV(f20_f32v, q + 2u * F20_L32F), g = F20_LDV(f20_f32v, q + 3u * F20_L32F);
            s0 += a;
            s1 += b;
            s2 += d;
            s3 += g;
        }
    }
    return F20_HSUM_F32V((s0 + s1) + (s2 + s3));
}

static inline FASCON_MAYBE_UNUSED double f20_col_sum_f64(const double *F20_RESTRICT c, const uint64_t *F20_RESTRICT live, uint32_t n) {
    f20_f64v s0 = {0}, s1 = {0}, s2 = {0}, s3 = {0};
    for (uint32_t base = 0u; base < n; base += 64u) {
        if (live[base >> 6] == 0u) continue;
        F20_PF_BLOCK(c + base, 8u);
        for (const double *q = c + base, *const e = q + 64; q < e; q += 4u * F20_L64) {
            f20_f64v a = F20_LDV(f20_f64v, q), b = F20_LDV(f20_f64v, q + F20_L64);
            f20_f64v d = F20_LDV(f20_f64v, q + 2u * F20_L64), g = F20_LDV(f20_f64v, q + 3u * F20_L64);
            s0 += a;
            s1 += b;
            s2 += d;
            s3 += g;
        }
    }
    return F20_HSUM_F64V((s0 + s1) + (s2 + s3));
}

/* f32 のマスク付き: u64 のレーン 1 本に float が 2 つ（偶数番 2l と奇数番 2l+1。f20_col_sum_u32_m と同じ分け方）。マスクの
 * 下位・上位 32 ビットをそれぞれのビットから作って AND する / masked f32: two floats per u64 lane (even 2l and odd 2l+1, split
 * as in f20_col_sum_u32_m); the low and high 32 bits of the lane mask come from their own bits, then an AND */
static inline FASCON_MAYBE_UNUSED float f20_col_sum_f32_m(const float *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n) {
    f20_f32v s0 = {0}, s1 = {0};
    const f20_u64v lo32 = F20_SPLAT(f20_u64v, 0xFFFFFFFFu), two = F20_SPLAT(f20_u64v, 2u);
    for (uint32_t base = 0u; base < n; base += 64u) {
        const uint64_t w = m[base >> 6];
        if (w == 0u) continue;
        F20_PF_BLOCK(c + base, 4u);
        f20_u64v we = F20_SPLAT(f20_u64v, w) >> ((f20_u64v)F20_LANE_SH64 * two); /* レーン l: w >> 2l / lane l: w >> 2l */
        f20_u64v wo = we >> 1u;                                                   /* レーン l: w >> (2l+1) */
        for (const float *q = c + base, *const e = q + 64; q < e; q += 4u * F20_L64) {
            const f20_u64v m0 = (F20_LANE_MASK_DOWN(F20_W_LO(we, wo)) & lo32) | (F20_LANE_MASK_DOWN(F20_W_HI(we, wo)) & ~lo32);
            we >>= 2u * F20_L64;
            wo >>= 2u * F20_L64;
            const f20_u64v m1 = (F20_LANE_MASK_DOWN(F20_W_LO(we, wo)) & lo32) | (F20_LANE_MASK_DOWN(F20_W_HI(we, wo)) & ~lo32);
            we >>= 2u * F20_L64;
            wo >>= 2u * F20_L64;
            const f20_u64v a = F20_LDV(f20_u64v, q), b = F20_LDV(f20_u64v, q + 2u * F20_L64);
            s0 += (f20_f32v)(a & m0);
            s1 += (f20_f32v)(b & m1);
        }
    }
    return F20_HSUM_F32V(s0 + s1);
}

static inline FASCON_MAYBE_UNUSED double f20_col_sum_f64_m(const double *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n) {
    f20_f64v s0 = {0}, s1 = {0};
    for (uint32_t base = 0u; base < n; base += 64u) {
        const uint64_t w = m[base >> 6];
        if (w == 0u) continue;
        F20_PF_BLOCK(c + base, 8u);
        f20_u64v wl = F20_LANE_BITS(w); /* レーン l: w >> l / lane l: w >> l */
        for (const double *q = c + base, *const e = q + 64; q < e; q += 2u * F20_L64) {
            const f20_u64v m0 = F20_LANE_MASK_DOWN(wl);
            wl >>= F20_L64;
            const f20_u64v m1 = F20_LANE_MASK_DOWN(wl);
            wl >>= F20_L64;
            const f20_u64v a = F20_LDV(f20_u64v, q), b = F20_LDV(f20_u64v, q + F20_L64);
            s0 += (f20_f64v)(a & m0);
            s1 += (f20_f64v)(b & m1);
        }
    }
    return F20_HSUM_F64V(s0 + s1);
}


/* レーンごとの可変シフトのない 2 レーンのターゲット（x86 の SSE2 / SSE4.2）: マスクの 4 ビットで 16 通りの「2 ベクトル分の
 * レーンのマスク」の表（512 B）を引く。マスクを作る演算が要らず、AND のメモリオペランドになる（AArch64 は USHL があるので
 * 表を使わない）
 * 2-lane targets without a per-lane variable shift (x86 SSE2 / SSE4.2): 4 mask bits index a 16-entry table of lane
 * masks for two vectors (512 B), so no instructions build the masks; they become memory operands of the ANDs (AArch64
 * has USHL and does not use the table) */
#if F20_L64 == 2 && (defined(__x86_64__) || defined(__i386__)) && !defined(F20_NO_MASK_TABLE)
#define F20_MASK_TABLE 1
#define F20_M2(b) ((b) ? ~UINT64_C(0) : UINT64_C(0))
#define F20_M4(k) {{F20_M2((k) & 1), F20_M2((k) & 2)}, {F20_M2((k) & 4), F20_M2((k) & 8)}}
static const f20_u64v f20_mask_tab[16][2] __attribute__((aligned(64), unused)) = {F20_M4(0), F20_M4(1), F20_M4(2), F20_M4(3), F20_M4(4), F20_M4(5), F20_M4(6), F20_M4(7),
                                                                                F20_M4(8), F20_M4(9), F20_M4(10), F20_M4(11), F20_M4(12), F20_M4(13), F20_M4(14), F20_M4(15)};
/* 4 つの 32 ビットの値（1 ベクトル）のマスク: 4 ビットで 16 通り（256 B） / masks for four 32-bit values (one vector):
 * 16 entries for 4 bits (256 B) */
#define F20_M32(b) ((b) ? UINT64_C(0xFFFFFFFF) : UINT64_C(0))
#define F20_N4(k) {F20_M32((k) & 1) | (F20_M32((k) & 2) << 32), F20_M32((k) & 4) | (F20_M32((k) & 8) << 32)}
static const f20_u64v f20_mask_tab32[16] __attribute__((aligned(64), unused)) = {F20_N4(0), F20_N4(1), F20_N4(2), F20_N4(3), F20_N4(4), F20_N4(5), F20_N4(6), F20_N4(7),
                                                                               F20_N4(8), F20_N4(9), F20_N4(10), F20_N4(11), F20_N4(12), F20_N4(13), F20_N4(14), F20_N4(15)};
#endif

/* 正確な u64 のマスク付き総和（f20_col_sum_u64_exact のマスク版。OR の上界が足りないときだけ） / the exact masked u64 sum (the
 * masked f20_col_sum_u64_exact; only when the OR bound is not enough) */
static FASCON_MAYBE_UNUSED __attribute__((noinline, cold)) uint64_t f20_col_sum_u64_m_exact(const uint64_t *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n, uint32_t *F20_RESTRICT flt) {
    uint64_t lo = 0u, hi = 0u;
    for (uint32_t base = 0u; base < n; base += 64u)
        for (uint64_t w = m[base >> 6]; w != 0u; w = f20_blsr64(w)) {
            const uint64_t a = c[base + f20_ctz64(w)];
            lo += a & 0xFFFFFFFFu;
            hi += a >> 32;
        }
    hi += lo >> 32;
    *flt |= (uint32_t)((hi >> 32) != 0u) << 1; /* F20_FAULT_ARITH */
    return (hi << 32) | (lo & 0xFFFFFFFFu);
}

/* True SoA の u64 の列の、マスク m（条件のビット列か生存ビットマップ）が立つスロットの総和
 *   64 スロットの塊ごと: マスクの語が 0 なら飛ばす（予測の当たる分岐 1 本）。塊の中は分岐なしで、レーンごとのマスク（AND と
 *   0 からの引き算）を値に AND して 2 本の独立なベクトルに足す。あふれは f20_col_sum_u64 と同じ OR の上界で外に出す
 * the sum of a True-SoA u64 column over the slots whose bit is set in m (a filter bit column or the live bitmap)
 *   per 64-slot block: a zero mask word skips the block (one predicted branch); inside, no branch: per-lane masks (an
 *   AND and a subtraction from 0) are ANDed into the values, which go to two independent vector sums. Overflow leaves
 *   the scan through the same OR bound as f20_col_sum_u64 */
static inline FASCON_MAYBE_UNUSED uint64_t f20_col_sum_u64_m(const uint64_t *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n, uint32_t *F20_RESTRICT flt) {
    f20_u64v s0 = {0}, s1 = {0}, o0 = {0}, o1 = {0};
    for (uint32_t base = 0u; base < n; base += 64u) {
        const uint64_t w = m[base >> 6];
        if (w == 0u) continue; /* 64 スロットどれも数えない / no slot of the 64 counts */
        F20_PF_BLOCK(c + base, 8u);
#if defined(F20_MASK_TABLE)
        uint64_t r = w;
        for (const uint64_t *q = c + base, *const e = q + 64; q < e; q += 8u, r >>= 8) {
            const f20_u64v *t = f20_mask_tab[r & 15u], *u = f20_mask_tab[(r >> 4) & 15u];
            const f20_u64v a = F20_LDV(f20_u64v, q) & t[0], b = F20_LDV(f20_u64v, q + 2) & t[1];
            const f20_u64v d = F20_LDV(f20_u64v, q + 4) & u[0], x = F20_LDV(f20_u64v, q + 6) & u[1];
            s0 += a + b;
            s1 += d + x;
            o0 |= a | b;
            o1 |= d | x;
        }
#else
        f20_u64v wv = F20_LANE_BITS(w);
        for (const uint64_t *q = c + base, *const e = q + 64; q < e; q += 2u * F20_L64) {
            f20_u64v a = F20_LDV(f20_u64v, q), b = F20_LDV(f20_u64v, q + F20_L64);
            F20_KEEP(a);
            F20_KEEP(b);
            a &= F20_LANE_MASK(wv);
            b &= F20_LANE_MASK(wv >> F20_L64);
            wv >>= 2u * F20_L64;
            s0 += a;
            s1 += b;
            o0 |= a;
            o1 |= b;
        }
#endif
    }
    uint64_t sum = F20_HSUM_U64V(s0 + s1);
    if (FASCON_UNLIKELY(f20_bitlen64(F20_HOR_U64V(o0 | o1)) + f20_bitlen64(n) > 64u)) sum = f20_col_sum_u64_m_exact(c, m, n, flt);
    return sum;
}

/* True SoA の 32 ビット以下の列（u32 / i32 は 4 B の列）のマスク付き総和。u64 のレーンとして読み、偶数番と奇数番の値を
 * マスクとシフトで分け、それぞれのマスク（ビット 2l と 2l+1）を AND する。2^24 スロット × 2^32 未満は 64 ビットに収まる
 * the masked sum of a True-SoA column of 32 bits (u32 / i32 get 4-byte columns): read as u64 lanes, the even and odd
 * values are split with a mask and a shift and each gets its own lane mask (bits 2l and 2l+1); 2^24 slots × values
 * below 2^32 fit in 64 bits */
static inline FASCON_MAYBE_UNUSED uint64_t f20_col_sum_u32_m(const uint32_t *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n) {
    f20_u64v s0 = {0}, s1 = {0};
    const f20_u64v m32 = F20_SPLAT(f20_u64v, 0xFFFFFFFFu), two = F20_SPLAT(f20_u64v, 2u);
    for (uint32_t base = 0u; base < n; base += 64u) {
        const uint64_t w = m[base >> 6];
        if (w == 0u) continue;
        F20_PF_BLOCK(c + base, 4u);
#if defined(F20_MASK_TABLE)
        (void)two;
        uint64_t r = w;
        for (const uint32_t *q = c + base, *const e = q + 64; q < e; q += 8u, r >>= 8) {
            const f20_u64v a = F20_LDV(f20_u64v, q) & f20_mask_tab32[r & 15u], b = F20_LDV(f20_u64v, q + 4) & f20_mask_tab32[(r >> 4) & 15u];
            s0 += (a & m32) + (b & m32);
            s1 += (a >> 32) + (b >> 32);
        }
#else
        f20_u64v we = F20_SPLAT(f20_u64v, w) >> ((f20_u64v)F20_LANE_SH64 * two); /* レーン l: w >> 2l / lane l: w >> 2l */
        f20_u64v wo = we >> 1u;                                                   /* レーン l: w >> (2l+1) */
        for (const uint32_t *q = c + base, *const e = q + 64; q < e; q += 2u * F20_L64) {
            f20_u64v a = F20_LDV(f20_u64v, q);
            F20_KEEP(a);
            s0 += a & m32 & F20_LANE_MASK(F20_W_LO(we, wo));
            s1 += (a >> 32) & F20_LANE_MASK(F20_W_HI(we, wo));
            we >>= 2u * F20_L64;
            wo >>= 2u * F20_L64;
        }
#endif
    }
    return F20_HSUM_U64V(s0 + s1);
}

/* True SoA の 64 ビットの列の最小 / 最大の選択（f20_col_pick_key と同じ 2 段・同じ結果）。鍵はその場で作る:
 *   key = (int64)(値 ^ x)。x = 2^63（u64 の最小）・2^63 - 1（u64 の最大）・0（i64 の最小）・全ビット 1（i64 の最大）。
 *   マスクの立たないスロットは段 1 では NONE、段 2 では数えない
 *   段 1 のベクトル版（SSE4.2 以上・AArch64）: レーンのマスクで鍵と NONE を選んでから符号付き最小。スカラ版（素の SSE2 など）:
 *   語が全部 1 なら 4 本の CMOV の鎖、それ以外は立っているビットだけをたどる（ctz + blsr）。比較は (値 ^ y) の符号なしの
 *   順序で行う（y = x ^ 2^63。u64 の最小なら y = 0 で、値をそのまま比べる）
 * min / max pick over a True-SoA 64-bit column (the same two stages and result as f20_col_pick_key). Keys are made on the
 *   fly: key = (int64)(value ^ x), with x = 2^63 (u64 min), 2^63 - 1 (u64 max), 0 (i64 min), all ones (i64 max).
 *   Slots whose mask bit is clear are NONE in stage 1 and never counted in stage 2.
 *   stage 1, vector (SSE4.2 and up, AArch64): lane masks select between the key and NONE, then the signed minimum.
 *   scalar (plain SSE2 etc.): four CMOV chains when the word is all ones, otherwise only the set bits are walked
 *   (ctz + blsr). The compare uses the unsigned order of (value ^ y), y = x ^ 2^63 (for a u64 min y = 0: the raw values) */
static inline FASCON_MAYBE_UNUSED uint32_t f20_col_pick64_m(const uint64_t *F20_RESTRICT v, const uint64_t *F20_RESTRICT m, uint32_t n, int64_t *F20_RESTRICT best, const int last, const uint64_t x) {
    int64_t b = *best;
    uint32_t sbk = 0xFFFFFFFFu;
    /* 段 1 は 256 スロット（4 塊）ごとにレーンの最小を 1 回だけまとめる（塊ごとにまとめるより速い。段 2 はその 256 スロットを
     * 1 回だけ見る） / stage 1 folds the lanes once per 256 slots (4 blocks), cheaper than per block; stage 2 looks at
     * those 256 slots once */
    for (uint32_t sb = 0u; sb < n; sb += 256u) {
        const uint32_t end = sb + 256u < n ? sb + 256u : n;
        int any = 0;
#if F20_VEC_I64_CMP
        const f20_u64v xv = F20_SPLAT(f20_u64v, x), nv = F20_SPLAT(f20_u64v, (uint64_t)F20_KEY_NONE);
        f20_i64v m0 = F20_SPLAT(f20_i64v, F20_KEY_NONE), m1 = m0;
        for (uint32_t base = sb; base < end; base += 64u) {
            const uint64_t w = m[base >> 6];
            if (w == 0u) continue;
            any = 1;
            f20_u64v wv = F20_LANE_BITS(w);
            for (const uint64_t *q = v + base, *const e = q + 64; q < e; q += 2u * F20_L64) {
                f20_u64v a = F20_LDV(f20_u64v, q), c = F20_LDV(f20_u64v, q + F20_L64);
                F20_KEEP(a);
                F20_KEEP(c);
                const f20_u64v ma = F20_LANE_MASK(wv), mc = F20_LANE_MASK(wv >> F20_L64);
                wv >>= 2u * F20_L64;
                m0 = F20_MIN_I64V(m0, (f20_i64v)(((a ^ xv) & ma) | (nv & ~ma)));
                m1 = F20_MIN_I64V(m1, (f20_i64v)(((c ^ xv) & mc) | (nv & ~mc)));
            }
        }
        if (!any) continue;
        const int64_t bm = F20_HMIN_I64V(F20_MIN_I64V(m0, m1));
#else
        const uint64_t y = x ^ UINT64_C(0x8000000000000000);
        uint64_t u = ~UINT64_C(0);
        for (uint32_t base = sb; base < end; base += 64u) {
            const uint64_t w = m[base >> 6];
            if (w == 0u) continue;
            any = 1;
            const uint64_t *q = v + base;
            if (w == ~UINT64_C(0)) {
                uint64_t u0 = q[0] ^ y, u1 = q[1] ^ y, u2 = q[2] ^ y, u3 = q[3] ^ y;
                for (const uint64_t *const e = q + 64; (q += 4) < e;) {
                    const uint64_t t0 = q[0] ^ y, t1 = q[1] ^ y, t2 = q[2] ^ y, t3 = q[3] ^ y;
                    u0 = t0 < u0 ? t0 : u0;
                    u1 = t1 < u1 ? t1 : u1;
                    u2 = t2 < u2 ? t2 : u2;
                    u3 = t3 < u3 ? t3 : u3;
                }
                u0 = u1 < u0 ? u1 : u0;
                u2 = u3 < u2 ? u3 : u2;
                u0 = u2 < u0 ? u2 : u0;
                u = u0 < u ? u0 : u;
            } else {
                for (uint64_t r = w; r != 0u; r = f20_blsr64(r)) {
                    const uint64_t t = q[f20_ctz64(r)] ^ y;
                    u = t < u ? t : u;
                }
            }
        }
        if (!any) continue;
        const int64_t bm = (int64_t)(u ^ UINT64_C(0x8000000000000000));
#endif
        const uint32_t k = last ? (uint32_t)(bm <= b) : (uint32_t)(bm < b);
        b = (int64_t)F20_SEL64(k, (uint64_t)bm, (uint64_t)b);
        sbk = F20_SEL32(k, sb, sbk);
    }
    if (sbk == 0xFFFFFFFFu) return 0xFFFFFFFFu;
    /* 段 2: 勝った 256 スロットで、マスクが立ち鍵が最小に等しい最初（last なら最後）のスロット / stage 2: in the winning 256
     * slots, the first (last: the last) slot whose mask bit is set and whose key equals the minimum */
    const uint32_t end = sbk + 256u < n ? sbk + 256u : n;
    uint32_t found = 0xFFFFFFFFu;
    for (uint32_t base = sbk; base < end; base += 64u) {
        const uint64_t w = m[base >> 6];
        uint64_t hit = 0u;
        for (uint32_t j = 0u; j < 64u; j++) hit |= (uint64_t)(((w >> j) & 1u) & (uint64_t)((int64_t)(v[base + j] ^ x) == b)) << j;
        if (hit != 0u) {
            found = base + (last ? 63u - (uint32_t)__builtin_clzll(hit) : f20_ctz64(hit));
            if (!last) break;
        }
    }
    *best = b;
    return found;
}

/* True SoA の 32 ビットの列（u32 / i32）の最小 / 最大の選択。鍵 = (int64)拡張(値) ^ x（x = 0 なら最小、全ビット 1 なら最大）。
 * sgn = 1 で符号拡張。u64 のレーンとして読み、偶数番と奇数番の値を分けて 64 ビットに広げる（符号拡張は ^ 2^31 - 2^31）
 * min / max pick over a True-SoA 32-bit column (u32 / i32): key = (int64)extend(value) ^ x (x = 0: min, all ones:
 * max), sgn = 1 sign-extends. Read as u64 lanes; the even and odd values are split and widened to 64 bits (sign
 * extension as ^ 2^31 - 2^31) */
static inline FASCON_MAYBE_UNUSED uint32_t f20_col_pick32_m(const uint32_t *F20_RESTRICT v, const uint64_t *F20_RESTRICT m, uint32_t n, int64_t *F20_RESTRICT best, const int last, const int sgn, const uint64_t x) {
    int64_t b = *best;
    uint32_t sbk = 0xFFFFFFFFu;
    const uint64_t sx = sgn ? UINT64_C(0x80000000) : 0u; /* 符号拡張の定数（0 なら零拡張） / the sign-extension constant (0: zero extension) */
    for (uint32_t sb = 0u; sb < n; sb += 256u) {
        const uint32_t end = sb + 256u < n ? sb + 256u : n;
        int any = 0;
#if F20_VEC_I64_CMP
        const f20_u64v xv = F20_SPLAT(f20_u64v, x), nv = F20_SPLAT(f20_u64v, (uint64_t)F20_KEY_NONE);
        const f20_u64v m32 = F20_SPLAT(f20_u64v, 0xFFFFFFFFu), sv = F20_SPLAT(f20_u64v, sx);
        f20_i64v m0 = F20_SPLAT(f20_i64v, F20_KEY_NONE), m1 = m0;
        for (uint32_t base = sb; base < end; base += 64u) {
            const uint64_t w = m[base >> 6];
            if (w == 0u) continue;
            any = 1;
            f20_u64v we = F20_SPLAT(f20_u64v, w) >> ((f20_u64v)F20_LANE_SH64 * F20_SPLAT(f20_u64v, 2u));
            f20_u64v wo = we >> 1u;
            for (const uint32_t *q = v + base, *const e = q + 64; q < e; q += 2u * F20_L64) {
                f20_u64v a = F20_LDV(f20_u64v, q);
                F20_KEEP(a);
                const f20_u64v ev = (((a & m32) ^ sv) - sv) ^ xv, od = (((a >> 32) ^ sv) - sv) ^ xv;
                const f20_u64v me = F20_LANE_MASK(F20_W_LO(we, wo)), mo = F20_LANE_MASK(F20_W_HI(we, wo));
                we >>= 2u * F20_L64;
                wo >>= 2u * F20_L64;
                m0 = F20_MIN_I64V(m0, (f20_i64v)((ev & me) | (nv & ~me)));
                m1 = F20_MIN_I64V(m1, (f20_i64v)((od & mo) | (nv & ~mo)));
            }
        }
        if (!any) continue;
        const int64_t bm = F20_HMIN_I64V(F20_MIN_I64V(m0, m1));
#else
        int64_t bm = F20_KEY_NONE;
        for (uint32_t base = sb; base < end; base += 64u) {
            const uint64_t w = m[base >> 6];
            if (w == 0u) continue;
            any = 1;
            const uint32_t *q = v + base;
            if (w == ~UINT64_C(0)) {
                /* 語が全部 1: 4 本の CMOV の鎖 / an all-ones word: four CMOV chains */
                int64_t b0 = F20_KEY_NONE, b1 = F20_KEY_NONE, b2 = F20_KEY_NONE, b3 = F20_KEY_NONE;
                for (const uint32_t *const e = q + 64; q < e; q += 4) {
                    const int64_t t0 = (int64_t)(((((uint64_t)q[0]) ^ sx) - sx) ^ x), t1 = (int64_t)(((((uint64_t)q[1]) ^ sx) - sx) ^ x);
                    const int64_t t2 = (int64_t)(((((uint64_t)q[2]) ^ sx) - sx) ^ x), t3 = (int64_t)(((((uint64_t)q[3]) ^ sx) - sx) ^ x);
                    b0 = t0 < b0 ? t0 : b0;
                    b1 = t1 < b1 ? t1 : b1;
                    b2 = t2 < b2 ? t2 : b2;
                    b3 = t3 < b3 ? t3 : b3;
                }
                b0 = b1 < b0 ? b1 : b0;
                b2 = b3 < b2 ? b3 : b2;
                b0 = b2 < b0 ? b2 : b0;
                bm = b0 < bm ? b0 : bm;
            } else {
                for (uint64_t r = w; r != 0u; r = f20_blsr64(r)) {
                    const int64_t t = (int64_t)(((((uint64_t)q[f20_ctz64(r)]) ^ sx) - sx) ^ x);
                    bm = t < bm ? t : bm;
                }
            }
        }
        if (!any) continue;
#endif
        const uint32_t k = last ? (uint32_t)(bm <= b) : (uint32_t)(bm < b);
        b = (int64_t)F20_SEL64(k, (uint64_t)bm, (uint64_t)b);
        sbk = F20_SEL32(k, sb, sbk);
    }
    if (sbk == 0xFFFFFFFFu) return 0xFFFFFFFFu;
    const uint32_t end = sbk + 256u < n ? sbk + 256u : n;
    uint32_t found = 0xFFFFFFFFu;
    for (uint32_t base = sbk; base < end; base += 64u) {
        const uint64_t w = m[base >> 6];
        uint64_t hit = 0u;
        for (uint32_t j = 0u; j < 64u; j++) hit |= (uint64_t)(((w >> j) & 1u) & (uint64_t)((int64_t)(((((uint64_t)v[base + j]) ^ sx) - sx) ^ x) == b)) << j;
        if (hit != 0u) {
            found = base + (last ? 63u - (uint32_t)__builtin_clzll(hit) : f20_ctz64(hit));
            if (!last) break;
        }
    }
    *best = b;
    return found;
}

/* ───── 非時間的ストア・プリフェッチ / non-temporal stores and prefetch ─────
 *   F20_NT_STORE_U64 / _U32: キャッシュを通さないストア（書き込み前の読み出し RFO が要らない）。clang は
 *   __builtin_nontemporal_store（x86-64: MOVNTI、AArch64: STNP、ほかは普通のストア）、GCC の x86-64 は MOVNTI の asm、
 *   それ以外は普通のストア。F20_SFENCE: 非時間的ストアをそれより後のストアより先に見えるようにする（x86: SFENCE、ほかは
 *   release の柵）。コンパイラは非時間的ストアのループの出口に必ず置く。F20_PREFETCH_R: 読み出しのプリフェッチの合図
 *   F20_NT_STORE_U64 / _U32: stores that bypass the caches (no read-for-ownership before the write). clang:
 *   __builtin_nontemporal_store (x86-64: MOVNTI, AArch64: STNP, plain stores elsewhere); GCC on x86-64: a MOVNTI asm;
 *   plain stores everywhere else. F20_SFENCE makes the non-temporal stores visible before any later store (x86:
 *   SFENCE, elsewhere a release fence); the compiler always places one at the exit of a non-temporal loop.
 *   F20_PREFETCH_R: a read prefetch hint */
#if defined(__clang__) && defined(__has_builtin)
#if __has_builtin(__builtin_nontemporal_store)
#define F20_HAVE_NT_BUILTIN 1
#endif
#endif
#if defined(F20_HAVE_NT_BUILTIN)
#define F20_NT_STORE_U64(p, v) __builtin_nontemporal_store((uint64_t)(v), (uint64_t *)(p))
#define F20_NT_STORE_U32(p, v) __builtin_nontemporal_store((uint32_t)(v), (uint32_t *)(p))
#define F20_NT_STORE_I64(p, v) __builtin_nontemporal_store((int64_t)(v), (int64_t *)(p))
#define F20_NT_STORE_I32(p, v) __builtin_nontemporal_store((int32_t)(v), (int32_t *)(p))
#define F20_NT_STORE_F32(p, v) __builtin_nontemporal_store((float)(v), (float *)(p))
#define F20_NT_STORE_F64(p, v) __builtin_nontemporal_store((double)(v), (double *)(p))
#elif defined(__GNUC__) && defined(__x86_64__)
#define F20_NT_STORE_U64(p, v) __asm__("movnti %1, %0" : "=m"(*(uint64_t *)(p)) : "r"((uint64_t)(v)))
#define F20_NT_STORE_U32(p, v) __asm__("movnti %1, %0" : "=m"(*(uint32_t *)(p)) : "r"((uint32_t)(v)))
#define F20_NT_STORE_I64(p, v) __asm__("movnti %1, %0" : "=m"(*(int64_t *)(p)) : "r"((int64_t)(v)))
#define F20_NT_STORE_I32(p, v) __asm__("movnti %1, %0" : "=m"(*(int32_t *)(p)) : "r"((int32_t)(v)))
/* 浮動小数点の列: ビットの並び（memcpy で取り出す）を MOVNTI で、メモリの被演算子は列の型のまま（型の別名の規則に触れない）
 * float columns: the bit pattern (taken out with memcpy) through MOVNTI, with the memory operand kept in the column's
 * type (the aliasing rules hold) */
#define F20_NT_STORE_F32(p, v) __asm__("movnti %1, %0" : "=m"(*(float *)(p)) : "r"(f20_f32_to_bits((float)(v))))
#define F20_NT_STORE_F64(p, v) __asm__("movnti %1, %0" : "=m"(*(double *)(p)) : "r"(f20_f64_to_bits((double)(v))))
#else
#define F20_NT_STORE_U64(p, v) (*(uint64_t *)(p) = (uint64_t)(v))
#define F20_NT_STORE_U32(p, v) (*(uint32_t *)(p) = (uint32_t)(v))
#define F20_NT_STORE_I64(p, v) (*(int64_t *)(p) = (int64_t)(v))
#define F20_NT_STORE_I32(p, v) (*(int32_t *)(p) = (int32_t)(v))
#define F20_NT_STORE_F32(p, v) (*(float *)(p) = (float)(v))
#define F20_NT_STORE_F64(p, v) (*(double *)(p) = (double)(v))
#endif

/* ───── 条件付き一括更新（Masked Column RMW）/ masked column read-modify-write ─────
 *   `for id in @a { if 条件 { @a[id].col += x } }` / `@a[id].col = x`（col は完全列化の列、条件は条件のビット列 fb = 生存 && 条件）
 *   を、ビットマップ + ctz の飛び石のスカラ更新ではなく、ベクトル幅の直線の走査にする。条件のビット列を 1 語（64 スロット）
 *   ずつ読み、
 *     語 == 0   → その 64 スロットの列の行（キャッシュライン）は読みも書きもしない
 *     語 == ~0  → マスクなしのベクトルの加算・代入（nt なら非時間的ストア F20_NT_STORE）
 *     その他    → ビットをレーンへ広げ（語をレーンへ配って 1 << レーン番号との AND を 0 と比べる）、選ばれたレーンだけ
 *                 v + x / x にする述語付きのベクトル演算でその場で更新（キャッシュを汚さないよう非時間的ストアは使わない）
 *   選ばれていないレーンは読んだビットをそのまま書き戻す（共有しないアリーナだけが対象）。整数の加算はレーンごとにあふれを
 *   確かめ、あふれる語があれば、その語を 1 つも書かずに語の先頭の添字を返す（呼び出し側がその語から逐次のループで続け、
 *   同じ場所で同じ panic になる）。浮動小数点の加算（#[fast_math] の下だけ）は v + x を選ばれたレーンにだけ入れる
 *   `for id in @a { if cond { @a[id].col += x } }` / `@a[id].col = x` (col a True-SoA column, the condition a filter bit
 *   column fb = live && cond) become a straight vector-width scan instead of the bitmap + ctz stepping-stone scalar update.
 *   The filter bit column is read one word (64 slots) at a time:
 *     word == 0   → the column's lines (cache lines) for those 64 slots are neither loaded nor stored
 *     word == ~0  → an unmasked vector add / assign (non-temporal stores F20_NT_STORE when nt)
 *     otherwise   → the bits are spread over the lanes (the word broadcast to every lane, ANDed with 1 << lane, compared
 *                   with 0) and a predicated vector operation updates in place, only the chosen lanes getting v + x / x
 *                   (no non-temporal stores, so the cache is not polluted)
 *   Lanes not chosen are written back with the bits that were read (only arenas nobody else shares qualify). Integer adds
 *   check every lane for overflow; when a word would overflow, nothing of it is written and the index of its first slot
 *   is returned (the caller continues from that word with the sequential loop, which panics at the same place, the same
 *   way). Float adds (only under #[fast_math]) put v + x into the chosen lanes only */
typedef uint32_t f20_u32v __attribute__((vector_size(F20_VECTOR_BYTES)));
typedef int32_t f20_i32v __attribute__((vector_size(F20_VECTOR_BYTES)));
#define F20_STV(T, p, v) (*(T *)(void *)(p) = (v))
#define F20_RMW_NONE 0xFFFFFFFFu /* あふれなし / no overflow */

/* レーン l = (語 >> j) のビット l が立っているか（-1 / 0）。64 ビットのレーンも 32 ビットのレーンの比較で作る（64 ビットの
 * レーンの 2 つの半分に同じビットを選ばせる: SSE2 には 64 ビットの比較がない）
 * lane l = whether bit l of (word >> j) is set (-1 / 0). 64-bit lanes are built with 32-bit lane compares too (both
 * halves of a 64-bit lane select the same bit: SSE2 has no 64-bit compare) */
#define F20_RMW_SEL32(sel)                                    \
    f20_u32v sel = {0};                                       \
    for (int _l = 0; _l < F20_L32F; _l++) sel[_l] = (uint32_t)1 << _l
#define F20_RMW_SEL64(sel)                                    \
    f20_u32v sel = {0};                                       \
    for (int _l = 0; _l < F20_L32F; _l++) sel[_l] = (uint32_t)1 << (_l >> 1)
#define F20_RMW_MASK32(w, j, sel) ((f20_u32v)((F20_SPLAT(f20_u32v, (uint32_t)((w) >> (j))) & (sel)) != 0))
#define F20_RMW_MASK64(w, j, sel) ((f20_u64v)((F20_SPLAT(f20_u32v, (uint32_t)((w) >> (j))) & (sel)) != 0))
/* 全レーンに同じ値（浮動小数点の -0.0 も符号のまま。F20_SPLAT の {0} + x は -0.0 を +0.0 にする）
 * the same value in every lane (a float -0.0 keeps its sign; F20_SPLAT's {0} + x would turn -0.0 into +0.0) */
#define F20_BCAST(T, L, x) ({ T _bv = {0}; for (int _bk = 0; _bk < (L); _bk++) _bv[_bk] = (x); _bv; })
/* どれかのレーンの最上位ビットが立っているか / whether any lane has its top bit set */
#define F20_RMW_TOP(T, L, BITS, v) ({ const T _av = (v); uint64_t _ar = 0u; for (int _k = 0; _k < (L); _k++) _ar |= (uint64_t)_av[_k]; ((_ar >> ((BITS) - 1)) & 1u) != 0u; })
/* あふれは最上位ビットで（比較を使わない: SSE2 には 64 ビットの比較も符号なしの比較もない）。符号なしは繰り上がり
 * (v & x) | ((v | x) & ~r)、符号付きは (v ^ r) & (x ^ r) / overflow in the top bit (no compares: SSE2 has neither 64-bit
 * nor unsigned compares): unsigned is the carry (v & x) | ((v | x) & ~r), signed is (v ^ r) & (x ^ r) */
#define F20_RMW_OVF_U(v, x, r) (((v) & (x)) | (((v) | (x)) & ~(r)))
#define F20_RMW_OVF_S(v, x, r) (((v) ^ (r)) & ((x) ^ (r)))

/* 整数の加算: 1 回の走査で足して書き、語の中にあふれるレーンがあれば、書いた値から x を引いて（剰余の算術なので元に正確に
 * 戻る）語の先頭の添字を返す。マスクで外したレーンは x = 0 なのであふれない。全部 1 の語を非時間的ストアで書くときは、先に
 * あふれを確かめてから書く（書いた値を読み戻さない）
 * integer adds: one pass adds and stores; when a lane of the word overflows, x is subtracted from what was written
 * (modular arithmetic restores the original exactly) and the word's first index is returned. Masked-out lanes add 0, so
 * they never overflow. All-one words written with non-temporal stores are checked first and then written (nothing
 * written is read back) */
#define F20_RMW_ADD_INT(NAME, T, VT, UVT, L, BITS, SEL, MASK, NTST, OVF)                                                      \
    static inline FASCON_MAYBE_UNUSED uint32_t f20_col_masked_add_##NAME(T *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n, T x, int nt) { \
        SEL(sel);                                                                                                             \
        const UVT xv = F20_SPLAT(UVT, (__typeof__(xv[0]))x);                                                                  \
        for (uint32_t base = 0u; base < n; base += 64u) {                                                                    \
            const uint64_t w = m[base >> 6];                                                                                  \
            if (w == 0u) continue;                                                                                            \
            T *const q = c + base;                                                                                            \
            UVT of = {0};                                                                                                     \
            if (w == ~UINT64_C(0)) {                                                                                          \
                if (nt) {                                                                                                     \
                    for (uint32_t j = 0u; j < 64u; j += (L)) {                                                                \
                        const UVT v = (UVT)F20_LDV(VT, q + j), r = v + xv;                                                    \
                        of |= OVF(v, xv, r);                                                                                  \
                    }                                                                                                         \
                    if (F20_RMW_TOP(UVT, L, BITS, of)) return base;                                                           \
                    for (uint32_t j = 0u; j < 64u; j++) NTST(&q[j], (T)(q[j] + x));                                           \
                    continue;                                                                                                 \
                }                                                                                                             \
                for (uint32_t j = 0u; j < 64u; j += (L)) {                                                                    \
                    const UVT v = (UVT)F20_LDV(VT, q + j), r = v + xv;                                                        \
                    of |= OVF(v, xv, r);                                                                                      \
                    F20_STV(VT, q + j, (VT)r);                                                                                \
                }                                                                                                             \
                if (FASCON_UNLIKELY(F20_RMW_TOP(UVT, L, BITS, of))) {                                                         \
                    for (uint32_t j = 0u; j < 64u; j += (L)) F20_STV(VT, q + j, (VT)((UVT)F20_LDV(VT, q + j) - xv));          \
                    return base;                                                                                              \
                }                                                                                                             \
                continue;                                                                                                     \
            }                                                                                                                 \
            for (uint32_t j = 0u; j < 64u; j += (L)) {                                                                        \
                const UVT v = (UVT)F20_LDV(VT, q + j), xm = xv & MASK(w, j, sel), r = v + xm;                                 \
                of |= OVF(v, xm, r);                                                                                          \
                F20_STV(VT, q + j, (VT)r);                                                                                    \
            }                                                                                                                 \
            if (FASCON_UNLIKELY(F20_RMW_TOP(UVT, L, BITS, of))) {                                                             \
                for (uint32_t j = 0u; j < 64u; j += (L)) F20_STV(VT, q + j, (VT)((UVT)F20_LDV(VT, q + j) - (xv & MASK(w, j, sel)))); \
                return base;                                                                                                  \
            }                                                                                                                 \
        }                                                                                                                     \
        return F20_RMW_NONE;                                                                                                  \
    }
F20_RMW_ADD_INT(u32, uint32_t, f20_u32v, f20_u32v, F20_L32F, 32, F20_RMW_SEL32, F20_RMW_MASK32, F20_NT_STORE_U32, F20_RMW_OVF_U)
F20_RMW_ADD_INT(u64, uint64_t, f20_u64v, f20_u64v, F20_L64, 64, F20_RMW_SEL64, F20_RMW_MASK64, F20_NT_STORE_U64, F20_RMW_OVF_U)
F20_RMW_ADD_INT(i32, int32_t, f20_i32v, f20_u32v, F20_L32F, 32, F20_RMW_SEL32, F20_RMW_MASK32, F20_NT_STORE_I32, F20_RMW_OVF_S)
F20_RMW_ADD_INT(i64, int64_t, f20_i64v, f20_u64v, F20_L64, 64, F20_RMW_SEL64, F20_RMW_MASK64, F20_NT_STORE_I64, F20_RMW_OVF_S)

/* 代入（整数・浮動小数点とも。浮動小数点はビットの並びのまま選ぶ） / assignment (integers and floats; floats are chosen
 * as bit patterns) */
#define F20_RMW_SET(NAME, T, VT, UVT, L, SEL, MASK, NTST)                                                                     \
    static inline FASCON_MAYBE_UNUSED void f20_col_masked_set_##NAME(T *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n, T x, int nt) { \
        SEL(sel);                                                                                                             \
        const VT xv = F20_BCAST(VT, L, x);                                                                                    \
        const UVT xb = (UVT)xv;                                                                                               \
        for (uint32_t base = 0u; base < n; base += 64u) {                                                                    \
            const uint64_t w = m[base >> 6];                                                                                  \
            if (w == 0u) continue;                                                                                            \
            T *const q = c + base;                                                                                            \
            if (w == ~UINT64_C(0)) {                                                                                          \
                if (nt) {                                                                                                     \
                    for (uint32_t j = 0u; j < 64u; j++) NTST(&q[j], x);                                                       \
                } else {                                                                                                      \
                    for (uint32_t j = 0u; j < 64u; j += (L)) F20_STV(VT, q + j, xv);                                          \
                }                                                                                                             \
                continue;                                                                                                     \
            }                                                                                                                 \
            for (uint32_t j = 0u; j < 64u; j += (L)) {                                                                        \
                const UVT mk = MASK(w, j, sel), v = (UVT)F20_LDV(VT, q + j);                                                  \
                F20_STV(VT, q + j, (VT)((xb & mk) | (v & ~mk)));                                                              \
            }                                                                                                                 \
        }                                                                                                                     \
    }
F20_RMW_SET(u32, uint32_t, f20_u32v, f20_u32v, F20_L32F, F20_RMW_SEL32, F20_RMW_MASK32, F20_NT_STORE_U32)
F20_RMW_SET(u64, uint64_t, f20_u64v, f20_u64v, F20_L64, F20_RMW_SEL64, F20_RMW_MASK64, F20_NT_STORE_U64)
F20_RMW_SET(i32, int32_t, f20_i32v, f20_u32v, F20_L32F, F20_RMW_SEL32, F20_RMW_MASK32, F20_NT_STORE_I32)
F20_RMW_SET(i64, int64_t, f20_i64v, f20_u64v, F20_L64, F20_RMW_SEL64, F20_RMW_MASK64, F20_NT_STORE_I64)
F20_RMW_SET(f32, float, f20_f32v, f20_u32v, F20_L32F, F20_RMW_SEL32, F20_RMW_MASK32, F20_NT_STORE_F32)
F20_RMW_SET(f64, double, f20_f64v, f20_u64v, F20_L64, F20_RMW_SEL64, F20_RMW_MASK64, F20_NT_STORE_F64)

/* 浮動小数点の加算（#[fast_math] の下だけ）: 選ばれたレーンだけ v + x、ほかはビットをそのまま（-0.0 も NaN の中身も変えない）
 * float adds (only under #[fast_math]): v + x in the chosen lanes only, the others keep their bits (-0.0 and NaN payloads
 * untouched) */
#define F20_RMW_ADD_FLT(NAME, T, VT, UVT, L, SEL, MASK, NTST)                                                                 \
    static inline FASCON_MAYBE_UNUSED void f20_col_masked_add_##NAME(T *F20_RESTRICT c, const uint64_t *F20_RESTRICT m, uint32_t n, T x, int nt) { \
        SEL(sel);                                                                                                             \
        const VT xv = F20_BCAST(VT, L, x);                                                                                    \
        for (uint32_t base = 0u; base < n; base += 64u) {                                                                    \
            const uint64_t w = m[base >> 6];                                                                                  \
            if (w == 0u) continue;                                                                                            \
            T *const q = c + base;                                                                                            \
            if (w == ~UINT64_C(0)) {                                                                                          \
                if (nt) {                                                                                                     \
                    for (uint32_t j = 0u; j < 64u; j++) NTST(&q[j], (T)(q[j] + x));                                           \
                } else {                                                                                                      \
                    for (uint32_t j = 0u; j < 64u; j += (L)) F20_STV(VT, q + j, F20_LDV(VT, q + j) + xv);                     \
                }                                                                                                             \
                continue;                                                                                                     \
            }                                                                                                                 \
            for (uint32_t j = 0u; j < 64u; j += (L)) {                                                                        \
                const VT v = F20_LDV(VT, q + j);                                                                              \
                const UVT mk = MASK(w, j, sel);                                                                               \
                F20_STV(VT, q + j, (VT)(((UVT)(v + xv) & mk) | ((UVT)v & ~mk)));                                              \
            }                                                                                                                 \
        }                                                                                                                     \
    }
F20_RMW_ADD_FLT(f32, float, f20_f32v, f20_u32v, F20_L32F, F20_RMW_SEL32, F20_RMW_MASK32, F20_NT_STORE_F32)
F20_RMW_ADD_FLT(f64, double, f20_f64v, f20_u64v, F20_L64, F20_RMW_SEL64, F20_RMW_MASK64, F20_NT_STORE_F64)
#if defined(__x86_64__) || defined(__i386__)
#define F20_SFENCE() __asm__ __volatile__("sfence" ::: "memory")
#else
#define F20_SFENCE() __atomic_thread_fence(__ATOMIC_RELEASE)
#endif
#define F20_PREFETCH_R(p) __builtin_prefetch((const void *)(p), 0, 0)

/* 列を持つアリーナの表（列は live[] のすぐ後ろ: 走査が読むもの = 生存ビットマップと列が表の先頭にまとまる。各列は 128 B 境界）
 * a table with columns (the columns sit right after live[]: everything scans read, the live bitmap and the columns, is
 * at the front of the table; every column starts on a 128-byte boundary) */
#define F20_DEFINE_TABLE_COLS(tag, K, GenT, ColsT)                                    \
    typedef struct F20_ALIGN128 {                                                     \
        F20_ALIGN128 f20_arena_state_t st;                                            \
        uint8_t st_pad_[F20_DESTRUCTIVE_SIZE - sizeof(f20_arena_state_t)];            \
        F20_ALIGN128 uint64_t live[F20_LIVE_WORDS(K)];                                \
        F20_ALIGN128 ColsT col;                                                       \
        F20_ALIGN128 GenT gen[F20_SLOTS(K)];                                          \
        F20_ALIGN128 uint32_t link[F20_SLOTS(K)];                                     \
        F20_ALIGN128 f20_slot_##tag##_t slot[F20_SLOTS(K) + 1u];                      \
    } f20_arena_##tag##_t

/* コミット点: ラッチが立っていればここで安全に止める（外へ影響が出る直前・関数から戻る直前・ループを出たところ）
 * a commit point: stop safely here if the latch is set (right before an external effect, a return, or at a loop exit)
 *
 *   回数が決まったループ（`for i in a..b`・`for id in @a`）の中にはコミット点を置かない。外へ影響の出る操作（呼び出し・
 *   出力・MMIO・return）の前には別にコミット点があり、ループは必ず終わるので、ループを出た所で 1 回確かめれば同じ保証になる
 *   （本体にジャンプが 1 本減り、C コンパイラがベクトル化できる形になる）。回数が決まらないループ（while・loop）では、
 *   ラッチを終了条件に混ぜて（F20_LATCH_OK）、ループの出口と同じ 1 本の分岐にする
 *   loops with a fixed trip count (`for i in a..b`, `for id in @a`) carry no commit point inside: every external effect
 *   (call, output, MMIO, return) already has its own, and the loop always ends, so one check at the exit gives the same
 *   guarantee (one jump fewer in the body, and a shape the C compiler can vectorize). Loops without a fixed trip count
 *   (while, loop) fold the latch into their exit condition (F20_LATCH_OK), so it shares the exit's single branch */
FASCON_NORETURN void fascon_fault_stop(uint32_t flt, const char *site);
#define F20_COMMIT(flt, site)                                        \
    do {                                                             \
        if (FASCON_UNLIKELY((flt) != 0u)) fascon_fault_stop((flt), (site)); \
    } while (0)
#define F20_LATCH_OK(flt) ((flt) == 0u)

FASCON_NORETURN void fascon_stale_id_panic(f20_id_t id, uint32_t k, const char *arena, const char *site);
FASCON_NORETURN void fascon_unwrap_panic(const char *site);

/* ───────────── own T の所有リージョン / own-T regions ─────────────
 *
 *   所有リージョン = [16 バイトの見出し][根のペイロード][詰めた子: 見出し + ペイロード]...
 *   an own region = [16-byte header][root payload][packed children: header + payload]...
 *   - 構築式の中で作った `own` の子（Option でないフィールド）は同じブロックに詰める（1 回の確保）
 *     `own` children built inside the construction expression (non-Option fields) are packed into the same block
 *   - 解放は根のブロックを 1 回返すだけ。詰めた子の解放は何もしない（見出しの EMBEDDED）
 *     freeing returns the root block once; freeing a packed child is a no-op (EMBEDDED header)
 *   - サイズ別の空きリストで O(1)。断片化は 2 倍以内 / O(1) with size-class free lists; fragmentation within 2x
 */
#define FASCON_OWN_HDR 16u
#define FASCON_OWN_EMBEDDED 0xFFFFu
#define FASCON_ALIGN16(n) ((((size_t)(n)) + 15u) & ~(size_t)15u)
#define FASCON_OWN_SPAN(T) FASCON_ALIGN16(sizeof(T))
#define FASCON_OWN_CHILD_SPAN(T) (FASCON_OWN_HDR + FASCON_ALIGN16(sizeof(T)))

typedef struct {
    uint32_t magic;
    uint16_t cls;
    uint16_t flags;
    uint32_t size;
    uint32_t pad;
} FasconOwnHdr;

#define FASCON_OWN_MAGIC 0x314E574Fu /* "OWN1" */
#define FASCON_OWN_DEAD 0x44414544u  /* "DEAD" */

void fascon_heap_init(void *base, size_t size);
void *fascon_own_alloc(size_t payload, const char *site);
void fascon_own_free(void *payload);
uintptr_t fascon_heap_live_blocks(void);
uintptr_t fascon_heap_live_bytes(void);

/* ───────────── 関数の中で閉じる own: CPU ごとのフレームリージョン / own values that stay inside a function: per-CPU frames ─────────────
 *
 *   エスケープ解析（コンパイラ）で「どこへも移動しない」と分かった `let x = own T { .. }` は、ヒープの空きリストではなく
 *   事前に確保したフレームリージョンから、ポインタを進めるだけで取る（スタックと同じ動き）。確保はインライン展開した
 *   比較 1 回と加算 1 回、解放は x のスコープの終わりで先頭ポインタを戻すだけ。MMU にもページテーブルにも触らない。
 *   `let x = own T { .. }` that escape analysis (in the compiler) proves is never moved is taken from a pre-reserved frame
 *   region by bumping a pointer (the same motion as a stack) instead of the heap free lists: allocation is one inlined
 *   compare and one add; release is resetting the top pointer at the end of x's scope. No MMU or page-table work at all.
 *
 *   - LIFO が保たれる: フレームを使うのは let の初期化だけで、解放は宣言の逆順（スコープの入れ子と同じ）。呼び出し先の
 *     フレームは戻る前に解放される / LIFO holds: only let initializers use the frame and releases run in reverse
 *     declaration order (the nesting of scopes); a callee releases its frame before it returns
 *   - ループの本体の中の x は周回ごとに戻すので、何周してもフレームは伸びない / an x inside a loop body is released every
 *     iteration, so the frame does not grow with the trip count
 *   - あふれたときはヒープへ切り替える（結果は同じ。fascon_own_free が見出しで見分ける） / when the frame is full the
 *     allocation falls back to the heap (same result; fascon_own_free tells them apart by the header)
 *   - FASCON2_CPU_LOCAL: 1.0 の BSP は CPU0 だけなので既定は空。SMP にするときは CPU ごとの実体にする（_Thread_local など）。
 *     構造体は 128 バイト境界なので、コアごとのフレームが同じラインに乗ることはない
 *     FASCON2_CPU_LOCAL: empty by default because the 1.0 BSP runs CPU0 only; for SMP make it per CPU (_Thread_local or
 *     similar). The struct is 128-byte aligned, so per-core frames never share a line */
#define FASCON_OWN_FRAME 0xFFFEu
#ifndef FASCON2_CPU_LOCAL
#if defined(FASCON2_SMP)
#define FASCON2_CPU_LOCAL __thread /* コアごとの実体（ベアメタルは TPIDR_EL1、PC はスレッド） / one per core (TPIDR_EL1 on bare metal, a thread on the PC) */
#else
#define FASCON2_CPU_LOCAL
#endif
#endif
#ifndef FASCON2_FRAME_SIZE
#define FASCON2_FRAME_SIZE (64u << 10) /* 既定 64 KiB（BSP は fascon_frame_init で差し替える） / default 64 KiB */
#endif
typedef struct F20_ALIGN128 {
    uint8_t *top, *end, *base;
    uintptr_t peak;
    uint8_t pad_[F20_DESTRUCTIVE_SIZE - 3u * sizeof(uint8_t *) - sizeof(uintptr_t)];
} FasconFrame;
extern FASCON2_CPU_LOCAL FasconFrame fascon__frame;
void fascon_frame_init(void *base, size_t size);
uintptr_t fascon_frame_peak_bytes(void);

/* 印（いまの先頭） / a mark (the current top) */
static inline uint8_t *fascon_frame_mark(void) { return fascon__frame.top; }
/* 印まで戻す（その後に取ったものをまとめて返す）。最大の深さも分岐なしで記録 / release back to a mark (everything taken
 * after it at once), recording the deepest use without a branch */
static inline void fascon_frame_release(uint8_t *mark) {
    FasconFrame *f = &fascon__frame;
    const uintptr_t used = (uintptr_t)(f->top - f->base);
    f->peak = used > f->peak ? used : f->peak;
    f->top = mark;
}
static inline void *fascon_frame_alloc(size_t payload, const char *site) {
    FasconFrame *f = &fascon__frame;
    const size_t need = FASCON_OWN_HDR + FASCON_ALIGN16(payload);
    uint8_t *blk = f->top;
    if (FASCON_UNLIKELY((size_t)(f->end - blk) < need)) return fascon_own_alloc(payload, site); /* 満杯: ヒープへ / full: heap */
    f->top = blk + need;
    FasconOwnHdr *h = (FasconOwnHdr *)blk;
    h->magic = FASCON_OWN_MAGIC;
    h->cls = FASCON_OWN_FRAME;
    h->flags = 0u;
    h->size = (uint32_t)need;
    h->pad = 0u;
    return blk + FASCON_OWN_HDR;
}

static inline void *fascon_own_embed(void *root, size_t off) {
    FasconOwnHdr *h = (FasconOwnHdr *)((char *)root + off);
    h->magic = FASCON_OWN_MAGIC;
    h->cls = FASCON_OWN_EMBEDDED;
    h->flags = 0u;
    h->size = 0u;
    h->pad = 0u;
    return (char *)h + FASCON_OWN_HDR;
}

/* ───────────── 固定番地リージョンのヘルパ / helpers for fixed-address regions ───────────── */

/* 生成 C が出す MMIO リージョン表（BSP が起動時に読む: フレームバッファの設定と、カーネル像との重なり検査）
 * the MMIO region table emitted by generated C (read by the BSP at boot: framebuffer setup and overlap checks) */
#define FASCON_MMIO_FRAMEBUFFER 0x1u
typedef struct {
    const char *name;
    uintptr_t base;
    uintptr_t size;
    uint32_t flags;
    uint32_t width, height, bpp;
} FasconMmioRegion;
extern const FasconMmioRegion fascon2_mmio_regions[];
extern const uint32_t fascon2_mmio_region_count;
#if defined(FASCON2_HOSTED_MMIO)
/* PC 上の実行（fascon run）では、各リージョンの実体をプラットフォーム層が確保してここに番地を入れる
 * when running on a PC (fascon run) the platform layer allocates each region and stores its address here */
extern uintptr_t fascon2_host_mmio_base[];
#endif

/* 32 ビット画素の矩形塗り（クリップ付き・通常メモリ） / fill a clipped rectangle of 32-bit pixels (normal memory) */
static inline void fascon_fill_rect32(uint32_t *base, int32_t W, int32_t H, int32_t x, int32_t y, int32_t w, int32_t h, uint32_t c) {
    int64_t x0 = x, y0 = y, x1 = (int64_t)x + w, y1 = (int64_t)y + h;
    if (x0 < 0) x0 = 0;
    if (y0 < 0) y0 = 0;
    if (x1 > W) x1 = W;
    if (y1 > H) y1 = H;
    for (int64_t yy = y0; yy < y1; yy++) {
        uint32_t *row = base + (size_t)yy * (size_t)W;
        for (int64_t xx = x0; xx < x1; xx++) row[xx] = c;
    }
}


/* ═════════════ ユニバーサル OS 拡張（Fascon 1.0: `--target rtos` / `--target generic-os` と新しい構文） ═════════════
 * universal-OS extension (Fascon 1.0: `--target rtos` / `--target generic-os` and the new syntax)
 *
 *   生成 C は、新しい構文（user T・catch_fault・atomic・rcu_read・#[secret]・dma own・bind_mmio・asm・extern "C" …）を
 *   使うか、プロファイルを指定したときだけ FASCON2_UNIVERSAL を定義する。今までのプログラムの C には何も足さない。
 *   generated C defines FASCON2_UNIVERSAL only when it uses the new syntax or a profile is given; earlier programs get
 *   nothing new. */
#if defined(FASCON2_BOARD) && !(defined(FASCON2_UNIVERSAL) || defined(FASCON2_GENERIC_OS) || defined(FASCON2_RTOS))
static inline FASCON_MAYBE_UNUSED void fascon_board_init(void) {}
#endif
#if defined(FASCON2_UNIVERSAL) || defined(FASCON2_GENERIC_OS) || defined(FASCON2_RTOS)
#ifndef FASCON2_UNIVERSAL
#define FASCON2_UNIVERSAL 1
#endif

/* ラッチのビット（F20_FAULT_STALE = 1・F20_FAULT_ARITH = 2 は上） / latch bits (STALE = 1 and ARITH = 2 are above) */
#define F20_FAULT_USER 0x4u /* ユーザーメモリの検証・写しの失敗 / a user-memory check or copy failed */
#define F20_FAULT_MMIO 0x8u /* bind_mmio が装置の窓の外 / bind_mmio outside every device window */

/* catch_fault から呼ばれた関数が渡すラッチ（コアごと。呼び出し元が直後に取り込む） / the latch handed back by functions called
 * from catch_fault (per core; the caller takes it right after the call) */
extern FASCON2_CPU_LOCAL uint32_t fascon__flt_pending;
/* ラッチ → errno（catch_fault と #[syscall] の Err に入る値） / latch → errno (the value in catch_fault / #[syscall] Errs) */
static inline FASCON_MAYBE_UNUSED int64_t f20_fault_errno(uint32_t flt) {
    return (flt & F20_FAULT_USER) ? 14 /* EFAULT */ : (flt & F20_FAULT_MMIO) ? 5 /* EIO */ : (flt & F20_FAULT_STALE) ? 116 /* ESTALE */ : (flt & F20_FAULT_ARITH) ? 75 /* EOVERFLOW */ : 22 /* EINVAL */;
}

/* `user Str`: レジスタ 2 本（ユーザーの仮想番地と長さ）。中身はカーネルから直接は読めない（copy_from_user を通す）
 * `user Str`: two registers (a user virtual address and a length); the kernel never reads it directly (copy_from_user) */
typedef struct {
    uint64_t addr;
    uint64_t len;
} f20_ustr_t;

/* ───── プラットフォーム層のフック（BSP が強い定義を持つ。無ければ実装節の弱い既定値） / platform hooks (BSPs give strong
 * definitions; otherwise the weak defaults of the implementation section apply) ───── */
/* ユーザーメモリの写し: 写せなかったバイト数を返す（0 = 成功）。generic-os の AArch64 では PAN = 1 のまま LDTRB / STTRB と例外表
 * user-memory copies: return the bytes NOT copied (0 = success); on generic-os AArch64: LDTRB / STTRB with PAN = 1 and an
 * exception table */
uint64_t fascon_copy_from_user(void *dst, uint64_t uaddr, uint64_t n);
uint64_t fascon_copy_to_user(uint64_t uaddr, const void *src, uint64_t n);
bool fascon_user_range_ok(uint64_t uaddr, uint64_t n);
/* dma own: 4 KiB 境界の独立した物理ページ（own ヒープとは別。EMBEDDED にしない） / own pages on 4 KiB boundaries */
void *fascon_dma_alloc(size_t n, const char *site);
void fascon_dma_free(void *p);
/* 貸し出し: キャッシュの掃き出し（DC CVAC）+ DSB SY、IOMMU（Arm SMMU / Intel VT-d）の IOVA を一時的に写して返す
 * lend: clean the cache (DC CVAC) + DSB SY, map an IOVA in the IOMMU (Arm SMMU / Intel VT-d) and return it */
uint64_t fascon_dma_submit(void *p, size_t n);
/* 回収: IOVA を検証して外し（IOMMU の写しも外す）、DC IVAC + DSB SY の後で値を返す（不正な IOVA は NULL = None）
 * reclaim: validate and unmap the IOVA (the IOMMU mapping too), DC IVAC + DSB SY, return the value (bad IOVA → NULL = None) */
void *fascon_dma_complete(uint64_t iova, size_t n);
/* bind_mmio: バス列挙が登録した装置の窓 [phys, phys + n) を高位の仮想番地の写しで返す。外れたら sink の窓 + *flt |= MMIO
 * bind_mmio: return the high virtual mirror of a device window [phys, phys + n) registered by bus enumeration; outside
 * every window, a sink window and *flt |= MMIO */
uintptr_t fascon_mmio_bind(uint64_t phys, size_t n, uint32_t *flt);
/* 装置の窓の登録（DTB / ACPI / PCIe の列挙が呼ぶ） / register a device window (called by DTB / ACPI / PCIe enumeration) */
void fascon_mmio_register_window(uint64_t phys, uint64_t size);
/* Sleep 効果の組み込み / builtins with the Sleep effect */
void fascon_sched_yield(void);
void fascon_sleep_ticks(uint64_t ticks);
/* 単調時計とコアの番号 / the monotonic clock and the core number */
uint64_t fascon_clock_mono_ns(void);
uint32_t fascon_cpu_this(void);

/* ───── ボード定義（--board の board.toml）: 装置の窓を main の最初に登録 / the board definition: device windows ───── */
#if defined(FASCON2_BOARD) && defined(FASCON_BOARD_DEVICES)
#define FASCON__BOARD_WIN(b, s) fascon_mmio_register_window((b), (s));
static inline FASCON_MAYBE_UNUSED void fascon_board_init(void) { FASCON_BOARD_DEVICES(FASCON__BOARD_WIN) }
#undef FASCON__BOARD_WIN
#endif

/* ───── RCU（エポック方式: 読み手はプリエンプション禁止 + 自分のエポックを公開、書き手は 2 エポック進むまで待つ）─────
 * RCU (epoch based: a reader disables preemption and publishes its epoch; a writer waits until the epoch advances twice) */
void fascon_rcu_read_lock(void);
void fascon_rcu_read_unlock(void);
void fascon_rcu_synchronize(void);
/* 猶予期間の後に fn(arg) を呼ぶ（generic-os のアリーナの解放の遅延キュー） / call fn(arg) after a grace period (the deferred
 * free queue of generic-os arenas) */
void fascon_rcu_defer(void (*fn)(uintptr_t), uintptr_t arg);
static inline void fascon__rcu_unlock_p(const int *x) {
    (void)x;
    fascon_rcu_read_unlock();
}
#define F20_RCU_READ(v) const int v __attribute__((cleanup(fascon__rcu_unlock_p), unused)) = (fascon_rcu_read_lock(), 0)

/* ───── 定数時間と秘密の値 / constant time and secret values ───── */
static inline FASCON_MAYBE_UNUSED bool fascon_ct_eq_u64(uint64_t a, uint64_t b) {
    uint64_t d = a ^ b;
    d |= d >> 32;
    d |= d >> 16;
    d |= d >> 8;
    d |= d >> 4;
    d |= d >> 2;
    d |= d >> 1;
    return (bool)((~d) & 1u);
}
static inline FASCON_MAYBE_UNUSED bool fascon_ct_memeq(const void *a, const void *b, size_t n) {
    const volatile uint8_t *x = (const volatile uint8_t *)a, *y = (const volatile uint8_t *)b;
    uint8_t d = 0u;
    for (size_t i = 0; i < n; i++) d = (uint8_t)(d | (x[i] ^ y[i]));
    return fascon_ct_eq_u64(d, 0u);
}
/* 揮発性の 0 埋め + メモリバリア（DSE で消えない: volatile の格納と、番地を入力に取る asm の両方） / volatile zeroing + a
 * memory barrier (never removed by dead-store elimination: volatile stores plus an asm that takes the address) */
static inline FASCON_MAYBE_UNUSED void fascon_secure_zero(void *p, size_t n) {
    volatile uint8_t *q = (volatile uint8_t *)p;
    for (size_t i = 0; i < n; i++) q[i] = 0u;
    __asm__ __volatile__("" : : "r"(p) : "memory");
    FASCON_HW_FENCE();
}

/* ───── Spectre v1: 外から来た添字のマスク（AArch64: CMP + SBC + CSDB、x86-64: CMP + SBB）─────
 * Spectre v1: masking of indices that came from outside (AArch64: CMP + SBC + CSDB, x86-64: CMP + SBB) */
static inline FASCON_MAYBE_UNUSED uint64_t f20_index_nospec(uint64_t i, uint64_t n) {
#if defined(__aarch64__)
    uint64_t m;
    __asm__ __volatile__("cmp %1, %2\n\tsbc %0, xzr, xzr\n\tcsdb" : "=r"(m) : "r"(i), "r"(n) : "cc");
    return i & m;
#elif defined(__x86_64__)
    uint64_t m;
    __asm__ __volatile__("cmp %2, %1\n\tsbb %0, %0" : "=r"(m) : "r"(i), "r"(n) : "cc");
    return i & m;
#else
    return i & (uint64_t)(0u - (uint64_t)(i < n));
#endif
}
/* 境界検査 + 推測実行でも範囲外を読まないマスク / a bounds check plus a mask that keeps speculation in range too */
static inline FASCON_MAYBE_UNUSED uintptr_t fascon_idx_u_ns(uint64_t v, uint64_t len, const char *site) {
    if (FASCON_UNLIKELY(v >= len)) fascon_bounds_panic(v, len, site);
    return (uintptr_t)f20_index_nospec(v, len);
}
static inline FASCON_MAYBE_UNUSED uintptr_t fascon_idx_s_ns(int64_t v, uint64_t len, const char *site) {
    if (FASCON_UNLIKELY(v < 0 || (uint64_t)v >= len)) fascon_bounds_panic((uint64_t)v, len, site);
    return (uintptr_t)f20_index_nospec((uint64_t)v, len);
}

/* ───── generic-os の MMIO: 固定番地の mmio と bind_mmio は高位の仮想番地の写しを通す（仮想 0〜2 MiB は守りの未マップ）─────
 * generic-os MMIO: fixed-address mmio and bind_mmio go through the high virtual mirror (virtual 0-2 MiB is an unmapped guard)
 *   FASCON2_MMIO_MIRROR は BSP が決める（qemu-virt-aarch64 の generic-os: 0x60_0000_0000）。PC 実行では 0
 *   FASCON2_MMIO_MIRROR is set by the BSP (generic-os on qemu-virt-aarch64: 0x60_0000_0000); 0 on PC runs */
#ifndef FASCON2_MMIO_MIRROR
#define FASCON2_MMIO_MIRROR UINT64_C(0)
#endif
#define F20_MMIO_VA(phys) ((uintptr_t)((uint64_t)(phys) + FASCON2_MMIO_MIRROR))

/* ───── PAC（ポインタ認証）: 関数ポインタを格納するときに PACIA、呼ぶ前に AUTIA（FEAT_FPAC なら失敗で即例外）─────
 * PAC (pointer authentication): PACIA when a function pointer is stored, AUTIA before the call (an immediate fault with
 * FEAT_FPAC). The 1716 forms are in the hint space, so cores without PAuth run them as NOPs */
#if defined(__aarch64__) && defined(FASCON2_GENERIC_OS) && !defined(FASCON2_HOSTED_MMIO)
static inline FASCON_MAYBE_UNUSED void *f20_pac_sign(void *p, uint64_t mod) {
    register uint64_t x17 __asm__("x17") = (uint64_t)(uintptr_t)p;
    register uint64_t x16 __asm__("x16") = mod;
    __asm__("hint #8" : "+r"(x17) : "r"(x16)); /* PACIA1716 */
    return (void *)(uintptr_t)x17;
}
static inline FASCON_MAYBE_UNUSED void *f20_pac_auth(void *p, uint64_t mod) {
    register uint64_t x17 __asm__("x17") = (uint64_t)(uintptr_t)p;
    register uint64_t x16 __asm__("x16") = mod;
    __asm__("hint #12" : "+r"(x17) : "r"(x16)); /* AUTIA1716 */
    return (void *)(uintptr_t)x17;
}
#define F20_BTI_C __attribute__((target("branch-protection=bti")))
#else
static inline FASCON_MAYBE_UNUSED void *f20_pac_sign(void *p, uint64_t mod) {
    (void)mod;
    return p;
}
static inline FASCON_MAYBE_UNUSED void *f20_pac_auth(void *p, uint64_t mod) {
    (void)mod;
    return p;
}
#define F20_BTI_C
#endif

/* ───── Per-CPU マガジン（generic-os の共有アリーナの O(1) ロックフリー確保・解放） ─────
 * per-CPU magazines (O(1) lock-free alloc / free for shared arenas on generic-os)
 *   alloc: 自分のコアのマガジンから取る（アトミック命令なし）→ 空なら大域の空きリストから半分まとめて補充（ビットロック）
 *          → それも空なら next_unused をアトミックに進め、2 MiB 境界を跨ぐときだけ要求元コアの NUMA ノードの 2 MiB ページを
 *          0 で写す（fascon_plat_arena_grow）
 *   free : 自分のコアのマガジンへ積む → 満杯なら半分を大域の空きリストへ返す
 *   alloc: take from this core's magazine (no atomics) → when empty, refill half a magazine from the global free list
 *          (bit lock) → when that is empty too, advance next_unused atomically, and only when it crosses a 2 MiB
 *          boundary map a zeroed 2 MiB page from the requesting core's NUMA node (fascon_plat_arena_grow)
 *   free : push onto this core's magazine → when full, give half back to the global free list */
#ifndef FASCON2_MAX_CPUS_MAG
#define FASCON2_MAX_CPUS_MAG 8u
#endif
#define F20_MAG_SIZE 32u
typedef struct F20_ALIGN128 {
    uint32_t n;
    uint32_t idx[F20_MAG_SIZE];
    uint8_t pad_[F20_DESTRUCTIVE_SIZE - ((4u + 4u * F20_MAG_SIZE) % F20_DESTRUCTIVE_SIZE)];
} f20_mag_t;
/* 仮想番地を予約したアリーナの伸長: [base, base + bytes) の 2 MiB ページを（まだなら）要求元コアのノードから 0 で写す
 * growing a VA-reserved arena: map the 2 MiB pages of [base, base + bytes) (if not yet) from the requesting core's node */
void fascon_plat_arena_grow(void *base, uint64_t bytes);
uint32_t fascon_numa_node_of(uint32_t cpu);
/* 期限の来た RCU のコールバックを流す（眠らない） / run the RCU callbacks that are due (never sleeps) */
uint32_t fascon_rcu_poll(void);


#if defined(FASCON2_GENERIC_OS)
/* ───── generic-os の共有アリーナ: Per-CPU マガジンの記述子と速い道（ロックもアトミックな比較交換もない）─────
 * generic-os shared arenas: the per-CPU magazine descriptor and the fast paths (no locks, no compare-and-swap)
 *   確保: 自分のコアのマガジンから添字を 1 つ取る（割り込みを止めるだけ）→ 値を書く → publish で生存にする（release）
 *   解放: retire で LIVE → PENDING（比較交換 1 回。勝った 1 人だけが進む）→ 生存ビットを消す → RCU の猶予期間の後に
 *         reclaim（中身の解放と 0 埋め、世代 +1、自分のマガジンへ）。猶予期間の間は rcu_read の読み手が古い中身を読める
 *   alloc: take an index from this core's magazine (interrupts masked only) → write the value → publish makes it live (release)
 *   free : retire turns LIVE into PENDING (one compare-and-swap; only the winner proceeds) → clear the live bit → after an
 *          RCU grace period, reclaim (drop and zero the contents, generation + 1, back to this core's magazine). During the
 *          grace period rcu_read readers can still read the old contents */
#define F20_PENDING 0xFFFFFFFEu
typedef struct {
    f20_arena_state_t *st;
    uint32_t *gen;
    uint32_t *link;
    uint64_t *live;
    uint32_t cap, K;
    uint8_t *base;      /* 仮想番地を予約したアリーナの先頭 / the start of the VA-reserved arena */
    uint64_t slot_off;  /* slot[] までの距離 / the offset of slot[] */
    uint64_t slot_size;
    uint64_t mapped;    /* 写した長さ（2 MiB 単位） / the mapped length (in 2 MiB) */
    volatile uint32_t lock;
    f20_mag_t mag[FASCON2_MAX_CPUS_MAG];
} f20_magd_t;
void f20_mag_refill(f20_magd_t *d, f20_mag_t *m);
void f20_mag_release(f20_magd_t *d, uint32_t i);
static inline FASCON_MAYBE_UNUSED f20_id_t f20_mag_acquire(f20_magd_t *d) {
    const uint64_t im = fascon_plat_irq_save();
    f20_mag_t *m = &d->mag[fascon_cpu_this() % FASCON2_MAX_CPUS_MAG];
    if (FASCON_UNLIKELY(m->n == 0u)) f20_mag_refill(d, m);
    if (FASCON_UNLIKELY(m->n == 0u)) {
        fascon_plat_irq_restore(im);
        return 0u; /* 満杯 / full */
    }
    const uint32_t i = m->idx[--m->n];
    fascon_plat_irq_restore(im);
    return F20_ID_MAKE(d->gen[i], i, d->K);
}
static inline FASCON_MAYBE_UNUSED void f20_mag_publish(f20_magd_t *d, uint32_t i) {
    __atomic_store_n(&d->link[i], F20_LIVE, __ATOMIC_RELEASE);
    (void)__atomic_fetch_or(&d->live[i >> 6], UINT64_C(1) << (i & 63u), __ATOMIC_RELEASE);
    (void)__atomic_fetch_add(&d->st->live, 1u, __ATOMIC_RELAXED);
    (void)__atomic_fetch_add(&d->st->allocs, 1u, __ATOMIC_RELAXED);
}
static inline FASCON_MAYBE_UNUSED bool f20_mag_retire(f20_magd_t *d, uint32_t i) {
    uint32_t want = F20_LIVE;
    if (!__atomic_compare_exchange_n(&d->link[i], &want, F20_PENDING, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) return false;
    (void)__atomic_fetch_and(&d->live[i >> 6], ~(UINT64_C(1) << (i & 63u)), __ATOMIC_RELEASE);
    (void)__atomic_fetch_sub(&d->st->live, 1u, __ATOMIC_RELAXED);
    return true;
}
/* 仮想番地を予約したアリーナの置き場所（generic-os の BSP が FASCON2_GENOS_VA と基底を決める）
 * where VA-reserved arenas live (the generic-os BSP defines FASCON2_GENOS_VA and the base) */
#ifndef FASCON2_ARENA_VA_BASE
#define FASCON2_ARENA_VA_BASE UINT64_C(0x4000000000)
#endif
#define F20_ROUND_2M(x) ((((uint64_t)(x)) + (F20_HUGE_PAGE - 1u)) & ~(uint64_t)(F20_HUGE_PAGE - 1u))
#define F20_ARENA_VA(off) ((uintptr_t)(FASCON2_ARENA_VA_BASE + (uint64_t)(off)))

/* ───── プロセスのアドレス空間（ページの所有権の表）と TLB の撃ち落とし ─────
 * process address spaces (page ownership tables) and TLB shootdown
 *   ページは own [u8; 4096]。map_page で所有権がアドレス空間へ移り、unmap_page で戻る。share_page は同じページを
 *   読み取り専用で 2 つの空間へ写し（Copy-on-Write）、参照の数を数える。書き込みのフォールトで #[trap] が cow_copy で
 *   新しい own ページへ写し、map_page で差し替える（古い共有ページは参照が 1 つ減る）。PTE の書き換えのたびに
 *   tlb_flush が cpu_mask を見て、自分のコアだけならローカルの TLBI（tlbi vale1 + dsb nsh + isb）、ほかのコアの
 *   ビットがあればそのコアへだけ IPI を送る
 *   pages are own [u8; 4096]. map_page moves the ownership into the address space and unmap_page gives it back.
 *   share_page maps the same page read-only into two spaces (Copy-on-Write) and counts references. On a write fault the
 *   #[trap] copies with cow_copy into a fresh own page and swaps it in with map_page (the old shared page loses one
 *   reference). Every PTE change is followed by tlb_flush, which looks at cpu_mask: a local TLBI (tlbi vale1 + dsb nsh
 *   + isb) when only this core runs the process, IPIs only to the cores whose bits are set otherwise */
#ifndef FASCON2_ASPACE_PAGES
#define FASCON2_ASPACE_PAGES 32u
#endif
#define F20_PG_PRESENT 1u
#define F20_PG_WRITE 2u
#define F20_PG_COW 4u
typedef struct {
    uint64_t va[FASCON2_ASPACE_PAGES];
    uint8_t *page[FASCON2_ASPACE_PAGES];
    uint8_t flags[FASCON2_ASPACE_PAGES];
} f20_aspace_t;
int64_t f20_aspace_map(f20_aspace_t *as, uint64_t asid, uint64_t va, uint8_t *page, bool writable);
int64_t f20_aspace_share(f20_aspace_t *src, uint64_t asid_src, uint64_t va, f20_aspace_t *dst, uint64_t asid_dst, uint64_t src_mask);
uint8_t *f20_aspace_cow_copy(f20_aspace_t *as, uint64_t va, const char *site);
uint8_t *f20_aspace_unmap(f20_aspace_t *as, uint64_t asid, uint64_t va);
uint8_t f20_aspace_peek(f20_aspace_t *as, uint64_t va, uint64_t off);
void f20_aspace_drop(f20_aspace_t *as, uint64_t asid);
/* BSP のフック: PTE を書く（PC 実行では何もしない）と、cpu_mask による TLB の撃ち落とし
 * BSP hooks: write a PTE (nothing on PC runs) and the TLB shootdown driven by cpu_mask */
void fascon_plat_pte_set(uint64_t asid, uint64_t va, const void *page, uint32_t flags);
void fascon_tlb_flush_va(uint64_t asid, uint64_t va, uint64_t cpu_mask);
uint64_t fascon_tlb_local_count(void);
uint64_t fascon_tlb_ipi_count(void);
#endif /* FASCON2_GENERIC_OS */
#endif /* FASCON2_UNIVERSAL */

#ifdef __cplusplus
}
#endif

/* ═════════════ fascon-gc（gc_arena: 決定論的・アリーナ連動型の正確な GC） / fascon-gc (gc_arena: a deterministic, arena-coupled precise GC) ═════════════
 *
 *   gc_arena を使うプログラムだけが FASCON2_GC を定義する（--gc=off や gc_arena の無いプログラムには 1 バイトも入らない）。
 *   only programs that use gc_arena define FASCON2_GC (with --gc=off or without gc_arena not a single byte of this is linked).
 *
 *   根（ルート）: 1. 静的変数 2. ふつうの arena / task_arena / bump_arena の生きているスロット 3. シャドウスタック（gc_arena の
 *   Id を持つローカル・引数。関数ごとの枠を cleanup 属性で外す）4. 止まっているタスクのシャドウスタック（BSP のフック）
 *   roots: 1. statics 2. live slots of ordinary arena / task_arena / bump_arena 3. the shadow stack (locals and parameters that
 *   hold gc_arena Ids; one frame per function, popped by the cleanup attribute) 4. shadow stacks of suspended tasks (a BSP hook)
 *
 *   印付け: 生ポインタが無いので、どのバイトが Id かは型記述子（.rodata）が正確に知っている（保守的 GC の誤認は起きない）。
 *   要素の Id がすべて単純なフィールドなら、スロット本体ではなく「Id 専用の射影列」だけを読む。印は mark[] のビット、
 *   掃除は live & ~mark を ctz / blsr で 1 ビットずつ取り出し、そのアリーナの free（own の drop・0 埋め・世代 +1・列の
 *   unmirror・空きリスト / マガジン。共有アリーナは RCU の猶予期間の後）へ渡す
 *   marking: with no raw pointers, the type descriptors (.rodata) know exactly which bytes are Ids (no conservative
 *   misidentification). When every Id of the element is a plain field, only the "Id-only projection columns" are read,
 *   not the slot bodies. Marks are bits in mark[]; the sweep takes live & ~mark one bit at a time with ctz / blsr and hands
 *   each slot to the arena's own free (drop of own fields, zero fill, generation + 1, column unmirror, free list / magazine;
 *   for shared arenas after an RCU grace period) */
#if defined(FASCON2_GC)
typedef struct f20_type_desc f20_type_desc_t;
typedef struct {
    uint32_t off;                /* 値の先頭からの位置 / byte offset in the value */
    uint8_t kind;                /* F20_GC_* */
    uint8_t heap;                /* F20_GC_ID: 指す gc_arena の番号 / the gc arena it points into */
    uint16_t count;              /* F20_GC_ARR: 要素数 / number of elements */
    uint32_t stride;             /* F20_GC_ARR: 要素の大きさ / element size */
    uint32_t tag;                /* F20_GC_OPT: is_some の位置 / offset of is_some */
    const f20_type_desc_t *desc; /* F20_GC_OWN / ARR / OPT: 中身の記述子 / descriptor of what is inside */
} f20_gc_field_t;
struct f20_type_desc {
    uint32_t size;
    uint32_t nfields;
    const f20_gc_field_t *fields;
};
#define F20_GC_ID 1u  /* gc_arena の Id（Option<Id> も同じ表現: 0 = None） / a gc_arena Id (Option<Id> has the same form: 0 = None) */
#define F20_GC_OWN 2u /* own T（NULL 可）→ 指す先を記述子で / own T (may be NULL) → the target by its descriptor */
#define F20_GC_ARR 3u /* 固定長配列 / a fixed-size array */
#define F20_GC_OPT 4u /* Option<構造体>（is_some が真のときだけ） / Option<struct> (only when is_some) */

/* シャドウスタックの枠（gc_arena に触れる関数ごと） / a shadow-stack frame (one per function that touches a gc_arena) */
typedef struct f20_gc_frame {
    struct f20_gc_frame *prev;
    uint32_t n;
    uint32_t cap;
    void *const *addr_unused_; /* 予備（ABI を固定） / reserved (keeps the layout fixed) */
    void **addr;
    const f20_type_desc_t **desc;
} f20_gc_frame_t;

/* 1 つの gc_arena（共有でなければコアごとに 1 つ） / one gc_arena (per core unless shared) */
typedef struct f20_gc_heap {
    const char *name;
    f20_arena_state_t *st;
    uint64_t *live;
    uint64_t *mark;
    void *gen;
    uint32_t *link;
    uint32_t gen_bytes;
    uint32_t k;
    uint32_t idx; /* fascon__gc_heap_at の番号 / its number in fascon__gc_heap_at */
    uint32_t sp;  /* 印付けの作業スタックの深さ / depth of the marking work stack */
    uint8_t *slot0; /* スロット i の値 = slot0 + i * stride / the value of slot i = slot0 + i * stride */
    uint32_t stride;
    uint32_t shared;
    const f20_type_desc_t *elem;
    uint32_t ncols; /* Id 専用の射影列（0 = スロットを記述子で読む） / Id-only projection columns (0 = read slots by descriptor) */
    uint32_t col_wide;
    const void *const *cols;
    const uint8_t *col_heap;
    bool (*free_fn)(uint32_t i);  /* 死んだスロットを解放（アリーナの free） / free a dead slot (the arena's free) */
    void (*remirror)(uint32_t i); /* 世代リフレッシュの後に列を写し直す / rewrite the columns after a generation refresh */
    uint32_t *work;               /* 印付けの作業スタック（2^K 個） / the marking work stack (2^K entries) */
    uint64_t collections, reclaimed, refreshes;
} f20_gc_heap_t;

extern F20_PERCPU f20_gc_frame_t *fascon__gc_top;
void f20_gc_pop(f20_gc_frame_t *f);
static inline void f20_gc_unroot(void ***p) { **p = NULL; }
void f20_gc_park(void);
void f20_gc_register_core(void);
extern volatile uint32_t fascon__gc_req;
extern F20_PERCPU uint32_t fascon__gc_reg;

/* 関数の入口: 枠を積む（どの出口でも cleanup が外す） / function entry: push a frame (cleanup pops it on every exit) */
#define F20_GC_FRAME(N)                                                                                   \
    void *_gcp[(N) + 1u] = {0};                                                                           \
    const f20_type_desc_t *_gcd[(N) + 1u];                                                                \
    f20_gc_frame_t _gcf __attribute__((cleanup(f20_gc_pop))) = {fascon__gc_top, 0u, (N), NULL, _gcp, _gcd}; \
    __atomic_store_n(&fascon__gc_top, &_gcf, __ATOMIC_RELAXED)
/* 根を 1 つ足す（k はこの関数の中で固定の番号。束縛の有効範囲が終わると cleanup が外す）
 * add one root (k is a fixed number within this function; cleanup removes it when the binding's scope ends) */
#define F20_GC_ROOT(k, p, d)                                                   \
    _gcp[(k)] = (void *)(p);                                                   \
    _gcd[(k)] = (d);                                                           \
    if (_gcf.n <= (uint32_t)(k)) _gcf.n = (uint32_t)(k) + 1u;                  \
    void **_gcs##k __attribute__((cleanup(f20_gc_unroot), unused)) = &_gcp[(k)]
/* 安全点（共有の gc_arena に触れる関数の入口とループの後ろ向きの辺だけ。ほかのコア・関数は止まらない）
 * a safepoint (only at the entry and loop back-edges of functions that touch a shared gc_arena; other cores and functions
 * never stop) */
#define F20_GC_POLL()                                                                     \
    do {                                                                                  \
        if (FASCON_UNLIKELY(__atomic_load_n(&fascon__gc_req, __ATOMIC_ACQUIRE) & 1u)) f20_gc_park(); \
    } while (0)
/* 関数の入口の安全点: 枠を積んだ（登録した）ことと、回収中かを読むことの間に順序の柵を置く。回収するコアも gc_req を
 * 奇数にしてから柵を置いて枠を読むので、「こちらは回収に気づかず、回収するコアはこちらの枠に気づかない」は起きない
 * the safepoint at function entry: an ordering fence between pushing the frame (registering) and reading whether a
 * collection runs. The collecting core makes gc_req odd, fences, then reads the frames, so "this core misses the
 * collection and the collecting core misses this frame" cannot happen */
#define F20_GC_POLL_ENTRY()                                                               \
    do {                                                                                  \
        __atomic_thread_fence(__ATOMIC_SEQ_CST);                                          \
        F20_GC_POLL();                                                                    \
    } while (0)

/* チャネルのリングバッファに滞留している値を根として歩く: 先頭から末尾までの、発行済み（seq が「位置 + 1」）のセルだけ。
 * 受け手がちょうど取り出している途中のセルは、念のため根に数える（保守側: 生かしすぎても壊れない）
 * walk the values waiting in the ring buffer of a channel as roots: only published cells (seq == position + 1) between head
 * and tail. A cell a receiver is taking out right now still counts (the safe side: keeping too much alive breaks nothing) */
#define F20_CHAN_GC_WALK(name, CAP, d, visit, cx)                                                                       \
    do {                                                                                                               \
        const uint64_t h_ = __atomic_load_n(&chan_##name.head, __ATOMIC_ACQUIRE);                                        \
        const uint64_t t_ = __atomic_load_n(&chan_##name.tail, __ATOMIC_ACQUIRE);                                        \
        for (uint64_t p_ = h_; p_ != t_ && p_ - h_ < (uint64_t)(CAP); p_++) {                                           \
            const uint64_t i_ = p_ & ((uint64_t)(CAP) - 1u);                                                            \
            if (__atomic_load_n(&chan_##name.cell[i_].seq, __ATOMIC_ACQUIRE) + i_ == p_ + 1u)                           \
                (visit)((void *)&chan_##name.cell[i_].v, (d), (cx));                                                    \
        }                                                                                                              \
    } while (0)
/* gc_collect(a) の本体: 回収したスロット数 / the body of gc_collect(a): the number of slots reclaimed */
uint32_t f20_gc_collect(f20_gc_heap_t *h);
/* 生成コードが用意する: gc_arena（コアごとの実体は TLS）と、静的変数・ふつうのアリーナの根を回す関数
 * provided by the generated code: the gc arenas (per-core instances live in TLS) and the function that walks the static /
 * ordinary-arena roots */
f20_gc_heap_t *fascon__gc_heap_at(uint32_t i);
extern const uint32_t fascon__gc_nheaps;
void fascon__gc_roots(void (*visit)(void *p, const f20_type_desc_t *d, void *cx), void *cx, int writable);
/* BSP のフック: 止まっているタスクのシャドウスタックの先頭（既定 0 個） / BSP hook: shadow-stack tops of suspended tasks
 * (default: none) */
uint32_t fascon_plat_gc_task_tops(void **out, uint32_t max);
#endif /* FASCON2_GC */

/* ═════════════ 実装（生成 C の 1 ファイルだけで展開） / implementation (expanded in exactly one generated file) ═════════════ */
#ifdef FASCON2_RT_IMPLEMENTATION

#ifndef FASCON2_HEAP_SIZE
#define FASCON2_HEAP_SIZE (1u << 20) /* 既定 1 MiB（BSP は fascon_heap_init で差し替え可） / default 1 MiB */
#endif

#if defined(FASCON2_SMP)
static __thread uint32_t fascon__core;      /* いまのコアの番号（.tbss: 既定は 0） / the current core (.tbss: 0 by default) */
static __thread uint32_t fascon__wdepth, fascon__rdepth;
/* 生成 C がコアごとの表（タスクのスタック・ユーザーメモリ）を引く添字 / the index generated C uses for per-core tables
 * (task stacks, user memory) */
#define F20_CORE_INDEX() (fascon__core)
static int32_t fascon__rw;                   /* 0 = 空き、-1 = 書き手、n > 0 = 読み手 n 本 / 0 free, -1 a writer, n readers */
static uint32_t fascon__panicked;            /* パニックしたコア + 1 / the panicking core + 1 */
static uint32_t fascon__console;             /* コンソールを持つコア + 1 / the core holding the console + 1 */
/* パニックしたコアがコンソールの空きを待つ回数の上限（それを超えたら取り上げる） / how long a panicking core waits for the
 * console before taking it over */
#ifndef FASCON__CONSOLE_STEAL_SPINS
#define FASCON__CONSOLE_STEAL_SPINS 20000000u
#endif
/* 出力は 1 回の書き出しごとにコンソールのロックを取る（コアどうしの文字が混ざらない）。パニックしたコアは取ったまま止まる
 * every write takes the console lock (characters of different cores never interleave); a panicking core keeps it */
static void fascon__write(const char *s, size_t n);
#else
static void fascon__write(const char *s, size_t n) { fascon_plat_print(s, n); }
#endif

static void fascon__u64(char *buf, size_t *n, uint64_t v) {
    char t[24];
    int k = 0;
    do {
        t[k++] = (char)('0' + v % 10u);
        v /= 10u;
    } while (v);
    while (k) buf[(*n)++] = t[--k];
}

void fascon_put_u64(uint64_t v) {
    char b[24];
    size_t n = 0;
    fascon__u64(b, &n, v);
    fascon__write(b, n);
}
void fascon_put_i64(int64_t v) {
    char b[24];
    size_t n = 0;
    if (v < 0) {
        b[n++] = '-';
        fascon__u64(b, &n, (uint64_t)0 - (uint64_t)v);
    } else {
        fascon__u64(b, &n, (uint64_t)v);
    }
    fascon__write(b, n);
}
void fascon_put_bytes(const f20_slice_t *s) { fascon__write((const char *)s->p, (size_t)s->n); }
void fascon_put_hex(uint64_t v) {
    char b[18];
    size_t n = 0;
    b[n++] = '0';
    b[n++] = 'x';
    int started = 0;
    for (int i = 60; i >= 0; i -= 4) {
        unsigned d = (unsigned)((v >> i) & 0xFu);
        if (d || started || i == 0) {
            b[n++] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
            started = 1;
        }
    }
    fascon__write(b, n);
}
void fascon_print_i64(int64_t v) {
    fascon_put_i64(v);
    fascon__write("\n", 1);
}
void fascon_print_u64(uint64_t v) {
    fascon_put_u64(v);
    fascon__write("\n", 1);
}
void fascon_put_str(FasconStr s) { fascon__write(s.ptr, s.len); }
void fascon_print_str(FasconStr s) {
    fascon__write(s.ptr, s.len);
    fascon__write("\n", 1);
}

#if defined(FASCON2_FLOAT)
/* ───── 浮動小数点の 10 進（往復できる最短の桁: Steele & White / Burger & Dybvig の自由形式、多倍長は 32 ビット語 40 個）
 * floats in decimal (the shortest digits that read back: Steele & White / Burger & Dybvig free-format printing with a
 * 40-limb 32-bit bignum). libc も浮動小数点の表も使わない（どの BSP でも同じ出力） / no libc and no float tables (the same
 * output on every BSP) ───── */
typedef struct {
    uint32_t n;     /* 使っている語の数 / limbs in use */
    uint32_t d[40]; /* 下位の語から / least significant first */
} f20_bn_t;

static void f20_bn_set(f20_bn_t *a, uint64_t v) {
    a->n = 0u;
    while (v) {
        a->d[a->n++] = (uint32_t)v;
        v >>= 32;
    }
}
static void f20_bn_mul(f20_bn_t *a, uint32_t m) {
    uint64_t c = 0u;
    for (uint32_t i = 0u; i < a->n; i++) {
        const uint64_t t = (uint64_t)a->d[i] * m + c;
        a->d[i] = (uint32_t)t;
        c = t >> 32;
    }
    if (c && a->n < 40u) a->d[a->n++] = (uint32_t)c;
}
static void f20_bn_shl(f20_bn_t *a, uint32_t s) {
    const uint32_t w = s / 32u, b = s % 32u;
    if (a->n == 0u) return;
    if (b) {
        uint32_t c = 0u;
        for (uint32_t i = 0u; i < a->n; i++) {
            const uint32_t x = a->d[i];
            a->d[i] = (x << b) | c;
            c = x >> (32u - b);
        }
        if (c && a->n < 40u) a->d[a->n++] = c;
    }
    if (w && a->n + w <= 40u) {
        for (uint32_t i = a->n; i-- > 0u;) a->d[i + w] = a->d[i];
        for (uint32_t i = 0u; i < w; i++) a->d[i] = 0u;
        a->n += w;
    }
}
static int f20_bn_cmp(const f20_bn_t *a, const f20_bn_t *b) {
    if (a->n != b->n) return a->n < b->n ? -1 : 1;
    for (uint32_t i = a->n; i-- > 0u;)
        if (a->d[i] != b->d[i]) return a->d[i] < b->d[i] ? -1 : 1;
    return 0;
}
static void f20_bn_add(f20_bn_t *o, const f20_bn_t *a, const f20_bn_t *b) {
    const uint32_t n = a->n > b->n ? a->n : b->n;
    uint64_t c = 0u;
    for (uint32_t i = 0u; i < n; i++) {
        const uint64_t t = c + (i < a->n ? a->d[i] : 0u) + (i < b->n ? b->d[i] : 0u);
        o->d[i] = (uint32_t)t;
        c = t >> 32;
    }
    o->n = n;
    if (c && o->n < 40u) o->d[o->n++] = (uint32_t)c;
}
/* a -= b（a >= b） / a -= b (a >= b) */
static void f20_bn_sub(f20_bn_t *a, const f20_bn_t *b) {
    uint64_t borrow = 0u;
    for (uint32_t i = 0u; i < a->n; i++) {
        const uint64_t x = a->d[i], y = (i < b->n ? b->d[i] : 0u) + borrow;
        a->d[i] = (uint32_t)(x - y);
        borrow = x < y;
    }
    while (a->n && a->d[a->n - 1u] == 0u) a->n--;
}
static void f20_bn_pow10(f20_bn_t *a, uint32_t k) {
    static const uint32_t p10[10] = {1u, 10u, 100u, 1000u, 10000u, 100000u, 1000000u, 10000000u, 100000000u, 1000000000u};
    while (k >= 9u) {
        f20_bn_mul(a, 1000000000u);
        k -= 9u;
    }
    f20_bn_mul(a, p10[k]);
}

/* v = f × 2^e（0 < v、有限、prec = 仮数のビット数、min_e = 非正規数の指数）を、v に最も近い最短の数字列 digs と指数 k
 * （v ≈ 0.digs × 10^k）に。仮数が偶数なら区間の端を含む（IEEE の偶数丸め） / turn v = f × 2^e (0 < v, finite, prec =
 * mantissa bits, min_e = the subnormal exponent) into the shortest digits digs nearest v and the exponent k (v ≈ 0.digs ×
 * 10^k); an even mantissa includes the interval ends (IEEE round-half-even) */
static uint32_t f20_shortest(uint64_t f, int32_t e, uint32_t prec, int32_t min_e, char *digs, int32_t *kout) {
    f20_bn_t r, s, mp, mm, t;
    const int lower_closer = f == (UINT64_C(1) << (prec - 1u)) && e > min_e;
    const int even = (f & 1u) == 0u;
    if (e >= 0) {
        f20_bn_set(&r, f);
        f20_bn_shl(&r, (uint32_t)e + (lower_closer ? 2u : 1u));
        f20_bn_set(&s, lower_closer ? 4u : 2u);
        f20_bn_set(&mp, 1u);
        f20_bn_shl(&mp, (uint32_t)e + (lower_closer ? 1u : 0u));
        f20_bn_set(&mm, 1u);
        f20_bn_shl(&mm, (uint32_t)e);
    } else {
        f20_bn_set(&r, f);
        f20_bn_shl(&r, lower_closer ? 2u : 1u);
        f20_bn_set(&s, 1u);
        f20_bn_shl(&s, (uint32_t)(-e) + (lower_closer ? 2u : 1u));
        f20_bn_set(&mp, lower_closer ? 2u : 1u);
        f20_bn_set(&mm, 1u);
    }
    /* k の見積もり（正しいか 1 小さい） / estimate k (exact or one too small) */
    uint32_t bl = 0u;
    for (uint64_t x = f; x; x >>= 1) bl++;
    const double lg = (double)(e + (int32_t)bl - 1) * 0.30102999566398114 - 1e-10;
    int32_t k = (int32_t)lg;
    if (lg > 0.0 && (double)k < lg) k++;
    if (k >= 0) {
        f20_bn_pow10(&s, (uint32_t)k);
    } else {
        f20_bn_pow10(&r, (uint32_t)(-k));
        f20_bn_pow10(&mp, (uint32_t)(-k));
        f20_bn_pow10(&mm, (uint32_t)(-k));
    }
    f20_bn_add(&t, &r, &mp);
    const int c0 = f20_bn_cmp(&t, &s);
    if (even ? c0 >= 0 : c0 > 0) {
        f20_bn_mul(&s, 10u);
        k++;
    }
    uint32_t n = 0u;
    for (;;) {
        f20_bn_mul(&r, 10u);
        f20_bn_mul(&mp, 10u);
        f20_bn_mul(&mm, 10u);
        uint32_t d = 0u;
        while (f20_bn_cmp(&r, &s) >= 0) {
            f20_bn_sub(&r, &s);
            d++;
        }
        const int c1 = f20_bn_cmp(&r, &mm);
        const int low = even ? c1 <= 0 : c1 < 0;
        f20_bn_add(&t, &r, &mp);
        const int c2 = f20_bn_cmp(&t, &s);
        const int high = even ? c2 >= 0 : c2 > 0;
        if (!low && !high && n < 30u) {
            digs[n++] = (char)('0' + d);
            continue;
        }
        if (low && high) {
            /* どちらの桁も区間の中: v に近いほう。ちょうど真ん中なら偶数の桁（Ryu・Python の repr と同じ）
             * both digits are inside the interval: the one nearer v; an exact tie takes the even digit (as Ryu and
             * Python's repr do) */
            t = r;
            f20_bn_shl(&t, 1u);
            const int c3 = f20_bn_cmp(&t, &s);
            if (c3 > 0 || (c3 == 0 && (d & 1u))) d++;
        } else if (high) {
            d++;
        }
        digs[n++] = (char)('0' + d);
        break;
    }
    *kout = k;
    return n;
}

/* 数字列と指数を書く: 1e-4 <= |v| < 1e16 は小数（整数の値は `.0` 付き）、それ以外は `1.5e-7` の形
 * write the digits and exponent: 1e-4 <= |v| < 1e16 as a decimal (integral values with `.0`), others as `1.5e-7` */
static void fascon__put_fl(uint64_t f, int32_t e, uint32_t prec, int32_t min_e, int neg, int decimal) {
    char d[32];
    int32_t k = 0;
    const uint32_t n = f20_shortest(f, e, prec, min_e, d, &k);
    char b[80];
    size_t o = 0u;
    if (neg) b[o++] = '-';
    if (decimal) {
        if (k <= 0) {
            b[o++] = '0';
            b[o++] = '.';
            for (int32_t i = 0; i < -k; i++) b[o++] = '0';
            for (uint32_t i = 0u; i < n; i++) b[o++] = d[i];
        } else if ((uint32_t)k < n) {
            for (uint32_t i = 0u; i < (uint32_t)k; i++) b[o++] = d[i];
            b[o++] = '.';
            for (uint32_t i = (uint32_t)k; i < n; i++) b[o++] = d[i];
        } else {
            for (uint32_t i = 0u; i < n; i++) b[o++] = d[i];
            for (uint32_t i = n; i < (uint32_t)k; i++) b[o++] = '0';
            b[o++] = '.';
            b[o++] = '0';
        }
    } else {
        b[o++] = d[0];
        if (n > 1u) {
            b[o++] = '.';
            for (uint32_t i = 1u; i < n; i++) b[o++] = d[i];
        }
        b[o++] = 'e';
        int32_t x = k - 1;
        if (x < 0) {
            b[o++] = '-';
            x = -x;
        }
        fascon__u64(b, &o, (uint64_t)x);
    }
    fascon__write(b, o);
}

void fascon_put_f64(double v) {
    const uint64_t b = f20_f64_to_bits(v);
    const int neg = (int)(b >> 63);
    const uint32_t ex = (uint32_t)(b >> 52) & 0x7FFu;
    const uint64_t m = b & ((UINT64_C(1) << 52) - 1u);
    if (ex == 0x7FFu) {
        if (m) fascon__write("NaN", 3);
        else if (neg) fascon__write("-inf", 4);
        else fascon__write("inf", 3);
        return;
    }
    if (ex == 0u && m == 0u) {
        if (neg) fascon__write("-0.0", 4);
        else fascon__write("0.0", 3);
        return;
    }
    const double a = f20_f64_abs(v);
    fascon__put_fl(ex ? (m | (UINT64_C(1) << 52)) : m, ex ? (int32_t)ex - 1075 : -1074, 53u, -1074, neg, a >= 1e-4 && a < 1e16);
}

void fascon_put_f32(float v) {
    const uint32_t b = f20_f32_to_bits(v);
    const int neg = (int)(b >> 31);
    const uint32_t ex = (b >> 23) & 0xFFu;
    const uint32_t m = b & 0x7FFFFFu;
    if (ex == 0xFFu) {
        if (m) fascon__write("NaN", 3);
        else if (neg) fascon__write("-inf", 4);
        else fascon__write("inf", 3);
        return;
    }
    if (ex == 0u && m == 0u) {
        if (neg) fascon__write("-0.0", 4);
        else fascon__write("0.0", 3);
        return;
    }
    const float a = f20_f32_abs(v);
    fascon__put_fl(ex ? (uint64_t)(m | 0x800000u) : (uint64_t)m, ex ? (int32_t)ex - 150 : -149, 24u, -149, neg, a >= 1e-4f && a < 1e16f);
}
#endif

static size_t fascon__strlen(const char *s) {
    size_t n = 0;
    while (s && s[n]) n++;
    return n;
}

#if defined(FASCON2_SMP)
static void fascon__write(const char *s, size_t n) {
    const uint32_t me = fascon__core + 1u;
    if (__atomic_load_n(&fascon__console, __ATOMIC_RELAXED) == me) {
        fascon_plat_print(s, n); /* パニックの報告の途中 / in the middle of a panic report */
        return;
    }
    const uint64_t im = fascon_plat_irq_save();
    for (;;) {
        uint32_t c = 0u;
        if (__atomic_compare_exchange_n(&fascon__console, &c, me, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) break;
        fascon_smp_relax();
    }
    fascon_plat_print(s, n);
    __atomic_store_n(&fascon__console, 0u, __ATOMIC_RELEASE);
    fascon_plat_irq_restore(im);
}
#endif

FASCON_NORETURN void fascon_panic(uint32_t code, const char *msg, const char *site) {
#if defined(FASCON2_SMP)
    /* 最初にパニックしたコアだけが報告する。共有の旗を立て（ほかのコアは待ちループと出力の入口で止まる）、コンソールを
     * 取ったまま止まるので、報告にほかのコアの文字は混ざらない
     * only the first core to panic reports: it raises the shared flag (other cores stop at their waiting loops and at
     * the console) and keeps the console, so no other core's output mixes into the report */
    {
        uint32_t e = 0u;
        if (!__atomic_compare_exchange_n(&fascon__panicked, &e, fascon__core + 1u, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED) && e != fascon__core + 1u) {
            fascon_plat_core_parked();
        }
        /* ほかの全コアの割り込みと実行を止める（SGI / NMI）。止まらなかったコアも、待ちループと出力の入口で旗を見て止まる
         * stop interrupts and execution on every other core (SGI / NMI); a core that misses it still stops at its waiting
         * loops and the console, where it sees the flag */
        fascon_plat_panic_broadcast();
        /* コンソールを取る。NMI で止まったコア（x86_64: NMI は割り込みを止めた区間にも届く）がロックを持ったままかもしれない
         * ので、しばらく待っても空かなければ取り上げる（ほかのコアはもう止まっている。途中の行は残るが、報告は改行から始まる）
         * take the console. A core stopped by an NMI (x86_64: an NMI also reaches masked sections) may still hold the lock,
         * so it is taken over when it stays busy for a while (the other cores have stopped already; a partial line may remain,
         * and the report starts with a newline) */
        for (uint64_t spin = 0;; spin++) {
            uint32_t c = 0u;
            if (__atomic_compare_exchange_n(&fascon__console, &c, fascon__core + 1u, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED) || c == fascon__core + 1u) break;
            if (spin >= FASCON__CONSOLE_STEAL_SPINS) {
                __atomic_store_n(&fascon__console, fascon__core + 1u, __ATOMIC_SEQ_CST);
                break;
            }
            FASCON_CPU_RELAX();
        }
    }
#endif
    /* "msg (at site)" を 1 本の文字列にして BSP へ / join "msg (at site)" and hand it to the BSP */
    static char buf[256];
    size_t n = 0, m = fascon__strlen(msg), k = fascon__strlen(site);
    for (size_t i = 0; i < m && n < 200; i++) buf[n++] = msg[i];
    if (k) {
        const char *at = " (at ";
        for (size_t i = 0; at[i] && n < 240; i++) buf[n++] = at[i];
        for (size_t i = 0; i < k && n < 252; i++) buf[n++] = site[i];
        buf[n++] = ')';
    }
    buf[n] = 0;
    fascon_plat_kernel_panic(code, buf);
}

#ifndef FASCON_RT_H
FASCON_NORETURN void fascon_rt_arith_panic(const char *what) { fascon_panic(FASCON_PANIC_ARITH, what, ""); }
#endif

FASCON_NORETURN void fascon_bounds_panic(uint64_t index, uint64_t len, const char *site) {
    static char buf[96];
    size_t n = 0;
    const char *a = "index out of bounds: ";
    for (size_t i = 0; a[i]; i++) buf[n++] = a[i];
    fascon__u64(buf, &n, index);
    const char *b = " >= ";
    for (size_t i = 0; b[i]; i++) buf[n++] = b[i];
    fascon__u64(buf, &n, len);
    buf[n] = 0;
    fascon_panic(FASCON_PANIC_BOUNDS, buf, site);
}

FASCON_NORETURN void fascon_stale_id_panic(f20_id_t id, uint32_t k, const char *arena, const char *site) {
    static char buf[128];
    size_t n = 0;
    const char *a = "stale or freed Id for @";
    for (size_t i = 0; a[i]; i++) buf[n++] = a[i];
    for (size_t i = 0; arena[i] && n < 80; i++) buf[n++] = arena[i];
    const char *b = " (slot ";
    for (size_t i = 0; b[i]; i++) buf[n++] = b[i];
    fascon__u64(buf, &n, F20_ID_INDEX(id, k));
    const char *c = ", generation ";
    for (size_t i = 0; c[i]; i++) buf[n++] = c[i];
    fascon__u64(buf, &n, F20_ID_GEN(id, k));
    buf[n++] = ')';
    buf[n] = 0;
    fascon_panic(FASCON_PANIC_STALE_ID, buf, site);
}

FASCON_NORETURN void fascon_fault_stop(uint32_t flt, const char *site) {
    if (flt & F20_FAULT_STALE) fascon_panic(FASCON_PANIC_STALE_ID, "a stale Id was used with @a[id] (latched; stopped at the commit point)", site);
    if (flt & 0x4u) fascon_panic(FASCON_PANIC_BOUNDS, "a user-memory check or copy failed (latched; stopped at the commit point)", site);
    if (flt & 0x8u) fascon_panic(FASCON_PANIC_BOUNDS, "bind_mmio outside every registered device window (latched; stopped at the commit point)", site);
    fascon_panic(FASCON_PANIC_ARITH, "integer overflow in a sum over an arena (latched; stopped at the commit point)", site);
}

FASCON_NORETURN void fascon_unwrap_panic(const char *site) { fascon_panic(FASCON_PANIC_UNWRAP, "called unwrap() on None", site); }

/* ───── own ヒープ: サイズ別の空きリスト（32 B〜64 KiB の 12 段） / own heap: size-class free lists (12 classes, 32 B to 64 KiB) ───── */
#define FASCON__CLASSES 12u
typedef struct FasconFree {
    struct FasconFree *next;
} FasconFree;
typedef struct {
    uint8_t *bump, *end;
    FasconFree *free[FASCON__CLASSES];
    uintptr_t live_blocks, live_bytes, peak_bytes;
    int ready;
} FasconHeapState;
#if defined(FASCON2_SMP)
/* ───── マルチコア: コアごとの状態（128 バイト境界。コアどうしが同じキャッシュラインに乗らない）
 *       multi-core: per-core state (128-byte aligned, so no two cores share a cache line) ─────
 *   own ヒープの空きリストはコアごと。ブロックの見出しに「確保したコア」を書いておき、別のコアが解放したブロックは
 *   持ち主の返却リスト（remote_free。CAS で積むだけのロックフリーなスタック）へ積む。持ち主は次の確保のときに
 *   まとめて引き取る。自分のコアで確保して自分のコアで解放する道には、ロックもアトミック命令も無い
 *   （返却リストが空かどうかを 1 回読むだけ）。
 *   The own heap free lists are per core. The block header records the allocating core; a block freed by another core
 *   is pushed on its owner's return list (remote_free, a lock-free stack pushed with one CAS) and the owner collects the
 *   list at its next allocation. Allocating and freeing on the same core takes no lock and no atomic instruction
 *   (one plain read to see whether the return list is empty). */
/* --target hpc: コアの文脈は 1 つずつ別のページに置く（NUMA の固定はページ単位） / under --target hpc each core context gets
 * pages of its own (NUMA binding works on pages) */
#if defined(FASCON2_HPC)
#define F20_CPU_CTX_ALIGN __attribute__((aligned(4096)))
#else
#define F20_CPU_CTX_ALIGN F20_ALIGN128
#endif
typedef struct F20_CPU_CTX_ALIGN {
    uint32_t core_id;
    uint32_t online;
    FasconHeapState heap;
    FasconFree *remote_free;  /* ほかのコアが解放したブロック / blocks freed by other cores */
    uintptr_t remote_frees;   /* 引き取った数 / how many were collected */
    uintptr_t remote_pending; /* 積まれたまま、まだ引き取っていない数（アトミック） / pushed but not collected yet (atomic) */
    FasconFrame *frame;       /* このコアのフレームリージョン / this core's frame region */
    void *percpu;             /* このコアのアリーナの置き場（TLS ブロック） / where this core's arenas live (its TLS block) */
    f20_sched_t sched;        /* このコアのタスクの「いま」と実行キュー / current task and run queue of this core */
} f20_cpu_ctx_t;
f20_cpu_ctx_t fascon__cpu[FASCON2_MAX_CPUS];
f20_sched_t *fascon_cpu_sched(uint32_t core) { return &fascon__cpu[core % FASCON2_MAX_CPUS].sched; }
#define fascon__heap (fascon__cpu[fascon__core].heap)
#define FASCON__OWNER fascon__core
#if defined(FASCON2_HPC)
static _Alignas(4096) FASCON_MAYBE_UNUSED uint8_t fascon__default_heaps[FASCON2_MAX_CPUS][FASCON2_HEAP_SIZE];
#else
static _Alignas(16) FASCON_MAYBE_UNUSED uint8_t fascon__default_heaps[FASCON2_MAX_CPUS][FASCON2_HEAP_SIZE];
#endif
#define fascon__default_heap fascon__default_heaps[fascon__core]

void fascon_smp_core_init(uint32_t core) {
    if (core == 0u) {
        /* ほかのコアを起こす前に、全コアのスケジューラの記録を「タスクなし」にする / before other cores start, the scheduler
         * record of every core becomes "no task" */
        for (uint32_t c = 0; c < FASCON2_MAX_CPUS; c++) {
            f20_sched_t *q = &fascon__cpu[c].sched;
            if (!q->ready) {
                q->cur = q->pending = q->svc_to = -1;
                q->ready = 1u;
            }
        }
    }
    fascon__core = core;
    fascon__cpu[core].core_id = core;
    fascon__cpu[core].frame = &fascon__frame;
    fascon__cpu[core].percpu = (void *)&fascon__core;
    __atomic_store_n(&fascon__cpu[core].online, 1u, __ATOMIC_RELEASE);
}

void fascon_smp_relax(void) {
    const uint32_t p = __atomic_load_n(&fascon__panicked, __ATOMIC_RELAXED);
    if (FASCON_UNLIKELY(p != 0u && p != fascon__core + 1u)) {
        fascon_plat_core_parked(); /* ほかのコアがパニックした: ここで止まる / another core panicked: stop here */
    }
#if defined(FASCON2_GC)
    /* 待っている間（チャネル・錠）は安全点: 共有の gc_arena の回収中なら止まる / waiting (channels, locks) is a safepoint */
    if (FASCON_UNLIKELY(__atomic_load_n(&fascon__gc_req, __ATOMIC_ACQUIRE) & 1u)) f20_gc_park();
#endif
    FASCON_CPU_RELAX();
}

/* 閉じた区間 = ① 割り込み禁止 → ② 書き手のロック。出るときは逆順。入れ子は深さで数える（同じコアが持っていれば待たない）。
 * 深さはロックを取ってから数える: 取るのを待っている間はロックを持っていないので、共有の gc_arena の安全点で止まれる。
 * BSP は fascon_plat_critical_enter / exit を強い定義（チケットロック・WFE など）で置き換えてよい（順序は同じにすること）
 * a closed section = (1) interrupts masked, then (2) the writer lock; leaving goes in reverse. Nesting is counted (the
 * owning core never waits). The depth is counted after the lock is taken: while waiting for it the core holds no lock, so
 * it may park at a shared gc_arena safepoint. A BSP may replace fascon_plat_critical_enter / exit with strong definitions
 * (ticket locks, WFE, ...) as long as the order stays the same */
__attribute__((weak)) uint64_t fascon_plat_critical_enter(void) {
    const uint64_t im = fascon_plat_irq_save(); /* ① ローカルの割り込みを止める / mask local interrupts */
    if (fascon__wdepth == 0u) {
        for (;;) { /* ② スピンロック / the spinlock */
            int32_t e = 0;
            if (__atomic_compare_exchange_n(&fascon__rw, &e, -1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) break;
            fascon_smp_relax();
        }
    }
    fascon__wdepth++;
    return im;
}
__attribute__((weak)) void fascon_plat_critical_exit(uint64_t state) {
    if (--fascon__wdepth == 0u) __atomic_store_n(&fascon__rw, 0, __ATOMIC_RELEASE); /* ② を放す / release (2) */
    fascon_plat_irq_restore(state);                                                 /* ① を戻す / restore (1) */
}
uint64_t fascon_smp_write_enter(void) { return fascon_plat_critical_enter(); }
void fascon_smp_write_exit(uint64_t state) { fascon_plat_critical_exit(state); }
/* パニックの全コア停止の既定: 何もしない（ほかのコアは待ちループと出力の入口で止まる）。BSP が SGI / NMI で置き換える
 * the default all-core stop on panic does nothing (other cores stop at waiting loops and at the console); BSPs replace it
 * with an SGI / NMI */
__attribute__((weak)) void fascon_plat_panic_broadcast(void) {}
__attribute__((weak)) FASCON_NORETURN void fascon_plat_core_parked(void) {
    for (;;) FASCON_CPU_RELAX();
}
/* 並列走査 = 読み手。読み手どうしは同時に入れる。書き手の中から入るときは、もう持っているので待たない
 * a parallel scan = a reader; readers enter together. Entering from inside a writer section does not wait */
uint64_t fascon_smp_read_enter(void) {
    const uint64_t im = fascon_plat_irq_save();
    if (fascon__wdepth == 0u && fascon__rdepth == 0u) {
        for (;;) {
            int32_t s = __atomic_load_n(&fascon__rw, __ATOMIC_RELAXED);
            if (s >= 0 && __atomic_compare_exchange_n(&fascon__rw, &s, s + 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) break;
            fascon_smp_relax();
        }
    }
    fascon__rdepth++;
    return im;
}
void fascon_smp_read_exit(uint64_t state) {
    if (--fascon__rdepth == 0u && fascon__wdepth == 0u) (void)__atomic_fetch_sub(&fascon__rw, 1, __ATOMIC_RELEASE);
    fascon_plat_irq_restore(state);
}
#else
#define F20_CORE_INDEX() 0u
static f20_sched_t fascon__sched1 = {-1, -1, -1, 0, 0u, 0u, 0u, 0u, 1u, 0u, 0u, 0u, (void *)0};
f20_sched_t *fascon_cpu_sched(uint32_t core) {
    (void)core;
    return &fascon__sched1;
}
static FasconHeapState fascon__heap;
#define FASCON__OWNER 0u
static _Alignas(16) FASCON_MAYBE_UNUSED uint8_t fascon__default_heap[FASCON2_HEAP_SIZE];
/* 1 コアのプログラム: チャネルの待ちは CPU を休ませるだけ / a single-core program: channel waits just relax the CPU */
void fascon_smp_relax(void) { FASCON_CPU_RELAX(); }
#endif

void fascon_heap_init(void *base, size_t size) {
    uintptr_t b = ((uintptr_t)base + 15u) & ~(uintptr_t)15u;
    fascon__heap.bump = (uint8_t *)b;
    fascon__heap.end = (uint8_t *)base + size;
    for (unsigned i = 0; i < FASCON__CLASSES; i++) fascon__heap.free[i] = NULL;
    fascon__heap.live_blocks = fascon__heap.live_bytes = 0;
    fascon__heap.ready = 1;
}

#ifdef FASCON2_PREEMPT
/* 割り込み・タスクのあるプログラム: ヒープの確保と解放（どちらも O(1)）を閉じた区間にする。下の定義は *_raw になり、
 * 末尾の包みが割り込みを閉じてから呼ぶ
 * programs with interrupts or tasks: heap allocation and release (both O(1)) become closed sections; the definitions
 * below become *_raw and the wrappers at the end call them with interrupts masked */
void *fascon__own_alloc_raw(size_t payload, const char *site);
void fascon__own_free_raw(void *payload);
#define fascon_own_alloc fascon__own_alloc_raw
#define fascon_own_free fascon__own_free_raw
#endif
#ifdef FASCON2_HEAP_MALLOC
/* 検査用: 所有リージョンを malloc / free で確保する（AddressSanitizer で解放漏れ・二重解放を検出できる）
 * for testing: allocate own regions with malloc / free so that AddressSanitizer can detect leaks and double frees */
void *malloc(size_t);
void free(void *);
void *fascon_own_alloc(size_t payload, const char *site) {
    size_t need = FASCON_OWN_HDR + FASCON_ALIGN16(payload);
    uint8_t *blk = (uint8_t *)malloc(need);
    if (blk == NULL) fascon_panic(FASCON_PANIC_OOM, "malloc failed", site);
    FasconOwnHdr *h = (FasconOwnHdr *)blk;
    h->magic = FASCON_OWN_MAGIC;
    h->cls = 0u;
    h->flags = 0u;
    h->size = (uint32_t)need;
    h->pad = 0u;
    fascon__heap.live_blocks++;
    fascon__heap.live_bytes += need;
    return blk + FASCON_OWN_HDR;
}
void fascon_own_free(void *payload) {
    if (payload == NULL) return;
    FasconOwnHdr *h = (FasconOwnHdr *)((uint8_t *)payload - FASCON_OWN_HDR);
    if (FASCON_UNLIKELY(h->magic != FASCON_OWN_MAGIC)) fascon_panic(FASCON_PANIC_OWN_CORRUPT, "own region header corrupted or freed twice", "");
    if (h->cls == FASCON_OWN_EMBEDDED || h->cls == FASCON_OWN_FRAME) return; /* 親と一緒・フレームと一緒 / with the parent or the frame */
    h->magic = FASCON_OWN_DEAD;
    fascon__heap.live_blocks--;
    fascon__heap.live_bytes -= h->size;
    free(h);
}
#else
void *fascon_own_alloc(size_t payload, const char *site) {
    if (FASCON_UNLIKELY(!fascon__heap.ready)) fascon_heap_init(fascon__default_heap, sizeof fascon__default_heap);
    size_t need = FASCON_OWN_HDR + FASCON_ALIGN16(payload);
    unsigned c = 0;
    size_t bs = 32u;
    while (bs < need && c < FASCON__CLASSES) {
        bs <<= 1;
        c++;
    }
    if (FASCON_UNLIKELY(c >= FASCON__CLASSES)) fascon_panic(FASCON_PANIC_TOO_LARGE, "own region larger than 64 KiB", site);
    uint8_t *blk;
#if defined(FASCON2_SMP)
    /* ほかのコアが解放したブロックを引き取る（空なら読み出し 1 回だけ） / collect blocks freed by other cores (one read when empty) */
    if (FASCON_UNLIKELY(fascon__cpu[fascon__core].remote_free != NULL)) {
        FasconFree *l = __atomic_exchange_n(&fascon__cpu[fascon__core].remote_free, NULL, __ATOMIC_ACQUIRE);
        while (l) {
            FasconFree *nx = l->next;
            FasconOwnHdr *rh = (FasconOwnHdr *)((uint8_t *)l - FASCON_OWN_HDR);
            fascon__heap.live_blocks--;
            fascon__heap.live_bytes -= rh->size;
            fascon__cpu[fascon__core].remote_frees++;
            (void)__atomic_fetch_sub(&fascon__cpu[fascon__core].remote_pending, 1u, __ATOMIC_RELAXED);
            FasconFree *rf = (FasconFree *)rh;
            const unsigned rc = rh->cls;
            rf->next = fascon__heap.free[rc];
            fascon__heap.free[rc] = rf;
            l = nx;
        }
    }
#endif
    if (fascon__heap.free[c]) {
        blk = (uint8_t *)fascon__heap.free[c];
        fascon__heap.free[c] = fascon__heap.free[c]->next;
    } else {
        if (FASCON_UNLIKELY((size_t)(fascon__heap.end - fascon__heap.bump) < bs)) fascon_panic(FASCON_PANIC_OOM, "own heap exhausted", site);
        blk = fascon__heap.bump;
        fascon__heap.bump += bs;
    }
    FasconOwnHdr *h = (FasconOwnHdr *)blk;
    h->magic = FASCON_OWN_MAGIC;
    h->cls = (uint16_t)c;
    h->flags = 0u;
    h->size = (uint32_t)bs;
    h->pad = FASCON__OWNER; /* 確保したコア / the allocating core */
    fascon__heap.live_blocks++;
    fascon__heap.live_bytes += bs;
    if (fascon__heap.live_bytes > fascon__heap.peak_bytes) fascon__heap.peak_bytes = fascon__heap.live_bytes;
    return blk + FASCON_OWN_HDR;
}

void fascon_own_free(void *payload) {
    if (payload == NULL) return;
    FasconOwnHdr *h = (FasconOwnHdr *)((uint8_t *)payload - FASCON_OWN_HDR);
    if (FASCON_UNLIKELY(h->magic != FASCON_OWN_MAGIC)) fascon_panic(FASCON_PANIC_OWN_CORRUPT, "own region header corrupted or freed twice", "");
    if (h->cls == FASCON_OWN_EMBEDDED) return; /* 親のブロックに詰めた子: 親と一緒に解放 / packed child: freed with its parent */
    if (h->cls == FASCON_OWN_FRAME) return;    /* フレームのブロック: スコープの終わりに印まで戻す / frame block: released by its mark */
    unsigned c = h->cls;
    h->magic = FASCON_OWN_DEAD;
#if defined(FASCON2_SMP)
    if (FASCON_UNLIKELY(h->pad != fascon__core)) {
        /* 別のコアが確保したブロック: 持ち主の返却リストへ積む（CAS 1 回。持ち主の空きリストには触らない）
         * a block allocated by another core: push it on the owner's return list (one CAS; the owner's free lists are
         * never touched from here) */
        FasconFree *node = (FasconFree *)payload;
        f20_cpu_ctx_t *o = &fascon__cpu[h->pad % FASCON2_MAX_CPUS];
        FasconFree *old = __atomic_load_n(&o->remote_free, __ATOMIC_RELAXED);
        do {
            node->next = old;
        } while (!__atomic_compare_exchange_n(&o->remote_free, &old, node, true, __ATOMIC_RELEASE, __ATOMIC_RELAXED));
        (void)__atomic_fetch_add(&o->remote_pending, 1u, __ATOMIC_RELAXED);
        return;
    }
#endif
    fascon__heap.live_blocks--;
    fascon__heap.live_bytes -= h->size;
    FasconFree *f = (FasconFree *)h;
    f->next = fascon__heap.free[c];
    fascon__heap.free[c] = f;
}
#endif /* FASCON2_HEAP_MALLOC */
#ifdef FASCON2_PREEMPT
#undef fascon_own_alloc
#undef fascon_own_free
void *fascon_own_alloc(size_t payload, const char *site) {
    const uint64_t im = fascon_plat_irq_save();
    void *p = fascon__own_alloc_raw(payload, site);
    fascon_plat_irq_restore(im);
    return p;
}
void fascon_own_free(void *payload) {
    const uint64_t im = fascon_plat_irq_save();
    fascon__own_free_raw(payload);
    fascon_plat_irq_restore(im);
}
#endif

#if defined(FASCON2_SMP)
/* 全コアの合計。返却リストに積まれたままのブロック（もう解放されたが、持ち主がまだ引き取っていない）は数えない。
 * ほかのコアが動いている間に読むと、途中の値になりうる（検査用の数）
 * the total over every core, not counting blocks still on a return list (already freed, not yet collected by their
 * owner). Read while other cores run, it may be a value in flux (a number for checks) */
uintptr_t fascon_heap_live_blocks(void) {
    uintptr_t n = 0;
    for (uint32_t i = 0; i < FASCON2_MAX_CPUS; i++) n += fascon__cpu[i].heap.live_blocks - __atomic_load_n(&fascon__cpu[i].remote_pending, __ATOMIC_RELAXED);
    return n;
}
uintptr_t fascon_heap_live_bytes(void) {
    uintptr_t n = 0;
    for (uint32_t i = 0; i < FASCON2_MAX_CPUS; i++) n += fascon__cpu[i].heap.live_bytes;
    return n;
}
#else
uintptr_t fascon_heap_live_blocks(void) { return fascon__heap.live_blocks; }
uintptr_t fascon_heap_live_bytes(void) { return fascon__heap.live_bytes; }
#endif

/* ───── ユーザー空間の既定（BSP が強い定義で置き換える）/ user-space defaults (BSPs replace them with strong definitions) ───── */
__attribute__((weak)) bool fascon_plat_user_window(uint64_t addr, uint64_t len, f20_slice_t *out) {
    (void)addr;
    (void)len;
    (void)out;
    return false;
}
#if defined(FASCON2_SMP)
/* 既定の写し: 検証済みの窓（恒等写像側の番地）から 1 バイトずつ volatile で 1 回だけ読む。BSP は LDTRB / STTRB と例外表、
 * SMAP の STAC / CLAC などハードウェアの境界検査つきの強い定義で置き換える
 * the default copies read each byte once through a validated window (its identity-map address) with volatile accesses;
 * BSPs replace them with strong definitions that check the boundary in hardware (LDTRB / STTRB + exception table, SMAP) */
__attribute__((weak)) uint64_t fascon_plat_copy_from_user(void *dst, uint64_t uaddr, uint64_t n) {
    f20_slice_t w;
    if (!fascon_plat_user_window(uaddr, n, &w)) return n;
    for (uint64_t i = 0; i < n; i++) ((uint8_t *)dst)[i] = ((volatile uint8_t *)w.p)[i];
    return 0u;
}
__attribute__((weak)) uint64_t fascon_plat_copy_to_user(uint64_t uaddr, const void *src, uint64_t n) {
    f20_slice_t w;
    if (!fascon_plat_user_window(uaddr, n, &w)) return n;
    for (uint64_t i = 0; i < n; i++) ((volatile uint8_t *)w.p)[i] = ((const uint8_t *)src)[i];
    return 0u;
}
#endif

/* ───── フレームリージョン（既定の置き場。BSP は fascon_frame_init で専用の領域に差し替える） / frame region ───── */
static F20_ALIGN128 FASCON_MAYBE_UNUSED uint8_t fascon__default_frame[FASCON2_FRAME_SIZE];
#if defined(FASCON2_SMP)
/* コアごと（.tbss）。プラットフォーム層がコアの起動時に fascon_frame_init で置き場を渡す。渡されるまではヒープへ回る
 * per core (.tbss); the platform layer hands over the region with fascon_frame_init when a core starts, and until then
 * allocations fall back to the heap */
FASCON2_CPU_LOCAL FasconFrame fascon__frame;
#else
FASCON2_CPU_LOCAL FasconFrame fascon__frame = {fascon__default_frame, fascon__default_frame + FASCON2_FRAME_SIZE, fascon__default_frame, 0u, {0}};
#endif

void fascon_frame_init(void *base, size_t size) {
    uintptr_t b = ((uintptr_t)base + 15u) & ~(uintptr_t)15u;
    fascon__frame.base = fascon__frame.top = (uint8_t *)b;
    fascon__frame.end = (uint8_t *)base + size;
    fascon__frame.peak = 0u;
}

/* いちばん深くまで使った量（解放の時点で記録） / the deepest use (recorded at each release) */
uintptr_t fascon_frame_peak_bytes(void) {
    uintptr_t now = (uintptr_t)(fascon__frame.top - fascon__frame.base);
    return now > fascon__frame.peak ? now : fascon__frame.peak;
}


#if defined(FASCON2_UNIVERSAL)
/* ═════════════ ユニバーサル OS 拡張の実装 / implementation of the universal-OS extension ═════════════ */
FASCON2_CPU_LOCAL uint32_t fascon__flt_pending;

/* ───── 弱い既定値（BSP・PC のプラットフォーム層が強い定義で置き換える） / weak defaults (replaced by the strong
 * definitions of the BSP or the PC platform layer) ───── */
__attribute__((weak)) uint64_t fascon_copy_from_user(void *dst, uint64_t uaddr, uint64_t n) {
    /* 今までの 1.0 の BSP: ユーザーメモリの窓（恒等写像側の番地）を通して写す / the 1.0 BSPs: copy through the user window */
    f20_slice_t w;
    if (!fascon_plat_user_window(uaddr, n, &w)) return n;
    for (uint64_t i = 0; i < n; i++) ((uint8_t *)dst)[i] = ((volatile uint8_t *)w.p)[i];
    return 0u;
}
__attribute__((weak)) uint64_t fascon_copy_to_user(uint64_t uaddr, const void *src, uint64_t n) {
    f20_slice_t w;
    if (!fascon_plat_user_window(uaddr, n, &w)) return n;
    for (uint64_t i = 0; i < n; i++) ((volatile uint8_t *)w.p)[i] = ((const uint8_t *)src)[i];
    return 0u;
}
__attribute__((weak)) bool fascon_user_range_ok(uint64_t uaddr, uint64_t n) {
    f20_slice_t w;
    return fascon_plat_user_window(uaddr, n, &w);
}

/* DMA プール: 4 KiB のページを単位に、ページごとの「使用中・貸し出し中」の印で管理（O(n) の探索。BSP は物理ページの
 * 割り当て器と IOMMU で置き換える） / the DMA pool: managed in 4 KiB pages with an in-use / lent mark per page (an O(n)
 * search; BSPs replace it with their physical page allocator and IOMMU) */
#ifndef FASCON2_DMA_POOL_PAGES
#define FASCON2_DMA_POOL_PAGES 64u
#endif
#if defined(__ELF__)
#define F20_DMA_SECTION __attribute__((section(".bss.fascon_dma")))
#else
#define F20_DMA_SECTION
#endif
static uint8_t fascon__dma_pool[FASCON2_DMA_POOL_PAGES][4096] __attribute__((aligned(4096))) F20_DMA_SECTION;
static uint8_t fascon__dma_used[FASCON2_DMA_POOL_PAGES]; /* 0 空き / 1 先頭 / 2 続き / 0 free, 1 head, 2 tail */
static uint8_t fascon__dma_lent[FASCON2_DMA_POOL_PAGES];
__attribute__((weak)) void *fascon_dma_alloc(size_t n, const char *site) {
    const uint32_t pages = (uint32_t)((n + 4095u) / 4096u);
    const uint64_t im = fascon_plat_irq_save();
    for (uint32_t i = 0; i + pages <= FASCON2_DMA_POOL_PAGES; i++) {
        uint32_t k = 0;
        while (k < pages && fascon__dma_used[i + k] == 0u) k++;
        if (k == pages) {
            for (k = 0; k < pages; k++) fascon__dma_used[i + k] = k == 0 ? 1u : 2u;
            fascon_plat_irq_restore(im);
            for (uint64_t b = 0; b < (uint64_t)pages * 4096u; b++) ((volatile uint8_t *)fascon__dma_pool[i])[b] = 0u;
            return fascon__dma_pool[i];
        }
        i += k;
    }
    fascon_plat_irq_restore(im);
    fascon_panic(FASCON_PANIC_OOM, "the DMA pool is exhausted (dma_alloc)", site);
}
static int64_t fascon__dma_page(const void *p) {
    const uintptr_t a = (uintptr_t)p, b = (uintptr_t)fascon__dma_pool;
    if (a < b || a >= b + sizeof fascon__dma_pool || ((a - b) & 4095u) != 0u) return -1;
    return (int64_t)((a - b) / 4096u);
}
__attribute__((weak)) void fascon_dma_free(void *p) {
    if (p == NULL) return;
    const int64_t i = fascon__dma_page(p);
    if (i < 0 || fascon__dma_used[i] != 1u || fascon__dma_lent[i]) fascon_panic(FASCON_PANIC_OWN_CORRUPT, "dma own freed twice or while lent", "");
    const uint64_t im = fascon_plat_irq_save();
    fascon__dma_used[i] = 0u;
    for (int64_t k = i + 1; k < (int64_t)FASCON2_DMA_POOL_PAGES && fascon__dma_used[k] == 2u; k++) fascon__dma_used[k] = 0u;
    fascon_plat_irq_restore(im);
}
__attribute__((weak)) uint64_t fascon_dma_submit(void *p, size_t n) {
    const int64_t i = fascon__dma_page(p);
    if (i < 0) fascon_panic(FASCON_PANIC_OWN_CORRUPT, "submit_dma: not a dma own buffer", "");
    fascon_plat_dma_sync(p, n, 1);
    fascon__dma_lent[i] = 1u;
    /* IOMMU の無い既定値: IOVA = 番地（board.toml の [dma] bus_offset があれば装置から見た番地 = bus_addr）
     * the default without an IOMMU: IOVA = address (plus [dma] bus_offset of board.toml = the address the device sees) */
#if defined(FASCON2_DMA_BUS_OFFSET)
    return (uint64_t)(uintptr_t)p + FASCON2_DMA_BUS_OFFSET;
#else
    return (uint64_t)(uintptr_t)p;
#endif
}
__attribute__((weak)) void *fascon_dma_complete(uint64_t iova, size_t n) {
#if defined(FASCON2_DMA_BUS_OFFSET)
    void *p = (void *)(uintptr_t)(iova - FASCON2_DMA_BUS_OFFSET);
#else
    void *p = (void *)(uintptr_t)iova;
#endif
    const int64_t i = fascon__dma_page(p);
    if (i < 0 || !fascon__dma_lent[i] || fascon__dma_used[i] != 1u) return NULL; /* 偽の IOVA・二重の完了 / forged IOVA or double completion */
    fascon__dma_lent[i] = 0u;
    fascon_plat_dma_sync(p, n, 0);
    return p;
}

/* 装置の窓（登録表）。読み出しが全 1 で書き込みを捨てる sink の窓 / device windows (a registry) and the sink window */
#ifndef FASCON2_MMIO_WINDOWS
#define FASCON2_MMIO_WINDOWS 64u
#endif
static uint64_t fascon__win[FASCON2_MMIO_WINDOWS][2];
static uint32_t fascon__nwin;
static uint8_t fascon__mmio_sink[4096] __attribute__((aligned(4096)));
/* Windows（MinGW の GNU ld）は、別のオブジェクトからの普通の参照を弱い定義（PE の weak external）へ結べない。この関数は
 * PC のプラットフォーム層（fascon2_plat_host.c）が呼ぶので、Windows では普通の定義にする（Windows でほかに定義するものはない）
 * Windows (GNU ld of MinGW) cannot bind a plain reference from another object to a weak definition (a PE weak external).
 * The PC platform layer (fascon2_plat_host.c) calls this function, so it is a plain definition on Windows (nothing else
 * defines it there) */
#if defined(_WIN32)
#define F20_WEAK_NOT_WIN
#else
#define F20_WEAK_NOT_WIN __attribute__((weak))
#endif
F20_WEAK_NOT_WIN void fascon_mmio_register_window(uint64_t phys, uint64_t size) {
    if (fascon__nwin < FASCON2_MMIO_WINDOWS) {
        fascon__win[fascon__nwin][0] = phys;
        fascon__win[fascon__nwin][1] = size;
        fascon__nwin++;
    }
}
/* 物理番地 → カーネルの仮想番地（generic-os の BSP は高位の写しを返す。既定は恒等写像） / physical → kernel virtual address
 * (generic-os BSPs return the high mirror; the default is the identity map) */
__attribute__((weak)) uintptr_t fascon_plat_mmio_va(uint64_t phys) { return (uintptr_t)phys; }
__attribute__((weak)) uintptr_t fascon_mmio_bind(uint64_t phys, size_t n, uint32_t *flt) {
    for (uint32_t i = 0; i < fascon__nwin; i++) {
        const uint64_t b = fascon__win[i][0], s = fascon__win[i][1];
        if (phys >= b && phys - b <= s && (uint64_t)n <= s - (phys - b)) return fascon_plat_mmio_va(phys);
    }
    *flt |= F20_FAULT_MMIO;
    for (size_t k = 0; k < sizeof fascon__mmio_sink; k++) fascon__mmio_sink[k] = 0xFFu;
    return (uintptr_t)fascon__mmio_sink;
}

__attribute__((weak)) void fascon_sched_yield(void) { fascon_cpu_relax(); }
__attribute__((weak)) void fascon_sleep_ticks(uint64_t ticks) {
    for (uint64_t i = 0; i < ticks; i++) fascon_cpu_relax();
}
__attribute__((weak)) uint64_t fascon_plat_timer_freq(void) { return 0u; }
__attribute__((weak)) uint64_t fascon_plat_timer_now(void) { return 0u; }
__attribute__((weak)) uint64_t fascon_clock_mono_ns(void) {
    const uint64_t f = fascon_plat_timer_freq(), t = fascon_plat_timer_now();
    if (f == 0u) return 0u;
    return (t / f) * UINT64_C(1000000000) + ((t % f) * UINT64_C(1000000000)) / f;
}
__attribute__((weak)) uint32_t fascon_cpu_this(void) { return fascon_plat_cpu_id(); }
__attribute__((weak)) uint32_t fascon_numa_node_of(uint32_t cpu) {
    (void)cpu;
    return 0u;
}
__attribute__((weak)) void fascon_plat_arena_grow(void *base, uint64_t bytes) {
    /* 既定: アリーナは .bss（すでに写っている）。generic-os の BSP は予約した仮想番地へ 2 MiB ずつ写す
     * default: arenas are in .bss (already mapped); generic-os BSPs map 2 MiB at a time into the reserved range */
    (void)base;
    (void)bytes;
}

/* ───── RCU: エポック方式（コアごとの入れ子の数とエポック、大域のエポック） / RCU: epoch based ───── */
#define FASCON__RCU_CPUS FASCON2_MAX_CPUS_MAG
static volatile uint64_t fascon__rcu_epoch = 1u;
static struct {
    volatile uint64_t epoch; /* 0 = 読み手でない / 0 = not reading */
    uint32_t nest;
    uint64_t irq;
    uint8_t pad_[F20_DESTRUCTIVE_SIZE - 24u];
} fascon__rcu[FASCON__RCU_CPUS] __attribute__((aligned(F20_DESTRUCTIVE_SIZE)));
typedef struct {
    void (*fn)(uintptr_t);
    uintptr_t arg;
    uint64_t epoch;
} fascon__rcu_cb_t;
#ifndef FASCON2_RCU_QUEUE
#define FASCON2_RCU_QUEUE 256u
#endif
/* 遅延キュー（全コアで 1 本、ビットロックで守る。コールバックはロックの外で呼ぶ）/ the deferred queue (one for all cores,
 * guarded by a bit lock; callbacks run outside the lock) */
static fascon__rcu_cb_t fascon__rcu_q[FASCON2_RCU_QUEUE];
static uint32_t fascon__rcu_qn;
static volatile uint32_t fascon__rcu_lock;
__attribute__((weak)) void fascon_rcu_read_lock(void) {
    const uint32_t c = fascon_cpu_this() % FASCON__RCU_CPUS;
    if (fascon__rcu[c].nest++ == 0u) {
        /* プリエンプション禁止（このコアの割り込みを止める）→ いまのエポックを公開（seq_cst: 書き手の読みより前に見える）
         * disable preemption (mask this core's interrupts) → publish the current epoch (seq_cst: visible before the writer reads) */
        fascon__rcu[c].irq = fascon_plat_irq_save();
        __atomic_store_n(&fascon__rcu[c].epoch, __atomic_load_n(&fascon__rcu_epoch, __ATOMIC_SEQ_CST), __ATOMIC_SEQ_CST);
    }
}
__attribute__((weak)) void fascon_rcu_read_unlock(void) {
    const uint32_t c = fascon_cpu_this() % FASCON__RCU_CPUS;
    if (--fascon__rcu[c].nest == 0u) {
        __atomic_store_n(&fascon__rcu[c].epoch, 0u, __ATOMIC_RELEASE);
        fascon_plat_irq_restore(fascon__rcu[c].irq);
    }
}
/* エポックを 1 進める: すべての読み手が「いまのエポック」か「読み手でない」なら進められる / advance the epoch by one: possible
 * once every reader is either in the current epoch or not reading */
static bool fascon__rcu_try_advance(void) {
    const uint64_t e = __atomic_load_n(&fascon__rcu_epoch, __ATOMIC_SEQ_CST);
    for (uint32_t c = 0; c < FASCON__RCU_CPUS; c++) {
        const uint64_t r = __atomic_load_n(&fascon__rcu[c].epoch, __ATOMIC_SEQ_CST);
        if (r != 0u && r != e) return false;
    }
    uint64_t want = e;
    (void)__atomic_compare_exchange_n(&fascon__rcu_epoch, &want, e + 1u, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return true;
}
/* 期限の来た（登録から 2 エポック進んだ）コールバックを、ロックの外で流す。眠らない / run the callbacks that are due (two
 * epochs past their registration) outside the lock; never sleeps */
__attribute__((weak)) uint32_t fascon_rcu_poll(void) {
    (void)fascon__rcu_try_advance();
    const uint64_t now = __atomic_load_n(&fascon__rcu_epoch, __ATOMIC_SEQ_CST);
    fascon__rcu_cb_t due[16];
    uint32_t nd = 0, ran = 0;
    for (;;) {
        const uint64_t im = fascon_plat_irq_save();
        f20_bitlock(&fascon__rcu_lock);
        uint32_t k = 0;
        nd = 0;
        for (uint32_t i = 0; i < fascon__rcu_qn; i++) {
            if (nd < 16u && fascon__rcu_q[i].epoch + 2u <= now) {
                due[nd++] = fascon__rcu_q[i];
            } else {
                fascon__rcu_q[k++] = fascon__rcu_q[i];
            }
        }
        fascon__rcu_qn = k;
        f20_bitunlock(&fascon__rcu_lock);
        fascon_plat_irq_restore(im);
        for (uint32_t i = 0; i < nd; i++) due[i].fn(due[i].arg);
        ran += nd;
        if (nd < 16u) return ran;
    }
}
/* 猶予期間: エポックが 2 つ進めば、呼んだ時点で読んでいた読み手はみな抜けている（Sleep 効果）/ a grace period: once the epoch
 * has advanced twice, every reader that was reading at the call has left (the Sleep effect) */
__attribute__((weak)) void fascon_rcu_synchronize(void) {
    const uint64_t start = __atomic_load_n(&fascon__rcu_epoch, __ATOMIC_SEQ_CST);
    while (__atomic_load_n(&fascon__rcu_epoch, __ATOMIC_SEQ_CST) < start + 2u) {
        if (!fascon__rcu_try_advance()) fascon_cpu_relax();
    }
    (void)fascon_rcu_poll();
}
__attribute__((weak)) void fascon_rcu_defer(void (*fn)(uintptr_t), uintptr_t arg) {
    for (;;) {
        const uint64_t im = fascon_plat_irq_save();
        f20_bitlock(&fascon__rcu_lock);
        const bool full = fascon__rcu_qn >= FASCON2_RCU_QUEUE;
        if (!full) {
            fascon__rcu_q[fascon__rcu_qn].fn = fn;
            fascon__rcu_q[fascon__rcu_qn].arg = arg;
            fascon__rcu_q[fascon__rcu_qn].epoch = __atomic_load_n(&fascon__rcu_epoch, __ATOMIC_SEQ_CST);
            fascon__rcu_qn++;
        }
        const uint32_t n = fascon__rcu_qn;
        f20_bitunlock(&fascon__rcu_lock);
        fascon_plat_irq_restore(im);
        if (!full) {
            /* 半分を超えたら期限の来たものを流す（確保の遅い道からも呼ばれる）/ past half full, run what is due */
            if (n > FASCON2_RCU_QUEUE / 2u) (void)fascon_rcu_poll();
            return;
        }
        /* 遅延キューが満杯: その場で猶予期間を待って流す / the deferred queue is full: wait for a grace period now */
        fascon_rcu_synchronize();
    }
}

#if defined(FASCON2_GENERIC_OS)
/* ───── Per-CPU マガジン（generic-os の共有アリーナ）の遅い道 / the slow paths of per-CPU magazines ───── */
/* 大域の空きリストか next_unused から半分のマガジンを補充する（ビットロックの中。2 MiB 境界を跨ぐときだけ写す）
 * refill half a magazine from the global free list or next_unused (inside the bit lock; maps only when crossing 2 MiB) */
__attribute__((weak)) void f20_mag_refill(f20_magd_t *d, f20_mag_t *m) {
    for (int pass = 0; pass < 2 && m->n == 0u; pass++) {
        f20_bitlock(&d->lock);
        while (m->n < F20_MAG_SIZE / 2u && d->st->free_head != 0u) {
            const uint32_t i = d->st->free_head - 1u;
            d->st->free_head = d->link[i];
            if (d->st->free_head == 0u) d->st->free_tail = 0u;
            d->link[i] = 0u;
            m->idx[m->n++] = i;
        }
        while (m->n < F20_MAG_SIZE / 2u && d->st->next_unused < d->cap) {
            const uint32_t i = d->st->next_unused;
            const uint64_t need = d->slot_off + (uint64_t)(i + 1u) * d->slot_size;
            if (need > d->mapped) {
                /* 2 MiB 境界を跨ぐ: 要求元コアの NUMA ノードの 2 MiB ページを 0 で写す / crossing 2 MiB: map a zeroed 2 MiB page
                 * from the requesting core's NUMA node */
                const uint64_t to = (need + (F20_HUGE_PAGE - 1u)) & ~(uint64_t)(F20_HUGE_PAGE - 1u);
                fascon_plat_arena_grow(d->base, to);
                d->mapped = to;
            }
            d->st->next_unused = i + 1u;
            d->gen[i] = 1u;
            m->idx[m->n++] = i;
        }
        f20_bitunlock(&d->lock);
        /* 空なら猶予期間の済んだ解放を回収してもう一度 / if empty, reclaim frees whose grace period has passed and retry */
        if (m->n == 0u && pass == 0) {
            fascon_plat_irq_restore(0u);
            (void)fascon_rcu_poll();
            (void)fascon_plat_irq_save();
        }
    }
}
/* 猶予期間の後: 世代を進め（使い切ったら引退）、自分のコアのマガジンへ返す。満杯なら半分を大域の空きリストへ
 * after the grace period: advance the generation (retire when exhausted) and return the slot to this core's magazine;
 * when full, give half back to the global free list */
__attribute__((weak)) void f20_mag_release(f20_magd_t *d, uint32_t i) {
    if (d->gen[i] == 0xFFFFFFFFu) {
        d->gen[i] = 0u; /* 引退 / retired */
        __atomic_store_n(&d->link[i], 0u, __ATOMIC_RELEASE);
        __atomic_fetch_add(&d->st->retired, 1u, __ATOMIC_RELAXED);
        return;
    }
    d->gen[i] = d->gen[i] + 1u;
    __atomic_store_n(&d->link[i], 0u, __ATOMIC_RELEASE);
    const uint64_t im = fascon_plat_irq_save();
    f20_mag_t *m = &d->mag[fascon_cpu_this() % FASCON2_MAX_CPUS_MAG];
    if (m->n == F20_MAG_SIZE) {
        f20_bitlock(&d->lock);
        while (m->n > F20_MAG_SIZE / 2u) {
            const uint32_t j = m->idx[--m->n];
            d->link[j] = 0u;
            if (d->st->free_tail == 0u) {
                d->st->free_head = d->st->free_tail = j + 1u;
            } else {
                d->link[d->st->free_tail - 1u] = j + 1u;
                d->st->free_tail = j + 1u;
            }
        }
        f20_bitunlock(&d->lock);
    }
    m->idx[m->n++] = i;
    fascon_plat_irq_restore(im);
}

/* ───── アドレス空間の実装（ページの参照の数は大域の小さな表。ビットロックで守る）/ address spaces ───── */
#ifndef FASCON2_PAGE_REFS
#define FASCON2_PAGE_REFS 256u
#endif
static struct {
    uint8_t *p;
    uint32_t n;
} fascon__pref[FASCON2_PAGE_REFS];
static volatile uint32_t fascon__pref_lock;
static uint32_t *fascon__pref_slot(uint8_t *p, bool make) {
    uint32_t h = (uint32_t)(((uintptr_t)p >> 12) * 2654435761u) % FASCON2_PAGE_REFS;
    for (uint32_t k = 0; k < FASCON2_PAGE_REFS; k++) {
        const uint32_t i = (h + k) % FASCON2_PAGE_REFS;
        if (fascon__pref[i].p == p) return &fascon__pref[i].n;
        if (fascon__pref[i].p == NULL) {
            if (!make) return NULL;
            fascon__pref[i].p = p;
            fascon__pref[i].n = 0u;
            return &fascon__pref[i].n;
        }
    }
    return NULL;
}
static uint32_t fascon__pref_add(uint8_t *p, int32_t d) {
    const uint64_t im = fascon_plat_irq_save();
    f20_bitlock(&fascon__pref_lock);
    uint32_t *n = fascon__pref_slot(p, d > 0);
    uint32_t v = 0u;
    if (n != NULL) {
        *n = (uint32_t)((int32_t)*n + d);
        v = *n;
    }
    f20_bitunlock(&fascon__pref_lock);
    fascon_plat_irq_restore(im);
    return v;
}
static int32_t fascon__as_find(const f20_aspace_t *as, uint64_t va) {
    for (uint32_t i = 0; i < FASCON2_ASPACE_PAGES; i++) {
        if ((as->flags[i] & F20_PG_PRESENT) && as->va[i] == va) return (int32_t)i;
    }
    return -1;
}
static int32_t fascon__as_free_entry(const f20_aspace_t *as) {
    for (uint32_t i = 0; i < FASCON2_ASPACE_PAGES; i++) {
        if (!(as->flags[i] & F20_PG_PRESENT)) return (int32_t)i;
    }
    return -1;
}
/* 古いページを手放す: 共有なら参照を 1 つ減らし、最後の参照か専有なら解放 / let go of an old page: drop one reference when
 * shared, free it when it was the last reference or owned exclusively */
static void fascon__as_release(uint8_t *page, uint8_t flags) {
    if (flags & F20_PG_COW) {
        if (fascon__pref_add(page, -1) != 0u) return;
    }
    fascon_own_free(page);
}
__attribute__((weak)) void fascon_plat_pte_set(uint64_t asid, uint64_t va, const void *page, uint32_t flags) {
    (void)asid;
    (void)va;
    (void)page;
    (void)flags;
}
static uint64_t fascon__tlb_local, fascon__tlb_ipi;
__attribute__((weak)) uint64_t fascon_tlb_local_count(void) { return __atomic_load_n(&fascon__tlb_local, __ATOMIC_RELAXED); }
__attribute__((weak)) uint64_t fascon_tlb_ipi_count(void) { return __atomic_load_n(&fascon__tlb_ipi, __ATOMIC_RELAXED); }
/* 既定（PC 実行）: 数えるだけ。BSP の強い定義が TLBI と SGI を出す / the default (PC runs) only counts; the BSP's strong
 * definition issues the TLBIs and SGIs */
__attribute__((weak)) void fascon_tlb_flush_va(uint64_t asid, uint64_t va, uint64_t cpu_mask) {
    (void)asid;
    (void)va;
    const uint64_t self = UINT64_C(1) << (fascon_cpu_this() & 63u);
    const uint64_t others = cpu_mask & ~self;
    if (others == 0u) {
        __atomic_fetch_add(&fascon__tlb_local, 1u, __ATOMIC_RELAXED);
        return;
    }
    __atomic_fetch_add(&fascon__tlb_ipi, (uint64_t)__builtin_popcountll(others), __ATOMIC_RELAXED);
    if (cpu_mask & self) __atomic_fetch_add(&fascon__tlb_local, 1u, __ATOMIC_RELAXED);
}
int64_t f20_aspace_map(f20_aspace_t *as, uint64_t asid, uint64_t va, uint8_t *page, bool writable) {
    if ((va & 4095u) != 0u) {
        fascon_own_free(page);
        return 22; /* EINVAL */
    }
    int32_t i = fascon__as_find(as, va);
    if (i >= 0) {
        fascon__as_release(as->page[i], as->flags[i]);
    } else {
        i = fascon__as_free_entry(as);
        if (i < 0) {
            fascon_own_free(page);
            return 12; /* ENOMEM */
        }
    }
    as->va[i] = va;
    as->page[i] = page;
    as->flags[i] = (uint8_t)(F20_PG_PRESENT | (writable ? F20_PG_WRITE : 0u));
    fascon_plat_pte_set(asid, va, page, as->flags[i]);
    return 0;
}
int64_t f20_aspace_share(f20_aspace_t *src, uint64_t asid_src, uint64_t va, f20_aspace_t *dst, uint64_t asid_dst, uint64_t src_mask) {
    const int32_t i = fascon__as_find(src, va);
    if (i < 0) return 14; /* EFAULT */
    if (fascon__as_find(dst, va) >= 0) return 17; /* EEXIST */
    const int32_t j = fascon__as_free_entry(dst);
    if (j < 0) return 12; /* ENOMEM */
    if (!(src->flags[i] & F20_PG_COW)) {
        (void)fascon__pref_add(src->page[i], 1); /* 専有 → 共有（参照 1） / exclusive → shared (1 reference) */
    }
    (void)fascon__pref_add(src->page[i], 1);
    src->flags[i] = (uint8_t)(F20_PG_PRESENT | F20_PG_COW); /* 書き込み不可へ / now read-only */
    dst->va[j] = va;
    dst->page[j] = src->page[i];
    dst->flags[j] = (uint8_t)(F20_PG_PRESENT | F20_PG_COW);
    fascon_plat_pte_set(asid_src, va, src->page[i], src->flags[i]);
    fascon_plat_pte_set(asid_dst, va, dst->page[j], dst->flags[j]);
    fascon_tlb_flush_va(asid_src, va, src_mask); /* RW → RO の変更を掃く / flush the RW → RO change */
    return 0;
}
uint8_t *f20_aspace_cow_copy(f20_aspace_t *as, uint64_t va, const char *site) {
    const int32_t i = fascon__as_find(as, va);
    if (i < 0 || !(as->flags[i] & F20_PG_COW)) return NULL;
    uint8_t *dst = (uint8_t *)fascon_own_alloc(4096u, site);
    for (uint32_t k = 0; k < 4096u; k++) dst[k] = as->page[i][k];
    return dst;
}
uint8_t *f20_aspace_unmap(f20_aspace_t *as, uint64_t asid, uint64_t va) {
    const int32_t i = fascon__as_find(as, va);
    if (i < 0) return NULL;
    uint8_t *p = as->page[i];
    const uint8_t f = as->flags[i];
    as->flags[i] = 0u;
    fascon_plat_pte_set(asid, va, NULL, 0u);
    if (f & F20_PG_COW) {
        if (fascon__pref_add(p, -1) != 0u) return NULL; /* まだ共有: 所有権は戻らない / still shared: no ownership comes back */
    }
    return p;
}
uint8_t f20_aspace_peek(f20_aspace_t *as, uint64_t va, uint64_t off) {
    const int32_t i = fascon__as_find(as, va);
    if (i < 0 || off >= 4096u) return 0u;
    return as->page[i][off];
}
void f20_aspace_drop(f20_aspace_t *as, uint64_t asid) {
    for (uint32_t i = 0; i < FASCON2_ASPACE_PAGES; i++) {
        if (as->flags[i] & F20_PG_PRESENT) {
            fascon_plat_pte_set(asid, as->va[i], NULL, 0u);
            fascon__as_release(as->page[i], as->flags[i]);
            as->flags[i] = 0u;
        }
    }
}
#endif /* FASCON2_GENERIC_OS */
#endif /* FASCON2_UNIVERSAL (implementation) */

/* ═════════════ fascon-gc の実装 / implementation of fascon-gc ═════════════ */
#if defined(FASCON2_GC)
F20_PERCPU f20_gc_frame_t *fascon__gc_top;
F20_PERCPU uint32_t fascon__gc_reg;
volatile uint32_t fascon__gc_req; /* 奇数 = 共有の gc_arena を回収中 / odd = a shared gc_arena is being collected */
static f20_gc_frame_t *volatile *fascon__gc_topp[FASCON2_MAX_CPUS]; /* コアごとの fascon__gc_top の番地 / per-core address of fascon__gc_top */
static volatile uint32_t fascon__gc_ack[FASCON2_MAX_CPUS];
static f20_gc_frame_t *volatile fascon__gc_pub[FASCON2_MAX_CPUS];
static volatile uint32_t fascon__gc_lock;
/* このコアで回収中（入れ子の gc_collect は何もしない: 掃除の中の #[on_drop]・同じコアのほかのタスク）
 * a collection is running on this core (a nested gc_collect does nothing: an #[on_drop] inside the sweep, another task on
 * the same core) */
static F20_PERCPU uint32_t fascon__gc_busy;
#if defined(FASCON2_GC_ALIAS_CHECK)
/* 検査用: 1 回の走査で同じ own の記憶域へ 2 度届いたら止める（持ち主が 2 人 = 移動元を空にし忘れた）。テストだけで使う
 * for tests: stop when one walk reaches the same own storage twice (two owners = a move that did not clear its source) */
#define F20_GC_ALIAS_SLOTS 65536u
static F20_PERCPU uintptr_t fascon__gc_seen[F20_GC_ALIAS_SLOTS];
static F20_PERCPU uint32_t fascon__gc_seen_n;
static void f20_gc_seen_clear(void) {
    __builtin_memset(fascon__gc_seen, 0, sizeof fascon__gc_seen);
    fascon__gc_seen_n = 0u;
}
static void f20_gc_seen_add(const void *q) {
    uintptr_t k = (uintptr_t)q;
    uint32_t h = (uint32_t)((k >> 4) * UINT64_C(0x9E3779B97F4A7C15) >> 40) & (F20_GC_ALIAS_SLOTS - 1u);
    for (uint32_t n = 0; n < F20_GC_ALIAS_SLOTS; n++, h = (h + 1u) & (F20_GC_ALIAS_SLOTS - 1u)) {
        if (fascon__gc_seen[h] == k) fascon_plat_kernel_panic(FASCON_PANIC_OWN_CORRUPT, "fascon-gc: one own storage is reachable from two roots (alias check)");
        if (fascon__gc_seen[h] == 0u) {
            fascon__gc_seen[h] = k;
            fascon__gc_seen_n++;
            return;
        }
    }
}
#endif

/* 枠の先頭はほかのコアの回収が待ち合わせで読む（空かどうか）ので、原子的に書く（x86-64 / AArch64 ではふつうの格納）
 * the top frame is read by another core's collection while it waits (empty or not), so it is written atomically (a plain
 * store on x86-64 / AArch64) */
void f20_gc_pop(f20_gc_frame_t *f) { __atomic_store_n(&fascon__gc_top, f->prev, __ATOMIC_RELAXED); }

/* 止まっているタスクの先頭（BSP が強い定義で置き換える） / tops of suspended tasks (the BSP replaces this) */
__attribute__((weak)) uint32_t fascon_plat_gc_task_tops(void **out, uint32_t max) {
    (void)out;
    (void)max;
    return 0u;
}
/* タスクの切り替えで保存・復元する（BSP が呼ぶ） / saved and restored at task switches (called by the BSP) */
void *fascon_gc_top_get(void) { return (void *)fascon__gc_top; }
void fascon_gc_top_set(void *p) { __atomic_store_n(&fascon__gc_top, (f20_gc_frame_t *)p, __ATOMIC_RELAXED); }

void f20_gc_register_core(void) {
    const uint32_t c = fascon_cpu_this();
    if (c < FASCON2_MAX_CPUS) __atomic_store_n(&fascon__gc_topp[c], (f20_gc_frame_t *volatile *)&fascon__gc_top, __ATOMIC_RELEASE);
    fascon__gc_reg = 1u;
}

/* 安全点で止まる: 自分のシャドウスタックを公開し、回収が終わるまで待つ / park at a safepoint: publish this core's shadow
 * stack and wait until the collection ends */
void f20_gc_park(void) {
    const uint32_t c = fascon_cpu_this();
    const uint32_t r = __atomic_load_n(&fascon__gc_req, __ATOMIC_ACQUIRE);
    if (!(r & 1u) || c >= FASCON2_MAX_CPUS) return;
    /* 回収しているコア自身は止まらない（タスクの切り替えから呼ばれても待ち合わせない） / the collecting core never parks
     * (not even when called from a task switch) */
    if (fascon__gc_busy != 0u) return;
#if defined(FASCON2_SMP)
    /* 安全点の待ち合わせは開いた区間でだけ: critical のロック・par() の読み手を持ったまま止まると、回収するコアとロックを
     * 待つコアが互いに待ちうる。持っている間は止まらず、区間を出た次の安全点で止まる
     * safepoint rendezvous only in open sections: parking while holding the critical lock or a par() reader could make
     * the collecting core and a core waiting for the lock wait on each other. While holding one, do not park; park at the
     * next safepoint after leaving the section */
    if (fascon__wdepth != 0u || fascon__rdepth != 0u) return;
#endif
    fascon__gc_pub[c] = fascon__gc_top;
    __atomic_store_n(&fascon__gc_ack[c], r, __ATOMIC_RELEASE);
    while (__atomic_load_n(&fascon__gc_req, __ATOMIC_ACQUIRE) == r) FASCON_CPU_RELAX();
}

static inline uint32_t f20_gc_gen_at(const f20_gc_heap_t *h, uint32_t i) {
    switch (h->gen_bytes) {
    case 1u: return ((const uint8_t *)h->gen)[i];
    case 2u: return ((const uint16_t *)h->gen)[i];
    default: return ((const uint32_t *)h->gen)[i];
    }
}
static inline void f20_gc_gen_put(f20_gc_heap_t *h, uint32_t i, uint32_t g) {
    switch (h->gen_bytes) {
    case 1u: ((uint8_t *)h->gen)[i] = (uint8_t)g; break;
    case 2u: ((uint16_t *)h->gen)[i] = (uint16_t)g; break;
    default: ((uint32_t *)h->gen)[i] = g; break;
    }
}

/* 走査の文脈: 印付け（mark）か、世代リフレッシュの書き換え（rewrite）か / the walk context: marking, or the rewrite of a
 * generation refresh */
typedef struct {
    int rewrite;
    uint32_t only; /* 書き換え: 対象の gc_arena / rewrite: the target gc arena */
    uint64_t marked;
} f20_gc_cx_t;

/* Id を 1 つ: 生きていて世代が合えば印を付けて作業スタックへ（書き換えでは、生きていれば世代 1、古ければ 0 = None）
 * one Id: if live with a matching generation, mark it and push it (rewrite: live → generation 1, stale → 0 = None) */
static void f20_gc_id(f20_gc_cx_t *cx, uint32_t hi, void *at) {
    if (hi >= fascon__gc_nheaps) return;
    f20_gc_heap_t *h = fascon__gc_heap_at(hi);
    f20_id_t id;
    __builtin_memcpy(&id, at, sizeof id);
    if (id == 0u) return;
    const uint32_t i = F20_ID_INDEX(id, h->k), g = F20_ID_GEN(id, h->k);
    /* 生存ビットマップはほかのコアの確保が原子的に書くので、原子的に読む（x86-64 / AArch64 ではふつうの読み込み）
     * the live bitmap is written atomically by allocations on other cores, so it is read atomically (a plain load on
     * x86-64 / AArch64) */
    const int ok = i < h->st->next_unused && ((__atomic_load_n(&h->live[i >> 6], __ATOMIC_RELAXED) >> (i & 63u)) & 1u) && f20_gc_gen_at(h, i) == g;
    if (cx->rewrite) {
        if (hi != cx->only) return;
        const f20_id_t nid = ok ? (f20_id_t)F20_ID_MAKE(1u, i, h->k) : (f20_id_t)0u;
        __builtin_memcpy(at, &nid, sizeof nid);
        return;
    }
    if (!ok) return;
    uint64_t *w = &h->mark[i >> 6];
    const uint64_t b = UINT64_C(1) << (i & 63u);
    if (*w & b) return;
    *w |= b;
    h->work[h->sp++] = i;
    cx->marked++;
}

/* 記述子どおりに値を歩く（生ポインタが無いので誤認は無い） / walk a value by its descriptor (no raw pointers: no guessing) */
static void f20_gc_walk(f20_gc_cx_t *cx, void *p, const f20_type_desc_t *d) {
    if (p == NULL || d == NULL) return;
    for (uint32_t k = 0; k < d->nfields; k++) {
        const f20_gc_field_t *f = &d->fields[k];
        uint8_t *at = (uint8_t *)p + f->off;
        switch (f->kind) {
        case F20_GC_ID: f20_gc_id(cx, f->heap, at); break;
        case F20_GC_OWN: {
            /* own の先: NULL（移動済み・解放済み・None）は f20_gc_walk の入口で飛ばす。詰めた子・フレームの own も同じく
             * 記述子のオフセットで読む / behind an own: NULL (moved, freed, None) is skipped at the entry of f20_gc_walk;
             * packed children and frame-region owns are read by descriptor offsets the same way */
            void *q;
            __builtin_memcpy(&q, at, sizeof q);
#if defined(FASCON2_GC_ALIAS_CHECK)
            if (q != NULL) f20_gc_seen_add(q);
#endif
            f20_gc_walk(cx, q, f->desc);
            break;
        }
        case F20_GC_ARR:
            for (uint32_t j = 0; j < f->count; j++) f20_gc_walk(cx, at + (size_t)j * f->stride, f->desc);
            break;
        case F20_GC_OPT:
            if (*((const bool *)((const uint8_t *)p + f->tag))) f20_gc_walk(cx, at, f->desc);
            break;
        default: break;
        }
    }
}
static void f20_gc_visit(void *p, const f20_type_desc_t *d, void *cx) { f20_gc_walk((f20_gc_cx_t *)cx, p, d); }

static void f20_gc_chain(f20_gc_cx_t *cx, f20_gc_frame_t *f) {
    for (; f != NULL; f = f->prev)
        for (uint32_t k = 0; k < f->n && k < f->cap; k++)
            if (f->addr[k] != NULL) f20_gc_walk(cx, f->addr[k], f->desc[k]);
}

/* すべての根: 静的変数・ふつうのアリーナ・自分と止まっているタスクのシャドウスタック・（共有なら）止めたコア
 * every root: statics, ordinary arenas, this core's and suspended tasks' shadow stacks, (shared) the parked cores */
static void f20_gc_all_roots(f20_gc_cx_t *cx, int shared) {
#if defined(FASCON2_GC_ALIAS_CHECK)
    f20_gc_seen_clear();
#endif
    fascon__gc_roots(f20_gc_visit, cx, cx->rewrite);
    /* 鎖は先頭の枠で見分けて 1 回だけ歩く（止まったタスクの鎖と、切り替えの途中で止まったコアが公開した鎖が同じでも 2 度
     * 書き換えない） / each chain is walked once, told apart by its head frame (a suspended task's chain and the chain a
     * core published while parking in a task switch may be the same; it is never rewritten twice) */
    f20_gc_frame_t *done[64 + FASCON2_MAX_CPUS + 1];
    uint32_t nd = 0u;
#define F20_GC_CHAIN_ONCE(head)                                                         \
    do {                                                                                \
        f20_gc_frame_t *const h_ = (head);                                              \
        int seen_ = h_ == NULL;                                                         \
        for (uint32_t i_ = 0; !seen_ && i_ < nd; i_++) seen_ = done[i_] == h_;          \
        if (!seen_) {                                                                   \
            if (nd < (uint32_t)(sizeof done / sizeof done[0])) done[nd++] = h_;         \
            f20_gc_chain(cx, h_);                                                       \
        }                                                                               \
    } while (0)
    F20_GC_CHAIN_ONCE(fascon__gc_top);
    void *tops[64];
    const uint32_t nt = fascon_plat_gc_task_tops(tops, 64u);
    for (uint32_t t = 0; t < nt && t < 64u; t++) F20_GC_CHAIN_ONCE((f20_gc_frame_t *)tops[t]);
    if (shared) {
        const uint32_t me = fascon_cpu_this();
        for (uint32_t c = 0; c < FASCON2_MAX_CPUS; c++)
            if (c != me && __atomic_load_n(&fascon__gc_topp[c], __ATOMIC_ACQUIRE) != NULL && __atomic_load_n(&fascon__gc_ack[c], __ATOMIC_ACQUIRE) == fascon__gc_req) F20_GC_CHAIN_ONCE(fascon__gc_pub[c]);
    }
#undef F20_GC_CHAIN_ONCE
}

/* 作業スタックが空になるまで: Id 専用の射影列だけを読む（無ければスロットを記述子で）。他の gc_arena へも辿る
 * drain the work stacks: read only the Id-only projection columns (or the slots by descriptor); edges into other gc
 * arenas are followed too */
static void f20_gc_drain(f20_gc_cx_t *cx) {
    for (;;) {
        int any = 0;
        for (uint32_t hi = 0; hi < fascon__gc_nheaps; hi++) {
            f20_gc_heap_t *h = fascon__gc_heap_at(hi);
            while (h->sp != 0u) {
                any = 1;
                const uint32_t i = h->work[--h->sp];
                if (h->ncols != 0u) {
                    for (uint32_t c = 0; c < h->ncols; c++) {
                        f20_id_t id = h->col_wide ? (f20_id_t)((const uint64_t *)h->cols[c])[i] : (f20_id_t)((const uint32_t *)h->cols[c])[i];
                        f20_gc_id(cx, h->col_heap[c], &id);
                    }
                } else {
                    f20_gc_walk(cx, h->slot0 + (size_t)i * h->stride, h->elem);
                }
            }
        }
        if (!any) return;
    }
}

#if !defined(FASCON2_ID64)
/* 世代リフレッシュ（32 ビット Id だけ）: 生きている Id を世代 1 へ書き換え、古い Id を 0 にし、引退したスロットを空きへ戻す
 * generation refresh (32-bit Ids only): rewrite live Ids to generation 1, stale Ids to 0, and return retired slots */
static void f20_gc_refresh(uint32_t hi) {
    f20_gc_heap_t *h = fascon__gc_heap_at(hi);
    f20_gc_cx_t cx = {1, hi, 0u};
    f20_gc_all_roots(&cx, h->shared != 0u);
    for (uint32_t hj = 0; hj < fascon__gc_nheaps; hj++) {
        f20_gc_heap_t *o = fascon__gc_heap_at(hj);
        for (uint32_t w = 0; w < F20_LIVE_WORDS_USED(o->st->next_unused); w++) {
            uint64_t m = __atomic_load_n(&o->live[w], __ATOMIC_RELAXED);
            while (m) {
                const uint32_t i = (w << 6) + (uint32_t)__builtin_ctzll(m);
                m &= m - 1u;
                f20_gc_walk(&cx, o->slot0 + (size_t)i * o->stride, o->elem);
            }
        }
    }
    /* 世代: 生きているスロット = 1、ほかも 1 にして空きリストを作り直す（古い Id はもうどこにも無い）
     * generations: live slots = 1, the others 1 as well, and the free list is rebuilt (no stale Id is left anywhere) */
    h->st->free_head = h->st->free_tail = 0u;
    h->st->retired = 0u;
    for (uint32_t i = 0; i < h->st->next_unused; i++) {
        f20_gc_gen_put(h, i, 1u);
        if ((__atomic_load_n(&h->live[i >> 6], __ATOMIC_RELAXED) >> (i & 63u)) & 1u) continue;
        f20_arena_state_t *st = h->st;
        h->link[i] = 0u;
        if (st->free_tail == 0u) {
            st->free_head = st->free_tail = i + 1u;
        } else {
            h->link[st->free_tail - 1u] = i + 1u;
            st->free_tail = i + 1u;
        }
    }
    if (h->remirror != NULL)
        for (uint32_t i = 0; i < h->st->next_unused; i++)
            if ((__atomic_load_n(&h->live[i >> 6], __ATOMIC_RELAXED) >> (i & 63u)) & 1u) h->remirror(i);
    h->refreshes++;
#if defined(FASCON2_GC_TRACE)
    fascon_print_str((FasconStr){"[gc] generation refresh", 23u});
#endif
}
#endif

uint32_t f20_gc_collect(f20_gc_heap_t *h) {
    /* 入れ子の回収（掃除の中の #[on_drop]、同じコアで割り込んだタスク）は何もしない: 外側の回収が使う印・作業スタックを
     * 壊さない / a nested collection (an #[on_drop] inside the sweep, a task that preempted this core) does nothing: the
     * marks and work stacks of the outer collection stay intact */
    if (fascon__gc_busy != 0u) return 0u;
#if defined(FASCON2_PREEMPT) && !defined(FASCON2_USER)
#define F20_GC_MASK 1
    /* 印付けが終わるまで、このコアは切り替わらない（止まっているタスクの鎖を歩いている間に、そのタスクが走り出さない）
     * this core does not switch until marking is done (a suspended task never resumes while its chain is being walked) */
    uint64_t im = fascon_plat_irq_save();
#endif
    const int shared = h->shared != 0u;
    uint32_t ep = 0u;
    if (shared) {
        /* ほかのコアが回収中なら、自分も安全点として止まってから錠を取る（互いに待って止まらない）
         * if another core is collecting, park as a safepoint first, then take the lock (no mutual waiting) */
        while (__atomic_exchange_n(&fascon__gc_lock, 1u, __ATOMIC_ACQUIRE) != 0u) {
            if (__atomic_load_n(&fascon__gc_req, __ATOMIC_ACQUIRE) & 1u) f20_gc_park();
            FASCON_CPU_RELAX();
        }
    }
    /* 錠を取ってから「回収中」（それまでは、ほかのコアの回収のために止まれる） / "collecting" from the moment the lock is
     * held (until then this core can still park for another core's collection) */
    fascon__gc_busy = 1u;
    if (shared) {
        ep = __atomic_add_fetch(&fascon__gc_req, 1u, __ATOMIC_SEQ_CST); /* 奇数: 回収を始める / odd: collection starts */
        __atomic_thread_fence(__ATOMIC_SEQ_CST); /* 入口の安全点（F20_GC_POLL_ENTRY）と対になる柵 / pairs with the entry safepoint fence */
        const uint32_t me = fascon_cpu_this();
        for (uint32_t c = 0; c < FASCON2_MAX_CPUS; c++) {
            if (c == me || __atomic_load_n(&fascon__gc_topp[c], __ATOMIC_ACQUIRE) == NULL) continue;
            /* その gc_arena に触れる関数の中にいるコアだけを待つ（枠が空のコアは止めない）
             * wait only for cores inside functions that touch the gc arena (cores with an empty chain never stop) */
            while (__atomic_load_n(fascon__gc_topp[c], __ATOMIC_ACQUIRE) != NULL && __atomic_load_n(&fascon__gc_ack[c], __ATOMIC_ACQUIRE) != ep) FASCON_CPU_RELAX();
        }
    }
    /* 印を消す / clear the marks */
    for (uint32_t hi = 0; hi < fascon__gc_nheaps; hi++) {
        f20_gc_heap_t *o = fascon__gc_heap_at(hi);
        __builtin_memset(o->mark, 0, (size_t)F20_LIVE_WORDS_USED(o->st->next_unused) * sizeof(uint64_t));
        o->sp = 0u;
    }
    f20_gc_cx_t cx = {0, 0u, 0u};
    f20_gc_all_roots(&cx, shared);
    f20_gc_drain(&cx);
    /* 掃除: live & ~mark。先に添字を集めてから解放する（解放の中の on_drop が確保しても数え違えない）
     * sweep: live & ~mark; collect the indices first, then free (an on_drop that allocates cannot confuse the count) */
    uint32_t n = 0u;
    for (uint32_t w = 0; w < F20_LIVE_WORDS_USED(h->st->next_unused); w++) {
        uint64_t dead = __atomic_load_n(&h->live[w], __ATOMIC_RELAXED) & ~h->mark[w];
        while (dead) {
            h->work[n++] = (w << 6) + (uint32_t)__builtin_ctzll(dead); /* ctz */
            dead &= dead - 1u;                                       /* blsr */
        }
    }
#if defined(F20_GC_MASK)
    /* 解放（#[on_drop] を呼ぶ）は割り込みを戻してから。死んだスロットの一覧はもう決まっている（このあと確保されたスロットは
     * 一覧に無い）。入れ子の回収は fascon__gc_busy が止める / the frees (they call #[on_drop]) run with interrupts restored;
     * the list of dead slots is already fixed (slots allocated from now on are not in it), and fascon__gc_busy stops
     * nested collections */
    fascon_plat_irq_restore(im);
#endif
#if defined(FASCON2_SMP)
    if (shared && n != 0u) fascon_rcu_synchronize(); /* 他コアの rcu_read の読み手が終わってから / after other cores' rcu_read readers */
#endif
    uint32_t freed = 0u;
    for (uint32_t k = 0; k < n; k++)
        if (h->free_fn(h->work[k])) freed++;
#if defined(FASCON2_GC_TRACE)
    fascon_print_str((FasconStr){"[gc] marked / dead / freed", 26u});
    fascon_print_u64(cx.marked);
    fascon_print_u64(n);
    fascon_print_u64(freed);
#endif
#if !defined(FASCON2_ID64)
    /* 世代が上限の 3/4 を超えたスロットがある・引退したスロットがある → 世代リフレッシュ（64 ビット Id では起きない）
     * a slot past 3/4 of the generation limit, or retired slots → generation refresh (never needed with 64-bit Ids) */
    {
        int need = h->st->retired != 0u;
        const uint32_t lim = F20_GEN_MAX(h->k) - (F20_GEN_MAX(h->k) >> 2);
        for (uint32_t i = 0; !need && i < h->st->next_unused; i++) need = f20_gc_gen_at(h, i) >= lim;
        if (need) {
#if defined(F20_GC_MASK)
            im = fascon_plat_irq_save(); /* 書き換えも切り替えなしで / the rewrite runs without switches too */
#endif
            f20_gc_refresh(h->idx);
#if defined(F20_GC_MASK)
            fascon_plat_irq_restore(im);
#endif
        }
    }
#endif
    h->collections++;
    h->reclaimed += freed;
    if (shared) {
        __atomic_add_fetch(&fascon__gc_req, 1u, __ATOMIC_ACQ_REL); /* 偶数: 止めたコアを放す / even: release the parked cores */
        __atomic_store_n(&fascon__gc_lock, 0u, __ATOMIC_RELEASE);
    }
    fascon__gc_busy = 0u;
    return freed;
}
#if defined(F20_GC_MASK)
#undef F20_GC_MASK
#endif
#endif /* FASCON2_GC */

#ifdef FASCON2_FREESTANDING
/* ベアメタルでは GCC / Clang が構造体コピーに使う関数を用意する。ループ内の空の asm は、
 * コンパイラがこのループ自体を memcpy 呼び出しへ書き換えて無限再帰になるのを防ぐ。
 * freestanding: functions GCC / Clang emit for struct copies. The empty asm inside each loop keeps the
 * compiler from turning the loop itself back into a memcpy call (which would recurse forever). */
#define FASCON__NO_IDIOM() __asm__ __volatile__("" ::: "memory")
__attribute__((weak)) void *memcpy(void *d, const void *s, size_t n) {
    uint8_t *dd = (uint8_t *)d;
    const uint8_t *ss = (const uint8_t *)s;
    while (n--) {
        *dd++ = *ss++;
        FASCON__NO_IDIOM();
    }
    return d;
}
__attribute__((weak)) void *memmove(void *d, const void *s, size_t n) {
    uint8_t *dd = (uint8_t *)d;
    const uint8_t *ss = (const uint8_t *)s;
    if (dd < ss) {
        while (n--) {
            *dd++ = *ss++;
            FASCON__NO_IDIOM();
        }
    } else {
        dd += n;
        ss += n;
        while (n--) {
            *--dd = *--ss;
            FASCON__NO_IDIOM();
        }
    }
    return d;
}
__attribute__((weak)) void *memset(void *d, int c, size_t n) {
    uint8_t *dd = (uint8_t *)d;
    while (n--) {
        *dd++ = (uint8_t)c;
        FASCON__NO_IDIOM();
    }
    return d;
}
__attribute__((weak)) int memcmp(const void *a, const void *b, size_t n) {
    const uint8_t *x = (const uint8_t *)a, *y = (const uint8_t *)b;
    for (; n; n--, x++, y++) {
        if (*x != *y) return *x < *y ? -1 : 1;
        FASCON__NO_IDIOM();
    }
    return 0;
}
#endif /* FASCON2_FREESTANDING */

#endif /* FASCON2_RT_IMPLEMENTATION */
#endif /* FASCON2_RT_H */
