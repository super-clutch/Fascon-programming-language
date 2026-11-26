/* genos.c — qemu-virt-aarch64 BSP の generic-os 拡張（bsp.c が FASCON2_GENERIC_OS のときだけ取り込む）
 * genos.c — the generic-os extension of the qemu-virt-aarch64 BSP (included by bsp.c only under FASCON2_GENERIC_OS)
 *
 *   1. 仮想番地の配置（T0SZ = 25 の 39 ビット、レベル 1 は 1 エントリ 1 GiB） / the virtual layout (39-bit, 1 GiB per L1 entry)
 *        0x00_0000_0000..0x00_0020_0000  守り（未マップ: ヌルポインタは Data Abort） / guard (unmapped: null → Data Abort)
 *        0x00_0020_0000..0x00_4000_0000  装置（恒等。BSP 自身の UART・GIC） / devices (identity; the BSP's own UART, GIC)
 *        0x00_4000_0000..0x00_8000_0000  RAM（2 MiB ブロック。守りページのところだけ 4 KiB のレベル 3 に分ける）
 *                                        RAM (2 MiB blocks; split into 4 KiB level-3 pages only around guard pages)
 *        0x50_0000_0000..                共有アリーナの予約域（2 MiB ずつ要求時に写す） / reserved for shared arenas (2 MiB on demand)
 *        0x60_0000_0000..0x60_4000_0000  物理 0〜1 GiB の装置の写し（mmio と bind_mmio はここを通る） / the device mirror
 *   2. PAN = 1（SPAN = 0 で例外のたびに 1）。ユーザーメモリは LDTRB / STTRB で写し、失敗は __ex_table の飛び先へ
 *      PAN = 1 (SPAN = 0 sets it on every exception); user memory is copied with LDTRB / STTRB, faults go to the __ex_table fixups
 *   3. PAC（APIA 鍵・SCTLR.EnIA）と BTI（SCTLR.BT1・ページの GP ビット）。コードは -mbranch-protection=standard で翻訳する
 *      PAC (the APIA key, SCTLR.EnIA) and BTI (SCTLR.BT1, the GP page bit); code is built with -mbranch-protection=standard
 *   4. TLB の撃ち落とし: 自分のコアだけならローカルの TLBI、ほかのコアには SGI 1 番でピンポイントに頼み、受けた側が
 *      ローカルの TLBI をして返事をする（待つ側は自分の郵便受けも見るので、互いに頼み合っても止まらない）
 *      TLB shootdown: a local TLBI when only this core is involved; other cores are asked pinpoint with SGI 1 and answer after
 *      their own local TLBI (a waiter also serves its own mailbox, so mutual requests cannot deadlock)
 *   5. バス列挙: QEMU が 0x4000_0000 に置く DTB を読み、装置の reg を bind_mmio の窓に登録、PCIe（ECAM）の BAR を割り当てる
 *      bus enumeration: read the DTB QEMU puts at 0x4000_0000, register device regs as bind_mmio windows, assign PCIe (ECAM) BARs */

#define GENOS_MIRROR FASCON2_MMIO_MIRROR
#define GENOS_ARENA_VA FASCON2_ARENA_VA_BASE
#define GENOS_ARENA_SPAN UINT64_C(0x1000000000) /* 64 GiB */
#define PTE_GP (1ull << 50)                     /* BTI の守られたページ / a BTI guarded page */
#define PTE_AP_RO (2ull << 6)                    /* AP[2:1] = 10: EL1 読み取りのみ / EL1 read-only */
#define PTE_AP_EL0 (1ull << 6)                   /* AP[2:1] = 01: EL0 からも読み書き / read-write from EL0 too */
#define PTE_PAGE 0x3ull                          /* レベル 3 のページ記述子 / a level-3 page descriptor */

static uint64_t genos_l2_low[512] __attribute__((aligned(4096)));
static uint64_t genos_l3_pool[24][512] __attribute__((aligned(4096)));
static uint32_t genos_l3_used;
static uint64_t genos_l2_va[8][512] __attribute__((aligned(4096)));
static uint32_t genos_l2_va_used;
static volatile uint32_t genos_pt_lock;
static uint64_t genos_guards[16];
static uint32_t genos_nguards;
static uint32_t genos_feat_pan, genos_feat_pac, genos_feat_bti;
static uint64_t genos_pool_next, genos_pool_end; /* アリーナを伸ばす 2 MiB ページの置き場（RAM の上の方） / the 2 MiB page pool */
static uint64_t genos_arena_maps, genos_arena_faults;
static uint32_t genos_rx_pages, genos_ro_pages, genos_rwx_pages, genos_split_fail;
static uint32_t genos_online = 1u; /* SGI を受けられるコアのビット / cores that can take SGIs */
static uint8_t genos_probe[4096] __attribute__((aligned(4096))); /* PAN の自己試験のページ / the PAN self-test page */
extern char __text_end[], __data_start[], __kstacks_start[], __kstacks_end[];

static void genos_tlbi_all(void) { __asm__ __volatile__("dsb ishst\n tlbi vmalle1is\n dsb ish\n isb" ::: "memory"); }

/* 2 MiB ブロックを 4 KiB のレベル 3 表へ分ける。MMU を有効にする前（genos_mmu_setup）にだけ呼ぶので、break-before-make は
 * 要らない（TLB にはまだ何も無い）。実行中は L3 の項目を「有効 → 無効」にするだけ（これは BBM なしで許される）
 * split a 2 MiB block into a level-3 table of 4 KiB pages. Only called before the MMU is on (genos_mmu_setup), so no
 * break-before-make is needed (the TLB holds nothing yet). At run time L3 entries only go "valid → invalid", which is
 * allowed without BBM */
