/* bios.ld — Fascon 1.0 BSP: QEMU x86_64 の仮想 BIOS（-bios の像） / the virtual BIOS for QEMU x86_64 (the -bios image)
 *
 *   ROM（4 GiB の直下）: [カーネル像（RAM の 1 MiB へ写す）] [起動コード 64 KiB: 32 / 64 ビット … 16 ビット … リセットベクタ]
 *   ROM (right below 4 GiB): [kernel image (copied to RAM at 1 MiB)] [64 KiB boot code: 32 / 64-bit ... 16-bit ... reset vector]
 *   ROM_BASE はコンパイラが像の大きさから決めて書き込む（像の大きさは 64 KiB の倍数 = QEMU の決まり）
 *   the compiler fills in ROM_BASE from the size of the image (the image is a multiple of 64 KiB, as QEMU requires)
 *
 *   RAM: 0x1000〜0x6FFF 起動時のページ表、0x8000 ほかのコアのトランポリン、1 MiB〜 カーネル（.text .rodata 例外表 .data |
 *        .fascon_arena（2 MiB 境界）| .bss | .fascon_user（2 MiB 境界）| コアごとの TLS | コア 1〜3 のスタックとフレーム |
 *        スタック | own フレーム 1 MiB | own ヒープ 8 MiB）
 *   RAM: 0x1000-0x6FFF boot page tables, 0x8000 the trampoline of the other cores, from 1 MiB the kernel (.text .rodata
 *        exception table .data | .fascon_arena (2 MiB aligned) | .bss | .fascon_user (2 MiB aligned) | per-core TLS |
 *        stacks and frames of cores 1-3 | stack | own frame 1 MiB | own heap 8 MiB) */
ENTRY(reset_vector)
ROM_BASE = @ROM_BASE@;

SECTIONS
{
    . = 0x100000;
    __kernel_start = .;
    __image_start = .;
    .text : AT(ROM_BASE) { *(.text .text.*) }
    .rodata : AT(ROM_BASE + (ADDR(.rodata) - 0x100000)) { *(.rodata .rodata.*) }
    /* ユーザーメモリの写し（copy_from_user / copy_to_user）の例外表（命令の番地, 飛び先） / the exception table of user
     * copies (instruction address, fixup) */
    __ex_table : AT(ROM_BASE + (ADDR(__ex_table) - 0x100000)) ALIGN(8) { __start___ex_table = .; KEEP(*(__ex_table)) __stop___ex_table = .; }
    .data : AT(ROM_BASE + (ADDR(.data) - 0x100000)) ALIGN(16) { *(.data .data.*) . = ALIGN(16); }
    /* マルチコア: .tdata（初期値のあるコアごとのデータ）は使わない。並びは .tbss と同じ境界にして、TLS の区間の大きさが
     * .tbss そのものになるようにする / multi-core: .tdata (initialised per-core data) is not used; it is aligned like .tbss
     * so the TLS segment is exactly .tbss */
    .tdata : AT(ROM_BASE + (ADDR(.tdata) - 0x100000)) ALIGN(128) { *(.tdata .tdata.*) }
    __image_end = .;
    __image_size = __image_end - __image_start;
    __image_lma = ROM_BASE;
    /* ここから先は RAM の中だけの領域（NOLOAD）: 読み込み番地 = 仮想番地にして、ROM の番地へはみ出さないようにする
     * from here on, RAM-only areas (NOLOAD): load address = virtual address, so nothing spills into ROM addresses */
    .tbss (NOLOAD) : AT(ADDR(.tbss)) ALIGN(128) { *(.tbss .tbss.*) *(.tcommon) }
    ASSERT(SIZEOF(.tdata) == 0, "Fascon: initialized per-core data (.tdata) is not supported by this BSP")
    /* x86_64 の TLS（variant II）: ブロックはスレッドポインタ（FS の土台）の直前。BSP はコアごとのブロックの終わりを FS に
     * 載せる。大きさと境界は TLS の区間全体（.tdata の始まりから .tbss の終わりまで）で測る
     * x86_64 TLS (variant II): the block sits right below the thread pointer (the FS base); the BSP loads the end of the block
     * of each core into FS. Size and alignment are measured over the whole TLS segment (.tdata start to .tbss end) */
    __tls_memsz = (ADDR(.tbss) + SIZEOF(.tbss)) - ADDR(.tdata);
    __tls_align = MAX(ALIGNOF(.tdata), ALIGNOF(.tbss));
    .fascon_arena (NOLOAD) : AT(ADDR(.fascon_arena)) ALIGN(0x200000) {
        __arena_start = .;
        *(.bss.fascon_arena .bss.fascon_arena.*)
        . = ALIGN(16);
        __arena_end = .;
    }
    .bss (NOLOAD) : AT(ADDR(.bss)) ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }
    /* user_mem: タスクごとのユーザーメモリ（2 MiB 境界。起動時には 0 にしない） / per-task user memory (2 MiB aligned; not
     * zeroed at boot) */
    .fascon_user (NOLOAD) : AT(ADDR(.fascon_user)) ALIGN(0x200000) { *(.bss.fascon_user .bss.fascon_user.*) }
    /* コアごとの TLS のブロック（4 コアぶん。TCB の自己ポインタを置く 4 KiB の余白つき） / per-core TLS blocks (four cores,
     * each with 4 KiB of room for the TCB self pointer) */
    . = ALIGN(4096);
    .percpu (NOLOAD) : AT(ADDR(.percpu)) { __percpu_start = .; . += 4 * ALIGN(__tls_memsz + 4096, 4096); __percpu_end = .; }
    /* コア 1〜3 のスタック（128 KiB ずつ）とフレームリージョン（1 MiB ずつ） / stacks (128 KiB each) and frame regions
     * (1 MiB each) of cores 1 to 3 */
    . = ALIGN(4096);
    .smp_stack (NOLOAD) : AT(ADDR(.smp_stack)) { . += 0x20000 * 3; __smp_stack_top = .; }
    . = ALIGN(4096);
    .smp_frame (NOLOAD) : AT(ADDR(.smp_frame)) { __smp_frame_start = .; . += 0x100000 * 3; __smp_frame_end = .; }
    . = ALIGN(4096);
    .stack (NOLOAD) : AT(ADDR(.stack)) { . += 0x20000; __stack_top = .; }
    . = ALIGN(4096);
    .frame (NOLOAD) : AT(ADDR(.frame)) { __frame_start = .; . += 0x100000; __frame_end = .; }
    . = ALIGN(4096);
    .heap (NOLOAD) : AT(ADDR(.heap)) { __heap_start = .; . += 0x800000; __heap_end = .; }
    __kernel_end = .;
    ASSERT(__kernel_end <= 0x80000000, "Fascon: the kernel, arenas and user memory must end below 2 GiB (RAM of QEMU pc -m 2G)")

    .rom32 0xFFFF0000 : AT(0xFFFF0000) { *(.rom32) }
    .rom16 0xFFFFF000 : AT(0xFFFFF000) { *(.rom16) }
    .reset 0xFFFFFFF0 : AT(0xFFFFFFF0) { KEEP(*(.reset)) }
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
