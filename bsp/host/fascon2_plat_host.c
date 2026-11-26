/*
 * fascon2_plat_host.c — Fascon 1.0 の生成 C を PC（Linux / macOS / Windows）上で動かすプラットフォーム層
 * Platform layer to run Fascon 1.0 generated C on a PC (Linux / macOS / Windows).
 *
 *   cc -O2 -std=c11 -DFASCON2_HOSTED_MMIO -Ibuild build/app.fascon.c fascon2_plat_host.c -o app
 *   （`fascon run app.fsc` がこのコマンドを自動で実行します / `fascon run app.fsc` runs this for you）
 *
 * - 生成 C の MMIO リージョン表にある各リージョンの実体を、プロセス内のメモリに確保する（デバイスの代わりのただのメモリ）。
 *   FASCON2_HOSTED_MMIO のときだけ、生成 C は固定番地の代わりに fascon2_host_mmio_base[] の番地を使う
 *   （ベアメタルのビルドでは番地は定数のまま＝コストゼロ）。
 *   allocates every region of the generated MMIO region table in process memory (plain memory standing in for devices).
 *   Only under FASCON2_HOSTED_MMIO does the generated C use the addresses in fascon2_host_mmio_base[] instead of the
 *   fixed addresses (bare-metal builds keep the constant address = zero cost).
 * - 終了時・halt() 時に #[framebuffer] リージョンを PPM 画像（既定 fascon_screen.ppm、環境変数 FASCON_SCREENSHOT）へ保存
 *   at exit / halt() the #[framebuffer] region is saved as a PPM image (default fascon_screen.ppm, env FASCON_SCREENSHOT)
 * - デバイスのリージョン（UART など）は 0 で埋まったメモリ。受信待ちのループは「受信あり・値 0」を読んですぐ進む
 *   device regions (UART, ...) are zero-filled memory: a receive-poll loop reads "data ready, value 0" and moves on
 * - Linux では 1 MiB 以上のアリーナ（生成 C が 2 MiB 境界に置く）に madvise(MADV_HUGEPAGE) を掛け、最初に触る前に
 *   2 MiB の透過的ヒュージページを頼む（THP が "madvise" か "always" のとき有効。無効なら何も起きない）
 *   on Linux, arenas of 1 MiB or more (placed on a 2 MiB boundary by the generated C) get madvise(MADV_HUGEPAGE) before
 *   their first touch, asking for 2 MiB transparent huge pages (effective when THP is "madvise" or "always"; a no-op otherwise)
 */
#if defined(__linux__) && defined(FASCON2_HPC) && !defined(_GNU_SOURCE)
#define _GNU_SOURCE 1 /* --target hpc: sched_setaffinity・CPU_SET（-std=c11 でも） / sched_setaffinity, CPU_SET even with -std=c11 */
#endif
#if defined(__linux__) && !defined(_DEFAULT_SOURCE)
#define _DEFAULT_SOURCE 1 /* madvise / MADV_HUGEPAGE（-std=c11 でも） / madvise / MADV_HUGEPAGE even with -std=c11 */
#endif
#if defined(__APPLE__) && !defined(_XOPEN_SOURCE)
#define _XOPEN_SOURCE 700 /* ucontext（タスクの切り替え） / ucontext (task switching) */
#endif
#ifndef FASCON2_HOSTED_MMIO
#define FASCON2_HOSTED_MMIO 1
#endif
#include "fascon2_rt.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#if defined(__linux__)
#include <sys/mman.h>
#endif
#if !defined(_WIN32)
#include <unistd.h>
#endif

void fascon2_main(void);

void fascon_plat_print(const char *s, size_t len) { fwrite(s, 1, len, stdout); }

static int screen_saved = 0;

static void save_screen(void) {
    if (screen_saved) return;
    for (uint32_t i = 0; i < fascon2_mmio_region_count; i++) {
        const FasconMmioRegion *r = &fascon2_mmio_regions[i];
        if (!(r->flags & FASCON_MMIO_FRAMEBUFFER) || r->bpp != 32 || fascon2_host_mmio_base[i] == 0) continue;
        const char *path = getenv("FASCON_SCREENSHOT");
        if (path == NULL || path[0] == 0) path = "fascon_screen.ppm";
        FILE *f = fopen(path, "wb");
        if (f == NULL) {
            fprintf(stderr, "[host] cannot write %s\n", path);
            return;
        }
        fprintf(f, "P6\n%u %u\n255\n", (unsigned)r->width, (unsigned)r->height);
        const uint32_t *px = (const uint32_t *)fascon2_host_mmio_base[i];
        for (uint32_t k = 0; k < r->width * r->height; k++) {
            unsigned char rgb[3] = {(unsigned char)(px[k] >> 16), (unsigned char)(px[k] >> 8), (unsigned char)px[k]};
            fwrite(rgb, 1, 3, f);
        }
        fclose(f);
        screen_saved = 1;
        fprintf(stderr, "[host] framebuffer %s (%ux%u, device address %#lx) saved to %s\n", r->name, (unsigned)r->width, (unsigned)r->height,
                (unsigned long)r->base, path);
        return;
    }
}

FASCON_NORETURN void fascon_plat_kernel_panic(uint32_t code, const char *msg) {
    fflush(stdout);
    fprintf(stderr, "FASCON PANIC 0x%04x %s\n", (unsigned)code, msg ? msg : "");
    exit(70);
}

FASCON_NORETURN void fascon_plat_exit(int code) {
    fflush(stdout);
    save_screen();
    exit(code);
}

FASCON_NORETURN void fascon_plat_halt(void) {
    fflush(stdout);
    save_screen();
    exit(0);
}

/* ───── マルチコア（PC ではコア = スレッド） / multi-core (on the PC a core is a thread) ─────
 * コアごとの実体（ふつうのアリーナ・フレームリージョン・own ヒープ）は __thread なので、スレッドごとの実体になる。
 * コア 1〜3 のスレッドは最初の spawn_on で作り、関数が戻ると次の仕事を待つ（ベアメタルの BSP と同じ動き）。
 * Per-core instances (plain arenas, the frame region, the own heap) are __thread, so they become per-thread instances.
 * The threads of cores 1 to 3 are created by their first spawn_on and wait for the next job when the function returns
 * (the same behaviour as the bare-metal BSP). */