static uint64_t *genos_split(uint64_t va) {
    if (va < RAM_BASE || va >= RAM_BASE + 0x40000000ull) return 0;
    const uint32_t i = (uint32_t)((va - RAM_BASE) >> 21);
    if ((l2_ram[i] & 3u) == PTE_TABLE) return (uint64_t *)(uintptr_t)(l2_ram[i] & 0x0000FFFFFFFFF000ull);
    if (genos_l3_used >= 24u) {
        genos_split_fail++;
        return 0;
    }
    uint64_t *l3 = genos_l3_pool[genos_l3_used++];
    const uint64_t blk = l2_ram[i];
    const uint64_t pa = blk & 0x0000FFFFFFE00000ull;
    const uint64_t attrs = blk & ~0x0000FFFFFFE00000ull & ~3ull & ~PTE_CONT;
    for (uint32_t k = 0; k < 512; k++) l3[k] = (pa + ((uint64_t)k << 12)) | attrs | PTE_PAGE;
    l2_ram[i] = (uint64_t)(uintptr_t)l3 | PTE_TABLE;
    return l3;
}
static uint64_t *genos_l3_of(uint64_t va) {
    if (va < RAM_BASE || va >= RAM_BASE + 0x40000000ull) return 0;
    const uint64_t e = l2_ram[(va - RAM_BASE) >> 21];
    return (e & 3u) == PTE_TABLE ? (uint64_t *)(uintptr_t)(e & 0x0000FFFFFFFFF000ull) : 0;
}

/* 4 KiB の守りページ（未マップ）。L3 に分けてあるブロックの中だけ（起動時に分けた範囲: カーネルスタック・タスクスタック）
 * a 4 KiB guard page (unmapped); only inside blocks already split at boot (kernel stacks, task stacks) */
void genos_guard_page(uint64_t va) {
    va &= ~0xFFFull;
    const uint64_t im = fascon_plat_irq_save();
    f20_bitlock(&genos_pt_lock);
    uint64_t *l3 = genos_l3_of(va);
    int dup = 0;
    for (uint32_t k = 0; k < genos_nguards; k++)
        if (genos_guards[k] == va) dup = 1;
    if (l3 && !dup) {
        l3[(va >> 12) & 511u] = 0;
        __asm__ __volatile__("dsb ishst\n tlbi vaae1is, %0\n dsb ish\n isb" : : "r"(va >> 12) : "memory");
        if (genos_nguards < 16u) genos_guards[genos_nguards++] = va;
    }
    f20_bitunlock(&genos_pt_lock);
    fascon_plat_irq_restore(im);
}

/* ページ表: bsp.c の mmu_enable から（MMU を有効にする前） / the page tables, from bsp.c's mmu_enable (before the MMU is on)
 *   W^X: カーネル像のブロックを L3 に分け、.text は読み取り専用 + 実行可 + GP（BTI）、.rodata は読み取り専用 + 実行禁止、
 *        残り（.data・.bss・スタック・ヒープ・アリーナ）は読み書き + 実行禁止。RAM の他のブロックも実行禁止
 *   W^X: the kernel image blocks are split into L3; .text is read-only + executable + GP (BTI), .rodata read-only +
 *        execute-never, the rest (.data, .bss, stacks, heap, arenas) read-write + execute-never; all other RAM blocks
 *        are execute-never too */
