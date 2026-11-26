/*
 * user.c — Fascon 1.0 ユーザープログラム（EL0）のプラットフォーム層
 *          the platform layer of Fascon 1.0 user programs (EL0)
 *
 * EL0 には装置も特権レジスタも無い。外へ出る道はカーネルのシステムコールだけ:
 *   - print 系の出力は、ABI の `write(fd: u32, buf: &[u8])` の fd 1 へ（その宣言が無ければ捨てる）
 *   - main が戻ったとき・パニックのときは、ABI の `exit(code)` へ（無ければ未定義命令。カーネルの trap がタスクを終わらせる）
 * 生成 C が、ABI に合う宣言を見つけたときに fascon_user_console_write / fascon_user_exit を定義する。
 * EL0 has no devices and no privileged registers; the only way out is the kernel's system calls:
 *   - printing goes to fd 1 of the `write(fd: u32, buf: &[u8])` of the ABI (dropped when there is no such declaration)
 *   - when main returns or the program panics, the `exit(code)` of the ABI is called (an undefined instruction when there
 *     is none; the kernel's trap then ends the task)
 * The generated C defines fascon_user_console_write / fascon_user_exit when the ABI has matching declarations.
 */
#include "fascon2_rt.h"

void fascon2_main(void);
extern char __heap_start[], __heap_end[], __frame_start[], __frame_end[];

__attribute__((weak)) void fascon_user_console_write(const char *s, size_t n) {
    (void)s;
    (void)n;
}

__attribute__((weak)) void fascon_user_exit(int code) { (void)code; }

void fascon_plat_print(const char *s, size_t len) { fascon_user_console_write(s, len); }

FASCON_NORETURN void fascon_plat_exit(int code) {
    fascon_user_exit(code);
    for (;;) __asm__ __volatile__("udf #0"); /* exit が無い・戻ってきた: カーネルの trap に任せる / leave it to the kernel's trap */
}

FASCON_NORETURN void fascon_plat_halt(void) { fascon_plat_exit(0); }

static void user_puts(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    fascon_plat_print(s, n);
}

FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    char b[11] = "0x00000000";
    for (int i = 0; i < 8; i++) {
        const unsigned d = (code >> (28 - 4 * i)) & 0xfu;
        b[2 + i] = (char)(d < 10 ? '0' + d : 'a' + d - 10);
    }
    user_puts("\n*** USER PANIC ");
    user_puts(b);
    user_puts(": ");
    user_puts(msg);
    user_puts("\n");
    fascon_plat_exit(70);
}

uint32_t fascon_plat_cpu_id(void) { return 0u; }
uint32_t fascon_plat_cpu_count(void) { return 1u; }

uint64_t fascon_plat_timer_freq(void) {
    uint64_t v;
    __asm__ __volatile__("mrs %0, cntfrq_el0" : "=r"(v));
    return v;
}

uint64_t fascon_plat_timer_now(void) {
    uint64_t v;
    __asm__ __volatile__("isb; mrs %0, cntvct_el0" : "=r"(v));
    return v;
}

void fascon_user_start(void) {
    fascon_heap_init(__heap_start, (size_t)(__heap_end - __heap_start));
    fascon_frame_init(__frame_start, (size_t)(__frame_end - __frame_start));
    fascon2_main();
    fascon_plat_exit(0);
}