#if defined(FASCON2_SMP)
#if defined(_WIN32)
#include <windows.h>
#else
#include <pthread.h>
#include <sched.h>
#endif
static __thread uint32_t host_core;
static struct {
    void (*fn)(uint64_t);
    uint64_t arg;
    uint32_t busy;
    uint32_t started;
} host_cores[FASCON2_MAX_CPUS];

static void host_core_setup(uint32_t core) {
    host_core = core;
    fascon_smp_core_init(core);
    void *frame = malloc(1u << 20);
    if (frame != NULL) fascon_frame_init(frame, 1u << 20);
}

static void host_worker(uint32_t core) {
    host_core_setup(core);
    for (;;) {
        void (*fn)(uint64_t) = __atomic_load_n(&host_cores[core].fn, __ATOMIC_ACQUIRE);
        if (fn == NULL) {
            fascon_smp_relax();
#if defined(_WIN32)
            Sleep(0);
#else
            sched_yield();
#endif
            continue;
        }
        fn(host_cores[core].arg);
        __atomic_store_n(&host_cores[core].fn, (void (*)(uint64_t))NULL, __ATOMIC_RELEASE);
        __atomic_store_n(&host_cores[core].busy, 0u, __ATOMIC_RELEASE);
    }
}
#if defined(_WIN32)
static DWORD WINAPI host_thread(LPVOID p) {
    host_worker((uint32_t)(uintptr_t)p);
    return 0;
}
#else
static void *host_thread(void *p) {
    host_worker((uint32_t)(uintptr_t)p);
    return NULL;
}
#endif

#if defined(FASCON2_HPC)
/* --target hpc: このプロセスが使ってよい論理 CPU の数（コアの上限まで） / --target hpc: the logical CPUs this process may use
 * (up to the core limit) */
static void host_hpc_scan(void);
static int host_hpc_ncpu;
uint32_t fascon_plat_cpu_count(void) {
    host_hpc_scan();
    return (uint32_t)host_hpc_ncpu < FASCON2_MAX_CPUS ? (uint32_t)host_hpc_ncpu : FASCON2_MAX_CPUS;
}
#else
uint32_t fascon_plat_cpu_count(void) { return FASCON2_MAX_CPUS; }
#endif
uint32_t fascon_plat_cpu_id(void) { return host_core; }