static void genos_mmu_setup(void) {
    /* 仮想 0〜1 GiB: 先頭 2 MiB は守り、残りは装置の恒等 / virtual 0-1 GiB: the first 2 MiB is a guard, the rest devices */
    genos_l2_low[0] = 0;
    for (uint64_t i = 1; i < 512; i++) genos_l2_low[i] = (i << 21) | PTE_VALID | PTE_ATTR(0) | PTE_AF | PTE_PXN | PTE_UXN;
    l1_table[0] = (uint64_t)(uintptr_t)genos_l2_low | PTE_TABLE;
    l1_table[1] = (uint64_t)(uintptr_t)l2_ram | PTE_TABLE;
    for (uint64_t i = 0; i < 512; i++) {
        const uint64_t pa = RAM_BASE + i * BLOCK_2M;
        if (in_framebuffer(pa)) {
            l2_ram[i] = pa | PTE_VALID | PTE_ATTR(2) | PTE_AF | PTE_SH_OUTER | PTE_UXN | PTE_PXN;
        } else {
            l2_ram[i] = pa | PTE_VALID | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER | PTE_UXN | PTE_PXN;
        }
    }
    /* カーネル像（.text から .data の先頭まで）のブロックを分けて、ページごとに権限を付ける
     * split the blocks of the kernel image (.text up to the start of .data) and give each page its permissions */
    const uint64_t ks = (uint64_t)(uintptr_t)__kernel_start & ~0xFFFull;
    const uint64_t te = ((uint64_t)(uintptr_t)__text_end + 0xFFFull) & ~0xFFFull;
    const uint64_t ds = (uint64_t)(uintptr_t)__data_start;
    for (uint64_t va = ks; va < ((ds + 0xFFFull) & ~0xFFFull); va += 4096u) {
        uint64_t *l3 = genos_split(va);
        if (!l3) break;
        uint64_t *e = &l3[(va >> 12) & 511u];
        const int has_data = va + 4096u > ds;
        if (va < te && !has_data) {
            *e = (*e & ~PTE_PXN) | PTE_AP_RO | PTE_GP; /* RX + BTI */
            genos_rx_pages++;
        } else if (va < te) {
            *e = (*e & ~PTE_PXN) | PTE_GP; /* .text と .data が同じページ（W^X の例外。数えて報告）/ shared page, reported */
            genos_rwx_pages++;
        } else if (!has_data) {
            *e |= PTE_AP_RO; /* .rodata */
            genos_ro_pages++;
        }
    }
    /* 守りページを置くブロック: カーネルスタック・タスクスタック・自己試験のページ / blocks that get guard pages */
    extern char __stack_top[], __smp_stack_top[];
    (void)genos_split((uint64_t)(uintptr_t)__stack_top - 0x20000u);
#if defined(FASCON2_SMP)
    for (uint32_t c = 1; c <= 3; c++) (void)genos_split((uint64_t)(uintptr_t)__smp_stack_top - (uint64_t)c * 0x20000u);
#endif
    for (uint64_t va = (uint64_t)(uintptr_t)__kstacks_start & ~(BLOCK_2M - 1u); va < (uint64_t)(uintptr_t)__kstacks_end; va += BLOCK_2M) (void)genos_split(va);
    uint64_t *pl3 = genos_split((uint64_t)(uintptr_t)genos_probe);
    if (pl3) pl3[((uint64_t)(uintptr_t)genos_probe >> 12) & 511u] |= PTE_AP_EL0; /* EL0 から読み書きできるページ / EL0-accessible */
    /* 物理 0〜1 GiB の装置を高位へ写す / mirror the physical 0-1 GiB of devices high */
    l1_table[GENOS_MIRROR >> 30] = 0x0ull | PTE_VALID | PTE_ATTR(0) | PTE_AF | PTE_PXN | PTE_UXN;
    mmu_ram_1g = 0;
    /* アリーナを伸ばすページの置き場: RAM の最後の 32 MiB（カーネル像の後ろでなければ使わない）
     * the arena page pool: the last 32 MiB of RAM (used only when it lies past the kernel image) */
    const uint64_t ram_end = RAM_BASE + 0x10000000ull; /* QEMU -m 256M */
    genos_pool_next = ram_end - 0x2000000ull;
    genos_pool_end = ram_end;
    if (genos_pool_next < (uint64_t)(uintptr_t)__kernel_end) genos_pool_next = genos_pool_end;
}

/* 共有アリーナの予約域を写す: [base, base + bytes) の 2 MiB ページを 0 で埋めて写す（要求元のコアのノード。QEMU は 1 ノード）
 * map the reserved range of a shared arena: zero-fill and map the 2 MiB pages of [base, base + bytes) (from the requesting
 * core's node; QEMU has one node) */
void fascon_plat_arena_grow(void *base, uint64_t bytes) {
    const uint64_t b = (uint64_t)(uintptr_t)base;
    if (b < GENOS_ARENA_VA || b >= GENOS_ARENA_VA + GENOS_ARENA_SPAN) return; /* .bss のアリーナ（もう写っている）/ a .bss arena */
    const uint64_t im = fascon_plat_irq_save();
    f20_bitlock(&genos_pt_lock);
    for (uint64_t va = b & ~(BLOCK_2M - 1u); va < b + bytes; va += BLOCK_2M) {
        uint64_t *l1e = &l1_table[va >> 30];
        if ((*l1e & 3u) != PTE_TABLE) {
            if (genos_l2_va_used >= 8u) break;
            uint64_t *l2 = genos_l2_va[genos_l2_va_used++];
            for (uint32_t k = 0; k < 512; k++) l2[k] = 0;
            __asm__ __volatile__("dsb ishst" ::: "memory");
            *l1e = (uint64_t)(uintptr_t)l2 | PTE_TABLE;
            bsp_vm_sync_l1((uint32_t)((va >> 30) & 511u)); /* タスクのアドレス空間にも / into task address spaces too */
        }
        uint64_t *l2 = (uint64_t *)(uintptr_t)(*l1e & 0x0000FFFFFFFFF000ull);
        uint64_t *e = &l2[(va >> 21) & 511u];
        if (*e & PTE_VALID) continue;
        if (genos_pool_next + BLOCK_2M > genos_pool_end) {
            f20_bitunlock(&genos_pt_lock);
            fascon_plat_irq_restore(im);
            fascon_plat_kernel_panic(FASCON_PANIC_OOM, "generic-os: the arena page pool is exhausted");
        }
        const uint64_t pa = genos_pool_next;
        genos_pool_next += BLOCK_2M;
        for (uint64_t k = 0; k < BLOCK_2M; k += 8) *(volatile uint64_t *)(uintptr_t)(pa + k) = 0; /* 恒等写像で 0 に / zero via the identity map */
        __asm__ __volatile__("dsb ishst" ::: "memory");
        *e = pa | PTE_VALID | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER | PTE_UXN | PTE_PXN;
        __asm__ __volatile__("dsb ishst\n tlbi vaae1is, %0\n dsb ish\n isb" : : "r"(va >> 12) : "memory");
        genos_arena_maps++;
    }
    f20_bitunlock(&genos_pt_lock);
    fascon_plat_irq_restore(im);
}

/* 物理番地 → 高位の写し（bind_mmio が使う） / physical → the high mirror (used by bind_mmio) */
uintptr_t fascon_plat_mmio_va(uint64_t phys) { return (uintptr_t)(phys + GENOS_MIRROR); }

/* ───── ユーザーメモリ: PAN = 1 のまま LDTRB / STTRB。失敗は __ex_table の飛び先が err = 1 にして続きへ戻る ─────
 * user memory: LDTRB / STTRB with PAN = 1; on a fault the __ex_table fixup sets err = 1 and resumes after the access */