void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    uint32_t idle = 0u;
    if (core == 0u || core >= FASCON2_MAX_CPUS) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: no such core (cores 1 to cpu_count() - 1 can be started)");
    if (!__atomic_compare_exchange_n(&host_cores[core].busy, &idle, 1u, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: the core is still running an earlier function");
    host_cores[core].arg = arg;
    __atomic_store_n(&host_cores[core].fn, fn, __ATOMIC_RELEASE);
    if (!host_cores[core].started) {
        host_cores[core].started = 1u;
#if defined(_WIN32)
        if (CreateThread(NULL, 0, host_thread, (LPVOID)(uintptr_t)core, 0, NULL) == NULL) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: cannot create a thread");
#else
        pthread_t t;
        if (pthread_create(&t, NULL, host_thread, (void *)(uintptr_t)core) != 0) fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: cannot create a thread");
        pthread_detach(t);
#endif
    }
}
#else
uint32_t fascon_plat_cpu_count(void) { return 1u; }
uint32_t fascon_plat_cpu_id(void) { return 0; }
void fascon_plat_spawn_on(uint32_t core, void (*fn)(uint64_t), uint64_t arg) {
    (void)core;
    (void)fn;
    (void)arg;
    fascon_plat_kernel_panic(FASCON_PANIC_SMP, "spawn_on: this program was built for one core");
}
#endif

/* ───── --target hpc: 計算ノードの形・ピン留め・NUMA の固定 / --target hpc: node shape, pinning and NUMA binding ─────
 * Linux: 使ってよい論理 CPU（sched_getaffinity: taskset・cgroup を尊重）を、/sys の topology（package・core_id）で物理
 * コアごとにまとめる。コア k は、k が物理コアの数より小さければ k 番目の物理コアの最初のスレッドへ（SMT の兄弟を使わない）、
 * それ以上なら兄弟スレッドへ順に回す。NUMA ノードは /sys/devices/system/cpu/cpuN/nodeM、固定は mbind（システムコールを
 * 直接。libnuma は使わない）。どれも助言なので、失敗しても止まらない（コンテナで拒まれる・NUMA の無いカーネル）。
 * 環境変数 FASCON_HPC_NO_PIN で、ピン留めと固定を止められる。macOS・Windows: Windows はピン留めだけ、ほかは何もしない
 * Linux: the usable logical CPUs (sched_getaffinity: honours taskset and cgroups) are grouped into physical cores by the
 * /sys topology (package, core_id). Core k goes to the first thread of the k-th physical core while k is below the
 * physical core count (no SMT sibling is used), then round the sibling threads. The NUMA node comes from
 * /sys/devices/system/cpu/cpuN/nodeM and binding is mbind (the raw system call; no libnuma). All of it is advisory and
 * never stops the program (containers may refuse it, kernels may lack NUMA). FASCON_HPC_NO_PIN turns pinning and binding
 * off. macOS / Windows: Windows pins only, the others do nothing */
#if defined(FASCON2_HPC)
#if defined(__linux__)
#include <sched.h>
#include <dirent.h>
#include <sys/syscall.h>
#endif
#if defined(_WIN32)
#include <windows.h>
#endif
#define HOST_HPC_MAX 1024
static int host_hpc_ready;
#if !defined(FASCON2_SMP)
static int host_hpc_ncpu; /* 使ってよい論理 CPU / usable logical CPUs */
#endif
static int host_hpc_cpu[HOST_HPC_MAX];      /* コア k に使う論理 CPU の並び / logical CPUs in the order cores take them */
static int host_hpc_node[HOST_HPC_MAX];     /* 並びと同じ順の NUMA ノード / NUMA nodes in the same order */
static uint32_t host_hpc_phys, host_hpc_smt, host_hpc_nodes;

#if defined(__linux__)
static long host_hpc_read_long(const char *path) {
    FILE *f = fopen(path, "r");
    long v = -1;
    if (f != NULL) {
        if (fscanf(f, "%ld", &v) != 1) v = -1;
        fclose(f);
    }
    return v;
}
static int host_hpc_node_of(int cpu) {
    char path[96];
    snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%d", cpu);
    DIR *d = opendir(path);
    int node = 0;
    if (d != NULL) {
        struct dirent *e;
        while ((e = readdir(d)) != NULL) {
            if (strncmp(e->d_name, "node", 4) == 0 && e->d_name[4] >= '0' && e->d_name[4] <= '9') {
                node = atoi(e->d_name + 4);
                break;
            }
        }
        closedir(d);
    }
    return node;
}
#endif

static void host_hpc_scan(void) {
    if (host_hpc_ready) return;
    host_hpc_ready = 1;
    host_hpc_ncpu = 0;
#if defined(__linux__)
    cpu_set_t set;
    CPU_ZERO(&set);
    int cpus[HOST_HPC_MAX];
    long key[HOST_HPC_MAX];
    int n = 0;
    if (sched_getaffinity(0, sizeof set, &set) == 0) {
        for (int c = 0; c < CPU_SETSIZE && n < HOST_HPC_MAX; c++) {
            if (!CPU_ISSET(c, &set)) continue;
            char path[128];
            snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%d/topology/physical_package_id", c);
            const long pkg = host_hpc_read_long(path);
            snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%d/topology/core_id", c);
            const long core = host_hpc_read_long(path);
            cpus[n] = c;
            key[n] = (pkg < 0 || core < 0) ? -1 - (long)c : (pkg << 20) | core;
            n++;
        }
    }
    /* 物理コアの順（最初に現れた順）と、その中の兄弟の順 / physical cores in order of first appearance, siblings within */
    long phys[HOST_HPC_MAX];
    int nphys = 0;
    for (int i = 0; i < n; i++) {
        int seen = 0;
        for (int j = 0; j < nphys; j++) seen |= phys[j] == key[i];
        if (!seen) phys[nphys++] = key[i];
    }
    uint32_t smt = 1u;
    for (int round = 0; host_hpc_ncpu < n; round++) {
        for (int p = 0; p < nphys; p++) {
            int k = 0;
            for (int i = 0; i < n; i++) {
                if (key[i] != phys[p]) continue;
                if (k++ == round) {
                    host_hpc_cpu[host_hpc_ncpu] = cpus[i];
                    host_hpc_node[host_hpc_ncpu] = host_hpc_node_of(cpus[i]);
                    host_hpc_ncpu++;
                }
            }
            if ((uint32_t)k > smt) smt = (uint32_t)k;
        }
        if (round > HOST_HPC_MAX) break;
    }
    host_hpc_phys = (uint32_t)(nphys > 0 ? nphys : 1);
    host_hpc_smt = smt;
    uint32_t nodes = 1u;
    for (int i = 0; i < host_hpc_ncpu; i++) {
        if ((uint32_t)host_hpc_node[i] + 1u > nodes) nodes = (uint32_t)host_hpc_node[i] + 1u;
    }
    host_hpc_nodes = nodes;
#elif defined(_WIN32)
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    host_hpc_ncpu = (int)si.dwNumberOfProcessors < HOST_HPC_MAX ? (int)si.dwNumberOfProcessors : HOST_HPC_MAX;
    for (int i = 0; i < host_hpc_ncpu; i++) {
        host_hpc_cpu[i] = i;
        host_hpc_node[i] = 0;
    }
    host_hpc_phys = (uint32_t)host_hpc_ncpu;
    host_hpc_smt = 1u;
    host_hpc_nodes = 1u;
#endif
    if (host_hpc_ncpu == 0) {
        host_hpc_ncpu = 1;
        host_hpc_cpu[0] = 0;
        host_hpc_node[0] = 0;
        host_hpc_phys = host_hpc_smt = host_hpc_nodes = 1u;
    }
}

void fascon_plat_topology(FasconTopology *t) {
    host_hpc_scan();
    t->cpus = (uint32_t)host_hpc_ncpu;
    t->cores = host_hpc_phys;
    t->smt = host_hpc_smt;
    t->numa_nodes = host_hpc_nodes;
}

void fascon_plat_pin_core(uint32_t core) {
    host_hpc_scan();
    if (getenv("FASCON_HPC_NO_PIN") != NULL) return;
    const int cpu = host_hpc_cpu[core % (uint32_t)host_hpc_ncpu];
#if defined(__linux__)
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    (void)sched_setaffinity(0, sizeof set, &set);
#elif defined(_WIN32)
    if (cpu < 64) (void)SetThreadAffinityMask(GetCurrentThread(), (DWORD_PTR)1 << cpu);
#else
    (void)cpu;
#endif
}

uint32_t fascon_plat_numa_node(uint32_t core) {
    host_hpc_scan();
    return (uint32_t)host_hpc_node[core % (uint32_t)host_hpc_ncpu];
}

int fascon_plat_numa_bind(void *base, size_t bytes, uint32_t node) {
#if defined(__linux__) && defined(SYS_mbind)
    if (getenv("FASCON_HPC_NO_PIN") != NULL || bytes == 0u || node >= 1024u) return 0;
    const uintptr_t b = (uintptr_t)base & ~(uintptr_t)4095u;
    const uintptr_t e = ((uintptr_t)base + bytes + 4095u) & ~(uintptr_t)4095u;
    unsigned long mask[1024 / (8 * sizeof(unsigned long))];
    memset(mask, 0, sizeof mask);
    mask[node / (8u * sizeof(unsigned long))] = 1ul << (node % (8u * sizeof(unsigned long)));
    /* MPOL_BIND = 2、MPOL_MF_MOVE = 2（このプロセスだけが使うページを移す） / MPOL_BIND = 2, MPOL_MF_MOVE = 2 (move pages
     * used by this process only). maxnode はビット数 + 1（カーネルは 1 引いて使う） / maxnode is bits + 1 (the kernel uses
     * one less) */
    return syscall(SYS_mbind, (void *)b, (unsigned long)(e - b), 2, mask, (unsigned long)(sizeof mask * 8u + 1u), 2u) == 0 ? 0 : -1;
#else
    (void)base;
    (void)bytes;
    (void)node;
    return 0;
#endif
}
#endif

/* ───── GPU の真似（gpu_arena） / GPU emulation (gpu_arena) ─────
 * fascon_plat_gpu_dispatch は仕事を FIFO に積み、1 本のワーカースレッドが積んだ順に、FASCON_GPU_KERNEL(番号) で登録された
 * C の関数をスロットの要素に対して走らせる。引換券（fence）は 1 から増える番号で、FIFO なので「番号 ≤ 終えた数」が完了。
 * 終えた数は release で書き、poll は acquire で読む（カーネルが書いた要素は、回収した CPU から見える）。終了時（exit・
 * panic）は積んだ仕事を走らせ終えてから終わる（装置が書いている最中の表をプロセスが捨てない）
 * fascon_plat_gpu_dispatch queues work in a FIFO; one worker thread runs, in queue order, the C functions registered with
 * FASCON_GPU_KERNEL(id) on the slots' elements. A fence is a number counting from 1 and, the queue being FIFO, it is
 * complete when it is <= the number of finished jobs. That count is stored with release and poll loads it with acquire
 * (what a kernel wrote is visible to the CPU that reclaims the slot). At exit (exit or panic) the queued work is finished
 * first (the process never drops a table the device is still writing) */
#if defined(FASCON2_GPU) && !defined(FASCON2_GPU_OFF)
#if defined(_WIN32)
#include <windows.h>
static SRWLOCK host_gpu_lock = SRWLOCK_INIT;
static CONDITION_VARIABLE host_gpu_work = CONDITION_VARIABLE_INIT, host_gpu_progress = CONDITION_VARIABLE_INIT;
#define HOST_GPU_LOCK() AcquireSRWLockExclusive(&host_gpu_lock)
#define HOST_GPU_UNLOCK() ReleaseSRWLockExclusive(&host_gpu_lock)
#define HOST_GPU_WAIT(cv) SleepConditionVariableSRW(&(cv), &host_gpu_lock, INFINITE, 0)
#define HOST_GPU_BROADCAST(cv) WakeAllConditionVariable(&(cv))
#else
#include <pthread.h>
static pthread_mutex_t host_gpu_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t host_gpu_work = PTHREAD_COND_INITIALIZER, host_gpu_progress = PTHREAD_COND_INITIALIZER;
#define HOST_GPU_LOCK() pthread_mutex_lock(&host_gpu_lock)
#define HOST_GPU_UNLOCK() pthread_mutex_unlock(&host_gpu_lock)
#define HOST_GPU_WAIT(cv) pthread_cond_wait(&(cv), &host_gpu_lock)
#define HOST_GPU_BROADCAST(cv) pthread_cond_broadcast(&(cv))
#endif
#define HOST_GPU_QUEUE 1024u
typedef struct {
    uint32_t arena, slot, kernel;
    uint64_t grid, block;
} host_gpu_job;
static host_gpu_job host_gpu_q[HOST_GPU_QUEUE];
static uint64_t host_gpu_issued; /* 積んだ数 = 最後の引換券 / jobs queued = the last fence */
static uint64_t host_gpu_done;   /* 終えた数 / jobs finished */
static int host_gpu_started;

static void host_gpu_run(void) {
    for (;;) {
        HOST_GPU_LOCK();
        while (host_gpu_issued == __atomic_load_n(&host_gpu_done, __ATOMIC_RELAXED)) HOST_GPU_WAIT(host_gpu_work);
        const host_gpu_job j = host_gpu_q[host_gpu_done % HOST_GPU_QUEUE];
        HOST_GPU_UNLOCK();
        const fascon_gpu_kernel_fn fn = fascon_gpu_kernel_lookup(j.kernel);
        const FasconGpuArena *a = &fascon2_gpu_arenas[j.arena];
        if (fn != NULL) fn((uint8_t *)a->base + (size_t)j.slot * a->stride, j.slot, j.grid, j.block);
        HOST_GPU_LOCK();
        __atomic_store_n(&host_gpu_done, __atomic_load_n(&host_gpu_done, __ATOMIC_RELAXED) + 1u, __ATOMIC_RELEASE);
        HOST_GPU_BROADCAST(host_gpu_progress);
        HOST_GPU_UNLOCK();
    }
}
#if defined(_WIN32)
static DWORD WINAPI host_gpu_thread(LPVOID p) {
    (void)p;
    host_gpu_run();
    return 0;
}
#else
static void *host_gpu_thread(void *p) {
    (void)p;
    host_gpu_run();
    return NULL;
}
#endif

/* 積んだ仕事を終えるまで待つ（終了の前） / wait for the queued work to finish (before exiting) */
static void host_gpu_drain(void) {
    HOST_GPU_LOCK();
    while (__atomic_load_n(&host_gpu_done, __ATOMIC_RELAXED) < host_gpu_issued) HOST_GPU_WAIT(host_gpu_progress);
    HOST_GPU_UNLOCK();
}

/* 呼ぶときは錠を持っている / called with the lock held */
static int host_gpu_start(void) {
    if (host_gpu_started) return 0;
#if defined(_WIN32)
    if (CreateThread(NULL, 0, host_gpu_thread, NULL, 0, NULL) == NULL) return -1;
#else
    pthread_t t;
    if (pthread_create(&t, NULL, host_gpu_thread, NULL) != 0) return -1;
    pthread_detach(t);
#endif
    host_gpu_started = 1;
    atexit(host_gpu_drain);
    return 0;
}

int fascon_plat_gpu_register(void *base, size_t bytes, uint32_t arena_id) {
    if (arena_id >= fascon2_gpu_arena_count || base != fascon2_gpu_arenas[arena_id].base || bytes != fascon2_gpu_arenas[arena_id].bytes) return -1;
    HOST_GPU_LOCK();
    const int r = host_gpu_start();
    HOST_GPU_UNLOCK();
    return r;
}

int fascon_plat_gpu_dispatch(uint32_t arena_id, uint32_t slot_idx, uint32_t kernel_id, uint64_t grid, uint64_t block, uint64_t *out_fence) {
    if (arena_id >= fascon2_gpu_arena_count || (size_t)slot_idx >= fascon2_gpu_arenas[arena_id].bytes / fascon2_gpu_arenas[arena_id].stride) return -1;
    if (fascon_gpu_kernel_lookup(kernel_id) == NULL) {
        fprintf(stderr, "[host] gpu: no kernel registered with id %u (FASCON_GPU_KERNEL(%u) in <name>.gpu.c)\n", (unsigned)kernel_id, (unsigned)kernel_id);
        return -1;
    }
    HOST_GPU_LOCK();
    if (host_gpu_start() != 0) {
        HOST_GPU_UNLOCK();
        return -1;
    }
    /* 列が満杯なら空くまで待つ / wait for room when the queue is full */
    while (host_gpu_issued - __atomic_load_n(&host_gpu_done, __ATOMIC_RELAXED) >= HOST_GPU_QUEUE) HOST_GPU_WAIT(host_gpu_progress);
    host_gpu_job *j = &host_gpu_q[host_gpu_issued % HOST_GPU_QUEUE];
    j->arena = arena_id;
    j->slot = slot_idx;
    j->kernel = kernel_id;
    j->grid = grid;
    j->block = block;
    host_gpu_issued++;
    *out_fence = host_gpu_issued;
    HOST_GPU_BROADCAST(host_gpu_work);
    HOST_GPU_UNLOCK();
    return 0;
}

int fascon_plat_gpu_poll(uint64_t fence) { return __atomic_load_n(&host_gpu_done, __ATOMIC_ACQUIRE) >= fence; }

void fascon_plat_gpu_wait(uint64_t fence) {
    if (fascon_plat_gpu_poll(fence)) return;
    HOST_GPU_LOCK();
    while (__atomic_load_n(&host_gpu_done, __ATOMIC_ACQUIRE) < fence) HOST_GPU_WAIT(host_gpu_progress);
    HOST_GPU_UNLOCK();
}
#endif

/* ───── I/O ポート（port_io）の真似 / emulated I/O ports (port_io) ─────
 *   - COM1（0x3f8〜0x3ff）は 16550 を真似る: 送信（THR）は標準出力へ、LSR は「送信可」（0x60）。DLAB = 1 の間の 0x3f8 は除数
 *   - 受信（RBR）は既定で MMIO の真似と同じ「受信あり・値 0」。環境変数 FASCON_HOST_STDIN=1 なら標準入力から読む
 *     （LSR の bit0 = まだ読める）
 *   - isa-debug-exit（0xf4 / 0x501）への書き込みは QEMU と同じ終了コード (値 << 1) | 1 で終わる
 *   - それ以外のポートは 64 KiB のただのメモリ（書いた値がそのまま読める）
 *   - COM1 (0x3f8-0x3ff) emulates a 16550: transmit (THR) goes to stdout, LSR says "transmit ready" (0x60); 0x3f8 is the
 *     divisor while DLAB = 1
 *   - receive (RBR) says "data ready, value 0" by default, like the MMIO stand-in; with FASCON_HOST_STDIN=1 it reads stdin
 *     (LSR bit 0 = more to read)
 *   - a write to isa-debug-exit (0xf4 / 0x501) exits with the same code as QEMU: (value << 1) | 1
 *   - every other port is 64 KiB of plain memory (a value written can be read back) */
static uint8_t host_ports[65536];
static int host_stdin = -1; /* -1 = まだ調べていない / not checked yet, 0 = 無効 / off, 1 = 有効 / on */
static int host_peek = -2;  /* 先読みした 1 文字（-2 = なし、EOF = 終わり） / one character read ahead (-2 = none) */

static int host_stdin_on(void) {
    if (host_stdin < 0) {
        const char *e = getenv("FASCON_HOST_STDIN");
        host_stdin = (e != NULL && e[0] != 0 && e[0] != '0') ? 1 : 0;
    }
    return host_stdin;
}

/* 受信あり? 標準入力を使わないときは MMIO の真似と同じく「受信あり・値 0」（受信待ちのループはすぐ進む）
 * data ready? Without stdin this matches the MMIO stand-in: "data ready, value 0" (receive-poll loops move on at once) */
static int host_rx_ready(void) {
    if (!host_stdin_on()) return 1;
    if (host_peek == -2) host_peek = getchar();
    return host_peek != EOF;
}

uint32_t fascon2_host_pio_in(uint32_t port, uint32_t bits) {
    port &= 0xFFFFu;
    const int dlab = (host_ports[0x3FBu] & 0x80u) != 0;
    switch (port) {
    case 0x3F8u:
        if (!dlab) {
            if (!host_stdin_on() || !host_rx_ready()) return 0u;
            const int c = host_peek;
            host_peek = -2;
            return (uint32_t)(unsigned char)c;
        }
        break;
    case 0x3FAu:
        return 0x01u; /* IIR: 割り込みなし / no interrupt pending */
    case 0x3FDu:
        fflush(stdout);
        return 0x60u | (uint32_t)host_rx_ready(); /* LSR */
    case 0x3FEu:
        return 0xB0u; /* MSR: DCD | DSR | CTS */
    case 0x2FDu:
    case 0x3EDu:
    case 0x2EDu:
        return 0x60u; /* COM2〜4 の LSR: 送信可 / LSR of COM2-4: transmit ready */
    default:
        break;
    }
    uint32_t v = 0u;
    for (uint32_t b = 0; b < bits / 8u; b++) v |= (uint32_t)host_ports[(port + b) & 0xFFFFu] << (8u * b);
    return v;
}

void fascon2_host_pio_out(uint32_t port, uint32_t bits, uint32_t value) {
    port &= 0xFFFFu;
    if (port == 0x3F8u && (host_ports[0x3FBu] & 0x80u) == 0u) {
        const char c = (char)(value & 0xFFu);
        fascon_plat_print(&c, 1u); /* COM1 の送信 → 標準出力 / COM1 transmit → stdout */
        return;
    }
    if (port == 0xF4u || port == 0x501u) fascon_plat_exit((int)(((value & 0xFFu) << 1) | 1u)); /* isa-debug-exit */
    for (uint32_t b = 0; b < bits / 8u; b++) host_ports[(port + b) & 0xFFFFu] = (uint8_t)(value >> (8u * b));
}

static void map_regions(void) {
    for (uint32_t i = 0; i < fascon2_mmio_region_count; i++) {
        const FasconMmioRegion *r = &fascon2_mmio_regions[i];
        /* 4 KiB 境界にそろえた 0 埋めのメモリ / zero-filled memory aligned to 4 KiB */
        unsigned char *p = (unsigned char *)calloc(1, (size_t)r->size + 4096u);
        if (p == NULL) {
            fprintf(stderr, "[host] cannot allocate MMIO region %s (%lu bytes)\n", r->name, (unsigned long)r->size);
            exit(72);
        }
        fascon2_host_mmio_base[i] = ((uintptr_t)p + 4095u) & ~(uintptr_t)4095u;
    }
}

/* 大きなアリーナに 2 MiB ページを頼む（助言なので失敗しても続ける）。環境変数 FASCON_NO_HUGEPAGE で止められる
 * ask for 2 MiB pages for large arenas (advisory); the FASCON_NO_HUGEPAGE environment variable turns it off.
 * 効いたかどうかは /proc/<pid>/smaps の AnonHugePages で確かめられる / check AnonHugePages in /proc/<pid>/smaps to see it took effect */
static void hint_huge_pages(void) {
#if defined(__linux__) && defined(MADV_HUGEPAGE)
    if (getenv("FASCON_NO_HUGEPAGE") != NULL) return;
    for (uint32_t i = 0; i < fascon2_arena_region_count; i++) {
        const FasconArenaRegion *r = &fascon2_arena_regions[i];
#if !defined(FASCON2_HPC)
        /* --target hpc はすべての表（2 MiB 境界に置かれる）に頼む / --target hpc asks for every table (all 2 MiB aligned) */
        if (r->size < F20_HUGE_MIN) continue;
#endif
        const uintptr_t b = (uintptr_t)r->base & ~(uintptr_t)4095u;
        const uintptr_t e = ((uintptr_t)r->base + r->size + 4095u) & ~(uintptr_t)4095u;
        (void)madvise((void *)b, (size_t)(e - b), MADV_HUGEPAGE);
    }
#endif
}

/* ───── カーネル拡張の PC 版: 割り込み・タイマー・タスク（決定的な模擬）
 *       the PC version of the kernel extensions: interrupts, the timer and tasks (a deterministic simulation)
 *
 * - 割り込みの禁止は 1 つのフラグ。タイマーは仮想の時計で、cpu_relax()・wait_irq()・timer_now() が進める
 *   interrupt masking is one flag; the timer is a virtual clock advanced by cpu_relax(), wait_irq() and timer_now()
 * - 期限が来ていて割り込みが許可されていれば、その場でタイマーのハンドラ（INTID 27 か 30）を呼ぶ。ハンドラが
 *   resume(id) を予約していれば、戻る前にそのタスクへ切り替える
 *   when the deadline has passed and interrupts are enabled, the timer handler (INTID 27 or 30) is called on the spot;
 *   if it requested resume(id), the switch happens before returning
 * - タスクの文脈は、POSIX では ucontext、Windows ではファイバー
 *   task contexts are ucontext on POSIX and fibers on Windows
 * 実機と違い、命令の途中では割り込まれない（割り込まれうる点は上の 3 つと、閉じた区間の出口だけ）。
 * Unlike real hardware nothing is interrupted between instructions (the points above and the end of a closed section
 * are the only ones). */
#if defined(_WIN32)
#include <windows.h>
typedef LPVOID host_ctx_t;
#else
#include <ucontext.h>
typedef ucontext_t host_ctx_t;
#endif

typedef struct {
    host_ctx_t ctx;
    f20_id_t id;
    int alive;
    uint64_t masked;
    void (*entry)(uint64_t);
    uint64_t arg;
    void (*on_exit)(void);
    uint8_t *stack;
    uint8_t *f_top, *f_end, *f_base;
} HostTask;

/* マルチコア: タスクの表・いまのタスク・割り込みの禁止・仮想の時計はコア（= スレッド）ごと。task_arena がコアごとの実体なので、
 * スロットの番号もコアごとに別のタスクを指す
 * multi-core: the task table, the current task, interrupt masking and the virtual clock are per core (= per thread);
 * task_arena has one instance per core, so slot numbers name different tasks on different cores */
#if defined(FASCON2_SMP)
#define HOST_PER_CORE __thread
#else
#define HOST_PER_CORE
#endif
static HOST_PER_CORE HostTask host_tasks[FASCON2_TASK_MAX];
static HOST_PER_CORE int host_cur = -1;      /* -1: 起動時（main） / -1: boot (main) */
static HOST_PER_CORE int host_pending = -1;  /* resume で予約されたタスク / the task requested by resume */
static HOST_PER_CORE uint64_t host_masked = 1;
static HOST_PER_CORE uint64_t host_now = 0, host_deadline = 0;
static HOST_PER_CORE int host_armed = 0, host_in_irq = 0;
static const FasconIrqEntry *host_irqs = NULL;
static uint32_t host_irq_count = 0;
#if defined(_WIN32)
static HOST_PER_CORE LPVOID host_boot_fiber = NULL;
#endif

/* このコアのタスクが尽きた・起こすものが無い: 1 コアのプログラムと CPU0 は終了（今までどおり）。マルチコアのほかのコアは
 * 止まったまま待つ（ほかのコアがまだ動いているかもしれない。終わりはプログラムか CPU0 が決める）
 * this core ran out of tasks or has nothing to wake it: single-core programs and CPU0 exit (as before); other cores of a
 * multi-core program stay halted (other cores may still run; the program or CPU0 decides when it ends) */
static FASCON_NORETURN void host_core_done(void) {
#if defined(FASCON2_SMP)
    if (fascon_plat_cpu_id() != 0u) {
        host_masked = 1;
        for (;;) {
#if defined(_WIN32)
            Sleep(10);
#else
            usleep(10000);
#endif
        }
    }
#endif
    fascon_plat_exit(0);
}

static void host_check_canary(int t) {
    if (t >= 0 && host_tasks[t].stack != NULL) {
        uint64_t c;
        memcpy(&c, host_tasks[t].stack, sizeof c);
        if (c != FASCON_STACK_CANARY) fascon_plat_kernel_panic(FASCON_PANIC_STACK, "task stack overflow (the canary at the bottom of the stack was overwritten)");
    }
}

/* いまのタスクを保存して to へ（save = 0 なら保存しない: enter / exit） / save the current task and switch to `to` */
static void host_switch(int to, int save) {
    const int from = host_cur;
    if (to == from) return;
    host_check_canary(from);
    if (from >= 0 && save) {
        host_tasks[from].masked = host_masked;
        host_tasks[from].f_top = fascon__frame.top;
        host_tasks[from].f_end = fascon__frame.end;
        host_tasks[from].f_base = fascon__frame.base;
    }
    host_cur = to;
    host_masked = host_tasks[to].masked;
    fascon__frame.top = host_tasks[to].f_top;
    fascon__frame.end = host_tasks[to].f_end;
    fascon__frame.base = host_tasks[to].f_base;
#if defined(_WIN32)
    SwitchToFiber(host_tasks[to].ctx);
#else
    if (from >= 0 && save) {
        swapcontext(&host_tasks[from].ctx, &host_tasks[to].ctx);
    } else {
        setcontext(&host_tasks[to].ctx);
    }
#endif
}

#if defined(_WIN32)
static VOID CALLBACK host_trampoline(LPVOID unused) {
    (void)unused;
#else
static void host_trampoline(void) {
#endif
    HostTask *t = &host_tasks[host_cur];
    t->entry(t->arg);
    t->on_exit(); /* 入口から戻った = 終了 / returning from the entry = exit */
    host_core_done();
}

/* 期限の来たタイマー割り込みを届ける / deliver a due timer interrupt */
static void host_deliver(void) {
    while (!host_masked && !host_in_irq && host_armed && host_now >= host_deadline) {
        host_armed = 0;
        host_in_irq = 1;
        host_masked = 1;
        for (uint32_t i = 0; i < host_irq_count; i++) {
            if (host_irqs[i].intid == 27u || host_irqs[i].intid == 30u) host_irqs[i].handler();
        }
        host_in_irq = 0;
        host_masked = 0;
        if (host_pending >= 0) {
            const int to = host_pending;
            host_pending = -1;
            if (host_cur >= 0 && host_tasks[to].alive) host_switch(to, 1);
        }
    }
}

void fascon2_host_poll(void) {
    host_now += 1000u;
    host_deliver();
}

void fascon_plat_irq_register(const FasconIrqEntry *table, uint32_t count) {
    host_irqs = table;
    host_irq_count = count;
}

uint64_t fascon_plat_irq_save(void) {
    const uint64_t prev = host_masked;
    host_masked = 1;
    return prev;
}

void fascon_plat_irq_restore(uint64_t state) {
    host_masked = state;
    host_deliver();
}

void fascon_plat_irq_open(void) {
    host_masked = 0;
    host_deliver();
}

void fascon_plat_wait_irq(void) {
    if (!host_armed) {
        /* 起こすものが何も無い: PC ではここで終わる（実機では止まったまま）
           nothing can wake us: a PC run ends here (real hardware would stay halted) */
        host_core_done();
    }
    if (host_now < host_deadline) host_now = host_deadline;
    const uint64_t prev = host_masked;
    host_masked = 0;
    host_deliver();
    host_masked = prev;
}

uint64_t fascon_plat_timer_freq(void) { return 62500000u; }
uint64_t fascon_plat_timer_now(void) { return ++host_now; }
void fascon_plat_timer_arm(uint64_t ticks) {
    host_deadline = host_now + ticks;
    host_armed = 1;
}

/* ユーザー空間（EL0）は PC では再現しない: 例外レベルとページ表が要るので、`fascon qemu` で動かす
 * user space (EL0) is not simulated on the PC: it needs exception levels and page tables, so run it with `fascon qemu` */
void fascon_plat_user_register(uint8_t *mem, uint64_t size, uint64_t va, uint32_t slots) { (void)mem; (void)size; (void)va; (void)slots; }
void fascon_plat_syscall_register(const FasconSyscallFn *table, uint32_t count) { (void)table; (void)count; }
void fascon_plat_trap_register(void (*handler)(uint64_t, uint64_t, uint64_t)) { (void)handler; }
bool fascon_plat_user_window(uint64_t addr, uint64_t len, f20_slice_t *out) { (void)addr; (void)len; (void)out; return false; }
bool fascon_plat_task_create_user(uint32_t slot, f20_id_t id, uint64_t entry, uint64_t sp, void *stack, size_t stack_size, void (*on_exit)(void)) {
    (void)slot; (void)id; (void)entry; (void)sp; (void)stack; (void)stack_size; (void)on_exit;
    fascon_plat_kernel_panic(FASCON_PANIC_TASK, "EL0 tasks (start_user) run only on the QEMU target: use `fascon qemu`");
}

bool fascon_plat_task_create(uint32_t slot, f20_id_t id, void (*entry)(uint64_t), uint64_t arg, void *stack, size_t stack_size, void (*on_exit)(void)) {
    if (slot >= FASCON2_TASK_MAX || stack_size < 4096u) return false;
    HostTask *t = &host_tasks[slot];
    const size_t fr = (stack_size / 4u) & ~(size_t)15u;
#if defined(_WIN32)
    if (host_boot_fiber == NULL) host_boot_fiber = ConvertThreadToFiber(NULL);
    if (t->ctx != NULL) DeleteFiber(t->ctx);
    t->ctx = CreateFiber(stack_size < 65536u ? 65536u : stack_size, host_trampoline, NULL);
    if (t->ctx == NULL) return false;
#else
    if (getcontext(&t->ctx) != 0) return false;
    t->ctx.uc_stack.ss_sp = (uint8_t *)stack + fr;
    t->ctx.uc_stack.ss_size = stack_size - fr;
    t->ctx.uc_link = NULL;
    makecontext(&t->ctx, host_trampoline, 0);
#endif
    t->id = id;
    t->alive = 1;
    t->masked = 0;
    t->entry = entry;
    t->arg = arg;
    t->on_exit = on_exit;
    t->stack = (uint8_t *)stack;
    /* スタックの下 1/4 はフレームリージョン。いちばん下の 8 バイトは番兵語
       the lowest quarter of the stack is the frame region; its lowest 8 bytes are the canary */
    const uint64_t canary = FASCON_STACK_CANARY;
    memcpy(stack, &canary, sizeof canary);
    t->f_base = (uint8_t *)stack + 16;
    t->f_top = t->f_base;
    t->f_end = (uint8_t *)stack + fr;
    return true;
}

f20_id_t fascon_plat_task_current(void) { return host_cur >= 0 ? host_tasks[host_cur].id : 0u; }

void fascon_plat_task_yield(uint32_t slot, f20_id_t id) {
    if (host_in_irq) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "yield_to inside an interrupt handler");
    if (host_cur < 0 || slot >= FASCON2_TASK_MAX || !host_tasks[slot].alive || host_tasks[slot].id != id) return;
    host_switch((int)slot, 1);
    host_deliver();
}

void fascon_plat_task_resume(uint32_t slot, f20_id_t id) {
    if (slot < FASCON2_TASK_MAX && host_tasks[slot].alive && host_tasks[slot].id == id) host_pending = (int)slot;
}

FASCON_NORETURN void fascon_plat_task_enter(uint32_t slot, f20_id_t id) {
    if (slot >= FASCON2_TASK_MAX || !host_tasks[slot].alive || host_tasks[slot].id != id) fascon_plat_kernel_panic(FASCON_PANIC_TASK, "enter: the task does not exist");
    host_switch((int)slot, 0);
    host_core_done();
}

FASCON_NORETURN void fascon_plat_task_exit(void) {
    if (host_cur >= 0) {
        host_check_canary(host_cur);
        host_tasks[host_cur].alive = 0;
        for (uint32_t k = 1; k <= FASCON2_TASK_MAX; k++) {
            const uint32_t i = ((uint32_t)host_cur + k) % FASCON2_TASK_MAX;
            if (host_tasks[i].alive) host_switch((int)i, 0);
        }
    }
    host_core_done(); /* 生きているタスクが無い / no live task is left */
}


#if defined(FASCON2_UNIVERSAL)
/* ───── ユニバーサル OS 拡張: PC 実行の装置の窓（bind_mmio） / universal-OS extension: device windows on PC runs ─────
 *   バス列挙（DTB / ACPI / PCIe）の代わりに、QEMU virt と同じ並びの窓を起動時に登録する（--board の board.toml があれば
 *   生成 C の fascon_board_init がそれを足す）。窓の中身はホストのメモリの影（0 で初期化、窓ごとに最大 16 MiB）
 *   instead of bus enumeration (DTB / ACPI / PCIe), windows laid out like QEMU virt are registered at start-up (a board.toml
 *   given with --board adds its own through fascon_board_init in the generated C). Window contents are shadows in host
 *   memory (zero-filled, at most 16 MiB per window) */
#define HOST_WIN_MAX 16u
#define HOST_SHADOW_MAX ((uint64_t)16u << 20)
static struct {
    uint64_t phys, size;
    uint8_t *shadow;
} host_win[HOST_WIN_MAX];
static uint32_t host_nwin;
static void host_win_add(uint64_t phys, uint64_t size) {
    if (host_nwin < HOST_WIN_MAX) {
        host_win[host_nwin].phys = phys;
        host_win[host_nwin].size = size < HOST_SHADOW_MAX ? size : HOST_SHADOW_MAX;
        host_win[host_nwin].shadow = NULL;
        host_nwin++;
        fascon_mmio_register_window(phys, size < HOST_SHADOW_MAX ? size : HOST_SHADOW_MAX);
    }
}
uintptr_t fascon_plat_mmio_va(uint64_t phys) {
    for (uint32_t i = 0; i < host_nwin; i++) {
        if (phys >= host_win[i].phys && phys - host_win[i].phys < host_win[i].size) {
            if (host_win[i].shadow == NULL) host_win[i].shadow = (uint8_t *)calloc(1, (size_t)host_win[i].size);
            if (host_win[i].shadow == NULL) fascon_plat_kernel_panic(FASCON_PANIC_OOM, "no memory for the shadow of a device window");
            return (uintptr_t)(host_win[i].shadow + (phys - host_win[i].phys));
        }
    }
    return 0u;
}
static void host_default_windows(void) {
    host_win_add(UINT64_C(0x08000000), UINT64_C(0x00020000)); /* GIC */
    host_win_add(UINT64_C(0x09000000), UINT64_C(0x00001000)); /* PL011 UART */
    host_win_add(UINT64_C(0x0a000000), UINT64_C(0x00004000)); /* virtio-mmio x 32 */
    host_win_add(UINT64_C(0x10000000), UINT64_C(0x2eff0000)); /* PCIe MMIO (the first 16 MiB are shadowed) */
}
#endif

int main(void) {
    hint_huge_pages();
    map_regions();
#if defined(FASCON2_UNIVERSAL)
    host_default_windows();
#endif
#if defined(FASCON2_SMP)
    host_core_setup(0);
#endif
    fascon2_main();
    fflush(stdout);
    save_screen();
    return 0;
}