/* 例外表（__ex_table）の引き方は bsp.c の bsp_find_fixup / the exception table is looked up by bsp_find_fixup in bsp.c */

static inline uint32_t genos_ldtrb(uint64_t ua, uint8_t *out) {
    uint64_t v = 0;
    uint32_t err = 0;
    __asm__ __volatile__("1: ldtrb %w0, [%2]\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: mov %w1, #1\n b 2b\n.popsection\n"
                         : "+r"(v), "+r"(err)
                         : "r"(ua)
                         : "memory");
    *out = (uint8_t)v;
    return err;
}
static inline uint32_t genos_sttrb(uint64_t ua, uint8_t v) {
    uint32_t err = 0;
    __asm__ __volatile__("1: sttrb %w1, [%2]\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: mov %w0, #1\n b 2b\n.popsection\n"
                         : "+r"(err)
                         : "r"((uint64_t)v), "r"(ua)
                         : "memory");
    return err;
}
uint64_t fascon_copy_from_user(void *dst, uint64_t uaddr, uint64_t n) {
    if (!fascon_user_range_ok(uaddr, n)) return n;
    for (uint64_t i = 0; i < n; i++) {
        if (genos_ldtrb(uaddr + i, (uint8_t *)dst + i)) return n - i;
    }
    return 0;
}
uint64_t fascon_copy_to_user(uint64_t uaddr, const void *src, uint64_t n) {
    if (!fascon_user_range_ok(uaddr, n)) return n;
    for (uint64_t i = 0; i < n; i++) {
        if (genos_sttrb(uaddr + i, ((const uint8_t *)src)[i])) return n - i;
    }
    return 0;
}
/* 範囲の検査は番地の形だけ（中身は読まない。写すときの例外が本当の検査）: いまの EL0 タスクのユーザーメモリの中か
 * the range check looks at the address only (the fault during the copy is the real check): inside the current EL0
 * task's user memory */
bool fascon_user_range_ok(uint64_t uaddr, uint64_t n) { return bsp_user_range(uaddr, n) != 0; }

/* 1 バイトを普通の LDRB で読む（例外表つき）。PAN の自己試験だけが使う / one plain LDRB with an exception-table entry,
 * used only by the PAN self-test */
static inline uint32_t genos_ldrb_probe(uint64_t a) {
    uint64_t v = 0;
    uint32_t err = 0;
    __asm__ __volatile__("1: ldrb %w0, [%2]\n"
                         "2:\n"
                         ".pushsection __ex_table, \"a\"\n.balign 8\n.quad 1b, 3f\n.popsection\n"
                         ".pushsection .text.fixup, \"ax\"\n3: mov %w1, #1\n b 2b\n.popsection\n"
                         : "+r"(v), "+r"(err)
                         : "r"(a)
                         : "memory");
    (void)v;
    return err;
}

/* EL1 の同期例外（SVC 以外。例外表の飛び先とユーザーページの要求時の写しは bsp.c の bsp_el1_fault が先に見る）:
 * アリーナの要求時の写し・守りページの報告。戻り値 = 復帰するフレーム
 * a synchronous EL1 exception (not an SVC; exception-table fixups and on-demand user pages are handled first by
 * bsp_el1_fault in bsp.c): demand mapping of arenas, guard-page reports; returns the frame to restore */
static f20_trapframe_t *genos_el1_fault(f20_trapframe_t *frame, uint64_t esr, uint64_t far) {
    const uint64_t ec = esr >> 26;
    if (ec == 0x25u || ec == 0x24u) { /* データアボート / a data abort */
        if (far >= GENOS_ARENA_VA && far < GENOS_ARENA_VA + GENOS_ARENA_SPAN) {
            genos_arena_faults++;
            fascon_plat_arena_grow((void *)(uintptr_t)(far & ~(BLOCK_2M - 1u)), BLOCK_2M); /* 予約域の要求時の 0 ページ / demand-zero */
            return frame;
        }
        if (far < BLOCK_2M) {
            puts_raw("\n[genos] GUARD: access to the null region (virtual 0x0..0x200000) far=");
            put_hex(far);
            puts_raw(" pc=");
            put_hex(frame->elr);
            puts_raw("\n");
            fascon_plat_kernel_panic(FASCON_PANIC_BOUNDS, "generic-os guard page: null-pointer access");
        }
        for (uint32_t k = 0; k < genos_nguards; k++) {
            if ((far & ~0xFFFull) == genos_guards[k]) {
                puts_raw("\n[genos] GUARD: access to a guard page ");
                put_hex(genos_guards[k]);
                puts_raw(" pc=");
                put_hex(frame->elr);
                puts_raw("\n");
                fascon_plat_kernel_panic(FASCON_PANIC_TASK, "generic-os guard page: stack overflow");
            }
        }
    }
    if (ec == 0x0Du) {
        puts_raw("\n[genos] BTI: branch target exception pc=");
        put_hex(frame->elr);
        puts_raw("\n");
    }
    if (ec == 0x1Cu) {
        puts_raw("\n[genos] PAC: pointer authentication failure pc=");
        put_hex(frame->elr);
        puts_raw("\n");
    }
    bsp_sync_exception(esr, frame->elr, far);
    return frame;
}

/* カーネルスタックが守りページへ溢れた（start.S が保存フレームを積む前に AT で見つけ、コアごとの非常用スタックへ移ってから
 * 呼ぶ）。戻らない
 * a kernel stack ran into its guard page (start.S finds it with AT before pushing the frame and calls this on the
 * per-core emergency stack); never returns */
uint8_t bsp_emerg_stack[4][4096] __attribute__((aligned(16)));
FASCON_NORETURN void bsp_stack_overflow(uint64_t far, uint64_t elr, uint64_t sp) {
    puts_raw("\n[genos] GUARD: kernel stack overflow into a guard page (sp=");
    put_hex(sp);
    puts_raw(" far=");
    put_hex(far);
    puts_raw(" pc=");
    put_hex(elr);
    puts_raw(")\n");
    fascon_plat_kernel_panic(FASCON_PANIC_STACK, "generic-os guard page: kernel stack overflow");
}

/* ───── PAN・PAC・BTI（コアごと）/ PAN, PAC and BTI (per core) ───── */
static uint64_t genos_key_lo, genos_key_hi;
/* この関数自身は分岐保護なしで翻訳する（ここで EnIA を立てるので、入口で署名していない戻り先を戻りで検証させない）
 * this function itself is built without branch protection (it sets EnIA, so its own return must not be authenticated) */
__attribute__((noinline, target("branch-protection=none"))) static void genos_cpu_features(uint32_t core) {
    uint64_t mmfr1, isar1, pfr1, sctlr;
    __asm__ __volatile__("mrs %0, id_aa64mmfr1_el1" : "=r"(mmfr1));
    __asm__ __volatile__("mrs %0, id_aa64isar1_el1" : "=r"(isar1));
    __asm__ __volatile__("mrs %0, S3_0_C0_C4_1" : "=r"(pfr1)); /* ID_AA64PFR1_EL1 */
    genos_feat_pan = ((mmfr1 >> 20) & 0xFu) != 0u;
    genos_feat_pac = (((isar1 >> 4) & 0xFu) | ((isar1 >> 8) & 0xFu)) != 0u; /* APA / API */
    genos_feat_bti = (pfr1 & 0xFu) != 0u;
    __asm__ __volatile__("mrs %0, sctlr_el1" : "=r"(sctlr));
    if (genos_feat_pan) {
        sctlr &= ~(1ull << 23);                           /* SPAN = 0: 例外のたびに PAN = 1 / PAN set on every exception */
        __asm__ __volatile__(".inst 0xd500419f" ::: "memory"); /* msr pan, #1 */
    }
    if (genos_feat_pac) {
        if (core == 0) {
            uint64_t t;
            __asm__ __volatile__("mrs %0, cntvct_el0" : "=r"(t));
            genos_key_lo = t * 0x9E3779B97F4A7C15ull ^ 0xD6E8FEB86659FD93ull;
            genos_key_hi = (t << 17 | t >> 47) * 0xBF58476D1CE4E5B9ull ^ 0x94D049BB133111EBull;
        }
        __asm__ __volatile__("msr S3_0_C2_C1_0, %0\n msr S3_0_C2_C1_1, %1" : : "r"(genos_key_lo), "r"(genos_key_hi)); /* APIAKeyLo/Hi */
        sctlr |= 1ull << 31;                                                                                  /* EnIA */
    }
    /* BTI の強制はページの GP ビット（.text のページ）。BT1 = 1 で PACIASP を BR（x16/x17 以外）の着地点として認めない
     * BTI is enforced by the GP bit of the .text pages; BT1 = 1 also stops PACIASP from being a landing pad for BTYPE 11 */
    if (genos_feat_bti) sctlr |= 1ull << 36;
    __asm__ __volatile__("msr sctlr_el1, %0\n isb" : : "r"(sctlr) : "memory");
}

/* ───── TLB の撃ち落とし（SGI 1 番）/ TLB shootdown (SGI 1) ───── */
#define GENOS_SGI_TLB 1u
static struct {
    volatile uint64_t va; /* (ASID << 48) | (va >> 12) の形の TLBI の引数 / the TLBI operand (ASID << 48) | (va >> 12) */
    volatile uint32_t req; /* 頼みの通し番号 / request sequence */
    volatile uint32_t ack;
    uint8_t pad_[F20_DESTRUCTIVE_SIZE - 16u];
} genos_mbox[FASCON2_MAX_CPUS] __attribute__((aligned(F20_DESTRUCTIVE_SIZE)));
static uint64_t genos_tlb_local_n, genos_tlb_ipi_n, genos_tlb_served_n;

static inline void genos_tlbi_local(uint64_t op) {
    /* このコアだけ（Non-shareable）、その ASID の VA だけ: tlbi vae1 + dsb nsh + isb（ASID 0 = 大域の項目は vaae1）
     * this core only (non-shareable), only the VA of that ASID: tlbi vae1 + dsb nsh + isb (ASID 0 = global entries: vaae1) */
    if ((op >> 48) == 0u) {
        __asm__ __volatile__("dsb nshst\n tlbi vaae1, %0\n dsb nsh\n isb" : : "r"(op) : "memory");
    } else {
        __asm__ __volatile__("dsb nshst\n tlbi vae1, %0\n dsb nsh\n isb" : : "r"(op) : "memory");
    }
}
static void genos_serve_mbox(uint32_t me) {
    const uint32_t r = __atomic_load_n(&genos_mbox[me].req, __ATOMIC_ACQUIRE);
    if (r != genos_mbox[me].ack) {
        genos_tlbi_local(genos_mbox[me].va);
        __atomic_fetch_add(&genos_tlb_served_n, 1u, __ATOMIC_RELAXED);
        __atomic_store_n(&genos_mbox[me].ack, r, __ATOMIC_RELEASE);
    }
}
/* 送り手は 1 度に 1 つ（割り込みを止めて取る全体のロック）。郵便受けの引数は 1 つなので、同じコアへの同時の頼みが互いを
 * 上書きしないように。待つ間も自分の郵便受けを読む / one sender at a time (a global lock taken with interrupts masked), so
 * simultaneous requests to the same core never overwrite the single operand of its mailbox; a waiting core keeps serving
 * its own mailbox */
static volatile uint32_t genos_tlb_lock;
void fascon_tlb_flush_va(uint64_t asid, uint64_t va, uint64_t cpu_mask) {
    /* 該当する ASID の VA だけを掃く（ほかのプロセスの TLB の項目は残る） / flush only the VA of that ASID (the TLB entries
     * of other processes stay) */
    const uint64_t op = ((asid & 0xFFFFu) << 48) | ((va >> 12) & 0xFFFFFFFFFFull);
    const uint32_t me = fascon_cpu_this();
    const uint64_t self = 1ull << me;
    const uint64_t others = cpu_mask & ~self & (uint64_t)__atomic_load_n(&genos_online, __ATOMIC_ACQUIRE); /* 動いているコアだけ / online cores only */
    if (cpu_mask & self || others == 0u) {
        genos_tlbi_local(op);
        __atomic_fetch_add(&genos_tlb_local_n, 1u, __ATOMIC_RELAXED);
    }
    if (others == 0u) return;
    const uint64_t im = fascon_plat_irq_save();
    while (__atomic_exchange_n(&genos_tlb_lock, 1u, __ATOMIC_ACQUIRE) != 0u) {
        genos_serve_mbox(me);
        FASCON_CPU_RELAX();
    }
    uint32_t want[FASCON2_MAX_CPUS] = {0};
    for (uint32_t c = 0; c < FASCON2_MAX_CPUS; c++) {
        if (!(others & (1ull << c))) continue;
        genos_mbox[c].va = op;
        want[c] = __atomic_add_fetch(&genos_mbox[c].req, 1u, __ATOMIC_ACQ_REL);
        __atomic_fetch_add(&genos_tlb_ipi_n, 1u, __ATOMIC_RELAXED);
    }
    __asm__ __volatile__("dsb ish" ::: "memory");
    MMIO32(GICD + 0xF00) = (uint32_t)((others & 0xFFu) << 16) | GENOS_SGI_TLB; /* GICD_SGIR: ターゲットの一覧へだけ / to the target list only */
    for (uint32_t c = 0; c < FASCON2_MAX_CPUS; c++) {
        if (!(others & (1ull << c))) continue;
        uint64_t spins = 0;
        while ((int32_t)(__atomic_load_n(&genos_mbox[c].ack, __ATOMIC_ACQUIRE) - want[c]) < 0) {
            genos_serve_mbox(me); /* 互いに頼み合っても止まらない / no deadlock when two cores ask each other */
            FASCON_CPU_RELAX();
            if (++spins == 400000000ull)
                fascon_plat_kernel_panic(FASCON_PANIC_SMP, "TLB shootdown: a core did not answer the SGI (spinning with interrupts masked on a lock this core holds?)");
        }
    }
    __atomic_store_n(&genos_tlb_lock, 0u, __ATOMIC_RELEASE);
    fascon_plat_irq_restore(im);
}
uint64_t fascon_tlb_local_count(void) { return genos_tlb_local_n; }
uint64_t fascon_tlb_ipi_count(void) { return genos_tlb_ipi_n; }

/* bsp_irq から: 16 未満の INTID（SGI）はここで受ける / from bsp_irq: INTIDs below 16 (SGIs) are taken here */
static void genos_sgi(uint32_t intid) {
    if (intid == GENOS_SGI_TLB) genos_serve_mbox(fascon_cpu_this());
}

/* GIC のこのコアの窓口を開き、SGI を受けられるようにする / open this core's GIC interface so it can take SGIs */
static void genos_gic_core_on(uint32_t core) {
    __atomic_fetch_or(&genos_online, 1u << core, __ATOMIC_ACQ_REL);
    MMIO8(GICD + 0x400 + GENOS_SGI_TLB) = 0x80u;          /* SGI の優先度（コアごとの写し）/ SGI priority (banked) */
    MMIO32(GICD + 0x100) = 1u << GENOS_SGI_TLB;           /* SGI 1 を許可（コアごと）/ enable SGI 1 (banked) */
    MMIO32(GICC + 0x004) = 0xFFu;
    MMIO32(GICC + 0x000) = 1u;
    MMIO32(GICD + 0x000) = 1u;
}

/* ───── バス列挙: DTB（FDT）と PCIe の ECAM / bus enumeration: the DTB (FDT) and PCIe ECAM ───── */
static uint32_t genos_be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }
static uint32_t genos_dt_devices, genos_pci_devices;
static uint64_t genos_ecam_base, genos_ecam_size;
static uint64_t genos_pci_mmio, genos_pci_mmio_end;

static int genos_streq(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}
static int genos_compat_has(const uint8_t *v, uint32_t len, const char *want) {
    uint32_t i = 0;
    while (i < len) {
        if (genos_streq((const char *)v + i, want)) return 1;
        while (i < len && v[i]) i++;
        i++;
    }
    return 0;
}

/* FDT の構造ブロックを歩き、reg を持つ装置を窓に登録する（#address-cells = #size-cells = 2 の QEMU virt を前提）。
 * libfdt は後から足したプロパティを前に置くので、ノードの終わりでまとめて判断する
 * walk the FDT structure block and register devices with a reg as windows (assumes QEMU virt: #address/#size-cells = 2).
 * libfdt puts properties added later in front, so each node is decided at its end */
static uint64_t genos_be64(const uint8_t *p) { return (uint64_t)genos_be32(p) << 32 | genos_be32(p + 4); }
static void genos_dtb_scan(uint64_t dtb) {
    const uint8_t *h = (const uint8_t *)(uintptr_t)dtb;
    if (genos_be32(h) != 0xD00DFEEDu) {
        puts_raw("[genos] no DTB at 0x40000000 (bus enumeration skipped; board.toml windows only)\n");
        return;
    }
    const uint8_t *st = h + genos_be32(h + 8), *strs = h + genos_be32(h + 12);
    const uint32_t size = genos_be32(h + 36);
    uint32_t off = 0;
    int depth = 0;
    const uint8_t *compat = 0, *reg = 0, *ranges = 0;
    uint32_t compat_len = 0, reg_len = 0, ranges_len = 0;
    while (off + 4u <= size) {
        const uint32_t tok = genos_be32(st + off);
        off += 4;
        if (tok == 1u) { /* BEGIN_NODE */
            while (st[off]) off++;
            off = (off + 4u) & ~3u;
            depth++;
            if (depth == 2) {
                compat = reg = ranges = 0;
                compat_len = reg_len = ranges_len = 0;
            }
        } else if (tok == 2u) { /* END_NODE */
            if (depth == 2 && reg && reg_len >= 16u) {
                const uint64_t base = genos_be64(reg), sz = genos_be64(reg + 8);
                if (compat && genos_compat_has(compat, compat_len, "pci-host-ecam-generic")) {
                    genos_ecam_base = base;
                    genos_ecam_size = sz;
                    /* (flags, pci addr × 2, cpu addr × 2, size × 2) の 7 セルの組から 32 ビットの MMIO の窓を取る
                     * take the 32-bit MMIO window from the 7-cell tuples */
                    for (uint32_t k = 0; ranges && k + 28u <= ranges_len; k += 28u) {
                        const uint32_t sp = genos_be32(ranges + k) >> 24 & 3u;
                        const uint64_t cpu = genos_be64(ranges + k + 12), rs = genos_be64(ranges + k + 20);
                        if (sp == 2u && cpu < 0x40000000ull) {
                            genos_pci_mmio = cpu;
                            genos_pci_mmio_end = cpu + rs;
                        }
                    }
                } else if (base < 0x40000000ull && sz != 0u && sz <= 0x10000000ull) {
                    fascon_mmio_register_window(base, sz);
                    genos_dt_devices++;
                }
            }
            depth--;
        } else if (tok == 3u) { /* PROP */
            const uint32_t len = genos_be32(st + off), name = genos_be32(st + off + 4);
            const uint8_t *val = st + off + 8;
            if (depth == 2) {
                const char *pn = (const char *)strs + name;
                if (genos_streq(pn, "compatible")) {
                    compat = val;
                    compat_len = len;
                } else if (genos_streq(pn, "reg")) {
                    reg = val;
                    reg_len = len;
                } else if (genos_streq(pn, "ranges")) {
                    ranges = val;
                    ranges_len = len;
                }
            }
            off += 8 + ((len + 3u) & ~3u);
        } else if (tok == 4u) { /* NOP */
        } else {
            break; /* END */
        }
    }
}

/* PCIe: バス 0 の機能を読み、BAR の大きさを測って 32 ビットの MMIO 窓から割り当て、窓として登録する
 * PCIe: read the functions of bus 0, size the BARs, assign them from the 32-bit MMIO window and register them as windows */
static void genos_pci_scan(void) {
    if (genos_ecam_base == 0 || genos_pci_mmio == 0) return;
    if (genos_ecam_base + genos_ecam_size > 0x40000000ull) {
        /* 高位の ECAM（0x40_1000_0000 など）は 1 GiB の恒等ブロックで写す / map a high ECAM with a 1 GiB identity block */
        const uint64_t g = genos_ecam_base >> 30;
        if (g < 512 && g != 0 && g != 1 && (l1_table[g] & 1u) == 0) {
            l1_table[g] = (g << 30) | PTE_VALID | PTE_ATTR(0) | PTE_AF | PTE_PXN | PTE_UXN;
            genos_tlbi_all();
        } else {
            return;
        }
    }
    uint64_t next = genos_pci_mmio;
    for (uint32_t dev = 0; dev < 32; dev++) {
        const uint64_t cfg = genos_ecam_base + ((uint64_t)dev << 15);
        const uint32_t id = MMIO32(cfg);
        if ((id & 0xFFFFu) == 0xFFFFu) continue;
        genos_pci_devices++;
        for (uint32_t bar = 0; bar < 6; bar++) {
            const uint64_t a = cfg + 0x10 + 4u * bar;
            const uint32_t old = MMIO32(a);
            if (old & 1u) continue; /* I/O の BAR / an I/O BAR */
            MMIO32(a) = 0xFFFFFFFFu;
            const uint32_t sz = ~(MMIO32(a) & ~0xFu) + 1u;
            if (sz == 0u || sz > 0x1000000u) {
                MMIO32(a) = old;
                continue;
            }
            next = (next + sz - 1u) & ~(uint64_t)(sz - 1u);
            if (next + sz > genos_pci_mmio_end) break;
            MMIO32(a) = (uint32_t)next;
            fascon_mmio_register_window(next, sz);
            next += sz;
            if (((old >> 1) & 3u) == 2u) bar++; /* 64 ビットの BAR は 2 つ分 / a 64-bit BAR uses two slots */
        }
        MMIO32(cfg + 4) = MMIO32(cfg + 4) | 0x2u; /* メモリ空間を有効に / enable memory space */
    }
}

/* ───── 起動 / start-up ───── */
/* 静的な守りページ: 起動コアと 2 番目以降のコアのカーネルスタックの一番下の 4 KiB / static guard pages: the lowest 4 KiB of
 * the boot core's and the secondary cores' kernel stacks */
static void genos_stack_guards(void) {
    extern char __stack_top[], __smp_stack_top[];
    genos_guard_page((uint64_t)(uintptr_t)__stack_top - 0x20000u);
#if defined(FASCON2_SMP)
    for (uint32_t c = 1; c <= 3; c++) genos_guard_page((uint64_t)(uintptr_t)__smp_stack_top - (uint64_t)c * 0x20000u);
#endif
}

/* 自己試験: 例外表（守りの番地 0x10 を LDTRB）と PAN（EL0 のページを普通の LDRB → 拒否、LDTRB / STTRB → 通る）
 * self-tests: the exception table (LDTRB from the guard address 0x10) and PAN (a plain LDRB of an EL0 page is refused,
 * LDTRB / STTRB pass) */
static void genos_selftest(void) {
    uint8_t b = 0;
    const uint32_t ex = genos_ldtrb(0x10u, &b);
    puts_raw("[genos] self-test: exception-table fixup on the null guard ");
    puts_raw(ex ? "ok" : "FAILED");
    if (genos_feat_pan) {
        const uint64_t a = (uint64_t)(uintptr_t)genos_probe;
        const uint32_t blocked = genos_ldrb_probe(a);
        const uint32_t w = genos_sttrb(a + 7u, 0x5Au);
        uint8_t r = 0;
        const uint32_t rd = genos_ldtrb(a + 7u, &r);
        puts_raw(", PAN: EL1 load of a user page ");
        puts_raw(blocked ? "blocked" : "NOT BLOCKED");
        puts_raw(", LDTRB/STTRB ");
        puts_raw(w == 0u && rd == 0u && r == 0x5Au ? "ok" : "FAILED");
    }
    puts_raw("\n");
}

/* bsp_main から（MMU の後）。この関数の中で EnIA が立つので、自分は戻りの検証をしない（branch-protection=none）
 * from bsp_main (after the MMU); EnIA is turned on inside, so this function itself does not authenticate its return */
__attribute__((noinline, target("branch-protection=none"))) static void genos_boot(void) {
    genos_cpu_features(0);
    genos_gic_core_on(0);
    genos_stack_guards();
    genos_dtb_scan(0x40000000ull);
    genos_pci_scan();
    puts_raw("[genos] generic-os: null guard 0x0..0x200000, MMIO mirror ");
    put_hex(GENOS_MIRROR);
    puts_raw(", arena VA ");
    put_hex(GENOS_ARENA_VA);
    puts_raw(", PAN ");
    puts_raw(genos_feat_pan ? "on" : "n/a");
    puts_raw(", PAC ");
    puts_raw(genos_feat_pac ? "on" : "n/a");
    puts_raw(", BTI ");
    puts_raw(genos_feat_bti ? "on" : "n/a");
    puts_raw("\n[genos] W^X: ");
    put_dec(genos_rx_pages);
    puts_raw(" text pages RX+GP, ");
    put_dec(genos_ro_pages);
    puts_raw(" rodata pages RO, ");
    put_dec(genos_rwx_pages);
    puts_raw(" shared pages; L3 tables ");
    put_dec(genos_l3_used);
    puts_raw(", stack guards ");
    put_dec(genos_nguards);
    puts_raw(", DTB devices ");
    put_dec(genos_dt_devices);
    puts_raw(", PCIe functions ");
    put_dec(genos_pci_devices);
    puts_raw("\n");
    genos_selftest();
}

/* 2 番目以降のコア: 同じ機能を立て、SGI を受けるために IRQ を開く / secondary cores: the same features, IRQs open for SGIs */
__attribute__((noinline, target("branch-protection=none"))) static void genos_secondary(uint32_t core) {
    genos_cpu_features(core);
    genos_gic_core_on(core);
    __asm__ __volatile__("msr daifclr, #2" ::: "memory");
}

/* タスクのスタック: フレームリージョン | 守りページ 4 KiB | スタック。守りを置けるだけの大きさがあるときだけ
 * a task stack: frame region | 4 KiB guard page | stack, when it is large enough for a guard */
static size_t genos_task_layout(uint8_t *base, size_t stack_size) {
    if (((uint64_t)(uintptr_t)base & 0xFFFu) != 0u || stack_size < 3u * 4096u) return 0;
    const size_t fr = (((stack_size - 4096u) / 4u) + 4095u) & ~(size_t)4095u;
    if (fr + 2u * 4096u > stack_size) return 0;
    genos_guard_page((uint64_t)(uintptr_t)(base + fr));
    return fr;
}

/* 終わりの報告（fascon_plat_exit の前） / the closing report (before fascon_plat_exit) */
static void genos_report(void) {
    puts_raw("[genos] arena 2 MiB maps ");
    put_dec(genos_arena_maps);
    puts_raw(" (demand faults ");
    put_dec(genos_arena_faults);
    puts_raw("), TLB local ");
    put_dec(genos_tlb_local_n);
    puts_raw(", shootdown IPIs ");
    put_dec(genos_tlb_ipi_n);
    puts_raw(" (served ");
    put_dec(genos_tlb_served_n);
    puts_raw(")\n");
}
