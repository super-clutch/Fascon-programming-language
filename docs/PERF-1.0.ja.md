# Fascon 1.0 — 分岐のない走査・キャッシュラインの配置・大きなページ・列（2026-11 の改修）

2026-11-06、列は 2026-11-07、完全列化（True SoA）は 2026-11-21、条件付き一括更新と浮動小数点の列は 2026-11-25 · Fascon 1.0.0（版は据え置き） · English: [PERF-1.0.md](PERF-1.0.md)

この文書は、表の走査（`for id in @a`）を速くするために 1.0.0 のコンパイラ・ランタイム・BSP に入れた変更をまとめたものです。課題として挙がった 3 つの原因（分岐・偽共有・TLB ミス）ごとに、「1.0.0 のコードが実際にしていたこと」「変えたこと」「生成される C とアセンブリ」「測った結果」の順に書きます。フェーズ 4 では、そのあとに残ったもの、つまり走査が読むバイト数を減らします。

## まとめ

| 課題で挙がった原因 | 1.0.0 のコードが実際にしていたこと | 今回の変更 |
| --- | --- | --- |
| 生存確認の動的な分岐（cbnz / jnz） | 0.9 ns の走査（総和の特殊化）は、もともと分岐なし（ループの戻りだけ）。分岐があったのは一般の `for id in @a`（スロットごとに `link[]` を見る分岐）と、ラッチを使う関数のループ先頭のコミット点（`cbnz`） | 生存ビットマップ + ctz（TZCNT / RBIT+CLZ）、条件付きの個数・総和・最小 / 最大をマスクで分岐なしに、回数の決まったループからコミット点を出口へ、while / loop ではラッチを終了条件に混ぜる |
| キャッシュラインの偽共有 | 確保のたびに書く管理情報 `st` が `gen[]` の先頭と同じラインに乗っていた | 管理情報を 128 バイトのブロックに単独で置き、各配列を 128 バイト境界から始める。`#[repr(align(N))]`。コアに割り当てる単位（512 スロット）を決めた |
| TLB ミス（4 KiB ページ） | BSP は RAM を最初から 2 MiB ブロックで恒等マップしていて、4 KiB のページはない。MMU の設定も起動時の 1 回だけで、呼び出しごとにページテーブルを書き換えるコードはない | 画面がなければ RAM を 1 GiB ブロック 1 枚に（引きが 1 段減る）、画面があれば 2 MiB ブロックに連続ビット（32 MiB）。アリーナを 2 MiB 境界の専用セクションへ。PC 実行では `madvise(MADV_HUGEPAGE)` |
| 「1 コール = 1 リージョン」の MMU 操作 | 上のとおり MMU 操作はない。own の確保はヒープの空きリスト（12 段） | エスケープ解析で移動しないと分かった `let x = own T { .. }` を、フレームリージョン（スタックと同じ動き）に置く |
| 1 タスクあたりに読むバイト数（フェーズ 1〜3 のあとに残ったもの） | 集計がスロットのどれだけを使うかに関係なく、どの走査も 32 バイトのスロットをまるごと読んでいた | 列（フェーズ 4）: 集計ごとに、その集計が読む値だけを並べた列（1 タスク 8 / 4 バイトか 1 ビット）を走査する。コンパイラが書き込みのたびに列を同期させる |
| 列が読むフィールドへの書き込みが約 2 倍（フェーズ 4 の二重書き込み） | 写しの列ではスロットと列の両方を書き、作業領域が L2 を超えていた | 完全列化（フェーズ 5）: 条件を満たすフィールドはスロットから外れ、列が唯一の正本（書き込みは 1 回）。大きな列の一括上書きは非時間的ストア。完全列化できず書き込みの多いフィールドには写しの列を作らない |
| 条件付きの書き込み（`if 条件 { @a[id].f += x }`）は、まだ生存ビットマップをスロットごとにたどっていた（フェーズ 5: C の 0.97〜1.90 倍） | 一般のループ: 生存スロットごとに ctz・条件の判定・検査付きの加算 | 条件付き一括更新（フェーズ 6）: 条件のビット列を 64 スロットの語ずつ読み、0 の語は飛ばし、全部 1 の語はふつうのベクトル加算、ばらばらの語はレーンごとにあふれを確かめる述語付きのベクトル加算。`#[fast_math]` の下では浮動小数点の列も |

測った結果（x86-64、[bench/RESULTS.md](../bench/RESULTS.md)、1 タスクあたりの ns）: 条件付きの個数と pick_next は 1.10〜1.31 ns → 0.69〜0.73 ns、まばらな表は 0.48〜0.51 ns → 0.07 ns、素の総和の走査は 0.71〜0.74 ns → 0.57〜0.63 ns です。素の総和は 2.10 GHz で 1.2〜1.3 クロックにあたり、課題の目標（0.48〜0.70 ns）の範囲に入りました。この走査には前から分岐がなく、速くなった分は 2 MiB ページによるものです。残った遅さの元は、1 タスク 32 バイトを読む帯域でした。

フェーズ 4 の列を入れると（別のホストでの後日の回、コアのクロック 3.85 GHz、[bench/RESULTS.md](../bench/RESULTS.md)）: 素の総和は 0.45〜0.54 → 0.10〜0.21 ns、条件付き総和は 0.48〜0.82 → 0.05〜0.10 ns、pick_next は 0.49〜0.63 → 0.07〜0.21 ns、条件付きの個数は 0.28〜0.60 → 0.00〜0.02 ns になりました。どのビルド（GCC と clang、SSE2 と AVX-512）でも 32 バイトのタスク 1 つあたり 1 クロック未満（0.19〜0.81 クロック）です。列が読むフィールドを書くループはその分を払います（実行可能なタスクすべての `vruntime` を書くループで約 2 倍）。ほかのフィールドを書くループは変わりません。

フェーズ 5 の完全列化のあと（このホスト、コアのクロック 2.87〜3.27 GHz、手書きの C（AoS）・改修前・改修後を交互に 9 回）: 6 万タスクでは読み出しがどのビルドでも 1 タスク 1 クロック未満（0.01〜0.75 クロック）のまま、全タスクの一括更新と一括上書きが C の 0.44〜0.86 倍（改修前は 3.1〜3.5 倍）、実行可能なタスクだけの書き込みが GCC で C の 0.97〜1.03 倍・clang で 1.47〜1.90 倍（改修前は 1.9〜3.6 倍）です。400 万タスクでは読み出しが 0.01〜1.03 クロック（C は 5.6〜10.5）、一括更新と上書きが C の 0.10〜0.24 倍、実行可能なタスクだけの書き込みが 1.11〜1.27 倍です（5-7）。

## フェーズ 1 — 走査から分岐をなくす

### 1-1 生存ビットマップと ctz（一般の `for id in @a`）

アリーナの表に、1 スロット 1 ビットの生存ビットマップ `live[]` を足しました（64 スロット = `uint64_t` 1 語）。`alloc` で立て、`free` / `remove` で落とします。

```c
/* runtime/fascon2_rt.h（確保と解放の中） */
live[i >> 6] |= UINT64_C(1) << (i & 63u);       /* acquire */
live[i >> 6] &= ~(UINT64_C(1) << (i & 63u));    /* release */

static inline uint32_t f20_ctz64(uint64_t m) { return (uint32_t)__builtin_ctzll(m); }   /* TZCNT / RBIT+CLZ */
static inline uint64_t f20_blsr64(uint64_t m) { return m & (m - 1u); }                 /* BLSR / SUB+AND */
```

一般の `for id in @a` は、1 語を読んで次の生存スロットを ctz で求めます。1.0.0 の「スロットごとに `link[_i] != F20_LIVE` を見て continue する」分岐はなくなりました。

```c
/* 1.0.0 */
for (uint32_t _i = 0; _i < arena_process.st.next_unused; _i++) {
    F20_COMMIT(_flt, site);                                   /* 周回ごとの cbnz */
    if (arena_process.link[_i] != F20_LIVE) continue;         /* スロットごとの分岐 */
    ...
}

/* いま（src/codegen.rs の loop_stmt、ForIter::Arena） */
for (uint32_t _w1 = 0u; _w1 < F20_LIVE_WORDS_USED(arena_process.st.next_unused); _w1++) {
    for (uint64_t _lm2 = arena_process.live[_w1]; _lm2 != 0u; _lm2 = f20_blsr64(_lm2)) {
        const uint32_t _i3 = (_w1 << 6) | f20_ctz64(_lm2);
        const fsc_Id_process id = F20_ID_MAKE(arena_process.gen[_i3], _i3, ARENA_process_K);
        { /* 本体 */ }
    }
}
F20_COMMIT(_flt, site);                                       /* 出口で 1 回 */
```

本体が同じアリーナで `alloc` / `free` / `remove` する（呼び出し先も含めて。`Effects` に `allocs` を足して推移的に求める）ときだけ、語を読み直して「今の添字より上の生存スロット」を残す形 `arena.live[w] & f20_above_lowest64(m)` にします。解放されたスロットは飛ばし、後ろに確保されたスロットは回るので、1.0.0 と同じ意味です。`break` は 2 重ループを抜けるラベルへ飛びます。

64 スロットの `@process` では、GCC は語のループそのものを消し、ビットマップを 1 語読むだけになります（x86-64、`-mbmi`）。

```asm
fsc_tick_all:                              // for id in @process { @process[id].ticks = @process[id].ticks + 1; }
    movl    arena_process(%rip), %eax      // st.next_unused
    addl    $63, %eax
    shrl    $6, %eax
    je      .L142
    movq    128+arena_process(%rip), %rcx  // live[0]: 64 スロットの生存状態を 1 回のロードで
    testq   %rcx, %rcx
    je      .L142
    leaq    arena_process(%rip), %rsi
.L148:
    xorl    %eax, %eax
    tzcntq  %rcx, %rax                     // 次の生存スロット（1 命令）
    andl    $63, %eax
    addq    $24, %rax
    salq    $5, %rax
    addq    %rsi, %rax
    movq    8(%rax), %rdx
    addq    $1, %rdx
    jc      .L160                          // 検査付き加算のあふれ（取られない）
    movq    %rdx, 8(%rax)
    blsr    %rcx, %rcx                     // 最下位の 1 を消す
    jne     .L148
.L142:
    ret
```

AArch64（clang 18、`-mcpu=cortex-a72`）では `rbit` + `clz` になります。ARM の `BRK` はブレークポイント命令で、ビット走査には使えません。ARMv8.9 / v9.4 の FEAT_CSSC がある CPU なら、`-march=armv8.9-a` で `ctz` 1 命令になります。

```asm
.LBB28_5:                                  // 生存スロット 1 個ごと
    rbit    x12, x11
    clz     x12, x12                       // 次の生存スロットの添字
    add     x12, x10, x12, lsl #5
    ldr     x13, [x12, #8]!
    adds    x13, x13, #1
    b.hs    .LBB28_7                       // あふれ（取られない）
    str     x13, [x12]
    sub     x12, x11, #1
    ands    x11, x12, x11                  // blsr
    b.ne    .LBB28_5
```

### 1-2 マスク付きの集計（条件付きの総和・個数・最小 / 最大）

1.0.0 の総和の特殊化（スロットのフィールドを足すだけのループ）を広げ、条件で絞り込むループも分岐なしの走査関数にしました（`src/codegen.rs` の `reduction_plan` / `sum_action` / `minmax_plan` / `cond_ok` / `cond_c` / `emit_reduction`）。

| 本体の形 | 走査 | 死んだスロットの外し方 |
| --- | --- | --- |
| `if 条件 { acc += フィールド + .. }` | 表を先頭から 4 レーンで | フィールドは死んだスロットで 0 なので、条件のマスク `m = 0 - 条件` を AND するだけ |
| `if 条件 { acc += 定数 }` | 生存ビットマップの語ごと、64 スロット × 4 レーン | 生存ビット AND 条件 を足す。条件なしなら `st.live × 定数`（走査なし） |
| `if フィールド < acc { acc = フィールド; pick = Some(id); }`（< <= > >=、`&&` で条件を足せる） | 語ごと、2 レーン | 生存ビット AND 条件 AND 比較 = k |

条件に使えるのは、スロットのフィールド・整数 / bool の定数・static・外のローカル（走査関数の引数になる）の比較と、`&&` `||` `!`、ビット演算です。どれも副作用がないので、`&&` は `&` にして全部を計算します。死んだスロット（中身は 0）に対して評価しても害はありません。語がすべて 0（64 スロットが全部死んでいる）なら、その 64 スロットは読みません。

条件付きの個数の生成 C と、内側のループ（AArch64）:

```c
for (uint32_t j = 0u; j < 64u; j += 4u) {
    c0 += ((uint32_t)(lw >> (j)) & 1u) & (uint32_t)((p[j].v.state == fsc_RUNNING));
    c1 += ((uint32_t)(lw >> (j + 1u)) & 1u) & (uint32_t)((p[j + 1u].v.state == fsc_RUNNING));
    ...
}
```

```asm
.LBB29_6:                                  // 4 スロットで 30 命令、分岐はループの戻りだけ
    lsr     x0, x16, x17                   // 生存ビット
    and     x0, x0, #0x1
    ldur    w1, [x18, #-48]
    cmp     w1, #1                         // 条件
    csel    x0, x0, xzr, eq                // 生存ビット AND 条件（CSEL）
    ...
    add     x12, x0, x12
    cmp     x17, #60
    add     x17, x17, #4
    b.lo    .LBB29_6
```

最小 / 最大（CFS の pick_next）では、k = 生存ビット AND 条件 AND 比較 はいつも分岐なしで作ります。そのあとの更新 `F20_PICK` には 2 つの形があります。

```c
/* runtime/fascon2_rt.h */
#if defined(FASCON2_PICK_BRANCHLESS)
#define F20_PICK(k, T, bv, v, iv, idx) do { (bv) = (T)F20_SEL64((k), (v), (bv)); (iv) = F20_SEL32((k), (idx), (iv)); } while (0)
#else
#define F20_PICK(k, T, bv, v, iv, idx) do { if (FASCON_UNLIKELY(k)) { (bv) = (v); (iv) = (idx); } } while (0)
#endif
```

- **既定**: 更新は「めったに真にならない分岐」1 本。走査が進むほど新しい最小は出にくく（ばらばらな値なら平均 ln n 回）、予測がほぼ外れないので、毎スロット 2 つの値を選び直すより速くなります（pick_next 0.69〜0.73 ns）。予測しにくい条件（`state == RUNNING` など）は k の中に入っているので、分岐にはなりません。
- **`FASCON2_PICK_BRANCHLESS`**: 更新も条件付き選択にして、データに依存する分岐を一本もなくします（実行時間をデータに依存させたくないコード向け）。clang では `__builtin_unpredictable` を付けた `?:` が AArch64 の `CSEL`、x86-64 の `CMOV` になります。GCC は `?:` を「予測が効く」と見なして分岐に戻すことがあるため、全ビット 1 / 0 のマスク（AND / BIC / ORR）で書きます。4 つのコンパイラ（clang / GCC × AArch64 / x86-64）とも、内側のループの分岐は戻りの 1 本だけでした。この形は既定より遅く、pick_next は 1.5 ns（clang）/ 2.6 ns（GCC）でした。

`FASCON2_PICK_BRANCHLESS` のときの内側のループ（clang 18）:

```asm
// AArch64: 2 スロットで 28 命令、CSEL / CSINC、分岐は戻りだけ
.LBB31_6:
    ldur    x1, [x0, #-16]                 // v = vruntime
    cmp     x1, x10                        // v < best ?
    ldur    w2, [x0, #-20]
    ccmp    w2, #1, #0, lo                 // && state == RUNNING
    cset    w2, ne
    lsr     x3, x17, x18
    tst     x3, #0x1                       // 生存ビット
    csinc   w2, w2, wzr, ne
    cmp     w2, #0
    csel    x10, x10, x1, ne               // best = k ? v : best
    add     w1, w11, w18
    csel    w8, w8, w1, ne                 // idx  = k ? i : idx
    ...
    b.lo    .LBB31_6

// x86-64: CMOV
.LBB27_4:
    cmpl    $1, -20(%r15)
    movq    -16(%r15), %r13
    setne   %bpl
    cmpq    %rdx, %r13
    setae   %al
    orb     %bpl, %al
    btq     %r12, %rbx                     // 生存ビット
    setae   %bpl
    orb     %al, %bpl
    leal    -1(%r14), %eax
    testb   %bpl, %bpl
    cmoveq  %r13, %rdx                     // best
    cmovel  %eax, %ecx                     // idx
    ...
    jb      .LBB27_4
```

レーンは最後に 1 つにまとめます。`<` / `>` では同じ値なら添字の小さい方、`<=` / `>=` では大きい方を選ぶので、結果は逐次のループと同じです（`tests/latch` で乱数の確保・解放 3 万歩と照合）。

### 1-3 コミット点を回数の決まったループの外へ

1.0.0 では、ラッチを使う関数のすべてのループで、各周回の先頭にコミット点（`cbnz`）がありました。回数の決まったループ（`for i in a..b`・`for id in @a`）では、これをループの出口へ移しました。外へ影響の出る操作（呼び出し・出力・MMIO・return）の前には別にコミット点があり、ループは必ず終わるので、出口で 1 回確かめれば同じ保証になります。本体のジャンプが 1 本減り、C コンパイラがベクトル化できる形になります。

回数の決まらないループ（`while`・`loop`）は、終わらないかもしれないので、各周回で確かめます。ただし、ラッチを終了条件に混ぜて、ループの出口と同じ分岐にしました。

```c
while ((arena_process_at_r(id, &_flt)->ticks < v) && (k < UINT64_C(1000))) && F20_LATCH_OK(_flt)) {
    k = fascon_add_u64(k, UINT64_C(1));
}
F20_COMMIT(_flt, site);
```

```asm
.LBB27_1:                                  // clang AArch64: 3 つの条件が分岐 1 本に
    cmp     x9, x1
    add     x0, x0, #1
    ccmp    x0, x10, #2, lo
    csinc   w11, w8, wzr, ls
    cmp     w11, #1
    b.ne    .LBB27_1
```

`loop { match cur { Some(c) => .., None => break } }` のように、外から来た Id を連鎖でたどるループでは、ラッチの確認が予測の当たる分岐として残ります（コンパイラがループの出口へつなぎ替えます）。読み出しの番地を比較の結果で選ぶようにすれば分岐は消えますが、連鎖 1 歩ごとの待ち時間が伸びるので採っていません。

## フェーズ 2 — キャッシュラインと偽共有

### 表の配置（再定義）

```c
#define F20_CACHE_LINE 64u
#define F20_DESTRUCTIVE_SIZE 128u   /* Intel の隣接ラインプリフェッチ・Apple M シリーズの 128 B ライン */

#define F20_DEFINE_TABLE(tag, K, GenT)                                           \
    typedef struct F20_ALIGN128 {                                                \
        F20_ALIGN128 f20_arena_state_t st;                                       \
        uint8_t st_pad_[F20_DESTRUCTIVE_SIZE - sizeof(f20_arena_state_t)];       \
        F20_ALIGN128 uint64_t live[F20_LIVE_WORDS(K)];                           \
        F20_ALIGN128 GenT gen[F20_SLOTS(K)];                                     \
        F20_ALIGN128 uint32_t link[F20_SLOTS(K)];                                \
        F20_ALIGN128 f20_slot_##tag##_t slot[F20_SLOTS(K) + 1u];                 \
    } f20_arena_##tag##_t
```

- **メタデータはデータと同じラインに置かない（SoA）。** 生存印をスロットの中に置くと、走査は死んだスロットのラインまで読むことになり、1 スロットも大きくなります。生存ビットマップは 65,536 スロットでも 8 KiB で、L1 に収まります。
- **確保のたびに書く `st` は、128 バイトのブロックに 1 個だけ。** 1.0.0 では `st`（32 バイト）の直後から `gen[]` が始まっていて、確保の書き込みが、ほかのコアが世代の検査で読む `gen[0..]` のラインを無効化する配置でした。いまはそうなりません。
- **コアに割り当てる単位は 512 スロット（`F20_SHARD_SLOTS`）。** 512 スロットなら `live[]` は 64 バイト、`gen[]`・`link[]`・`slot[]` はどれも 128 バイトの倍数になります。各配列は 128 バイト境界から始まるので、担当の境目がどの配列でも 128 バイト境界に来て、どのラインも 1 つのコアしか書きません。64 スロットの `@process` は 1 つの単位に収まるので、コアごとに分けるなら、コアごとのアリーナにします。
- **スロットそのものは 16 バイト境界のまま。** 走査の速さは 1 スロットのバイト数で決まるので、全スロットを 64 バイトにすると、16 バイトの Task では読む量が 4 倍になります。1 スロット 1 ラインが要る表（コアごとの実行キューなど）にだけ、`#[repr(align(N))]` を付けます。

小さく熱い管理情報（`st`・`live`・`gen`）を前に、大きなスロットの列を後ろに置いています。表は 2 MiB 境界から始まるので、管理情報とスロットの大部分が最初の 2 MiB ページに入ります。`slot[]` を先頭に置く形も試しましたが、ばらばらの順の連鎖が GCC で 20〜30 % 遅くなった（`gen[]` がスロットから 2 MiB 離れる）ので採りませんでした。なお、この比較は大きなページが効いていることを確かめる前に測ったものです。

### `#[repr(align(N))]`

```rust
#[repr(align(64))]
struct PerCpu { ticks: u64, runnable: u32 }

arena @cpu: PerCpu[4];          // スロットが 64 バイト境界（1 スロット 1 ライン）
```

```c
struct __attribute__((aligned(64))) fsc_PerCpu { uint64_t ticks; uint32_t runnable; };
_Static_assert(_Alignof(struct fsc_PerCpu) >= 64u, "#[repr(align(64))]");
```

N は 1〜4096 の 2 の累乗です（それ以外は **F2031**）。own の所有リージョンは 16 バイト境界なので、16 を超える整列の型（を値として含む型）を own に入れると **F2032** になります。

## フェーズ 3 — MMU と大きなページ

### ページテーブル（`bsp/qemu-virt-aarch64/bsp.c`）

仮想アドレスは 39 ビット（T0SZ = 25）、4 KiB 粒度なので、表の引きはレベル 1（1 エントリ 1 GiB）から始まります。1.0.0 は RAM を、レベル 1 → レベル 2 の 2 MiB ブロックで引いていました（2 段）。

```c
#define PTE_CONT (1ull << 52)  /* 連続ビット */

if (!fb) {
    /* 画面がない: RAM（1〜2 GiB）を 1 GiB ブロック 1 枚に。引きはレベル 1 で終わる（1 段） */
    l1_table[1] = RAM_BASE | PTE_VALID | PTE_ATTR(1) | PTE_AF | PTE_SH_INNER;
} else {
    /* 画面がある: 2 MiB ブロック。属性のそろった 16 個（32 MiB 境界）には連続ビット */
    l1_table[1] = (uint64_t)(uintptr_t)l2_ram | PTE_TABLE;
    ...
    for (uint32_t g = 0; g < 512u / 16u; g++)
        if (/* 16 個の属性が同じ */) for (uint32_t k = 0; k < 16u; k++) l2_ram[g * 16u + k] |= PTE_CONT;
}
```

ページテーブルは起動時に 1 回書くだけで、実行中に書き換えることはありません。4 KiB のページ（レベル 3）もどこにもありません。

```text
[bsp] MMU + caches on (identity map, no 4 KiB pages: RAM = one 1 GiB block, 1-level walk)
[bsp] arenas: 3 tables, 0x0000000040200000..0x0000000040205a80 (22 KiB, 2 MiB aligned) -> inside the 1 GiB RAM block (1 TLB entry for all RAM)

[bsp] MMU + caches on (identity map, no 4 KiB pages: RAM in 2 MiB blocks, 31 of 32 groups contiguous = 32 MiB TLB entries; framebuffer pages are Normal non-cacheable)
```

1 GiB ブロックや連続ビットの範囲を TLB の 1 エントリで持つかどうかは、CPU の実装次第です。持たない CPU でも正しく動き、引きが短くなる点は同じです。QEMU の TCG は TLB を模していないので、QEMU 上では速さの差は出ません。

### アリーナの置き場所

生成 C はアリーナを `.bss.fascon_arena` に置き、1 MiB 以上の表には 2 MiB 境界を付けます。BSP のリンカスクリプトはこれを 2 MiB 境界の `.fascon_arena` にまとめ、`start.S` が 0 で埋めます。

```c
#define F20_ARENA_STORAGE(T) __attribute__((section(".bss.fascon_arena"), \
        aligned(sizeof(T) >= F20_HUGE_MIN ? F20_HUGE_PAGE : F20_DESTRUCTIVE_SIZE)))
static f20_arena_t(task) arena_task F20_ARENA_STORAGE(f20_arena_t(task));
```

```ld
.fascon_arena (NOLOAD) : ALIGN(0x200000) {
    __arena_start = .;
    *(.bss.fascon_arena .bss.fascon_arena.*)
    . = ALIGN(16);
    __arena_end = .;
}
```

PC での実行（`fascon run`、Linux）では、1 MiB 以上のアリーナに、最初に触る前に `madvise(MADV_HUGEPAGE)` を掛けます（`FASCON_NO_HUGEPAGE=1` で止められます）。大きなページが効いた状態では、x86-64 の PC で素の総和が 13〜17 %、ばらばらの順の Id の連鎖が 16〜31 % 速くなりました（「測った結果」を参照）。大きなページにできるのは 2 MiB 境界にそろった 2 MiB の範囲だけなので、ベンチマークの 2.4 MiB の表では 2 MiB ページが 1 枚になり、最後の 0.4 MiB は 4 KiB ページのままです。Linux がページを割り当てるかどうかは環境しだいです（THP が `madvise` か `always` であること、空いた 2 MiB ページがあること）。効いたかどうかは `/proc/<pid>/smaps` の `AnonHugePages` で分かります。macOS と Windows は実行形式が 2 MiB の整列を持てないので、128 バイト境界だけです。

### 「1 コール = 1 リージョン」をフレームリージョンで

エスケープ解析（`FlowResults::moved_locals`）で、どこへも移動しないと分かった `let x = own T { .. }` は、ヒープの空きリストではなく、事前に確保したフレームリージョンから取ります。

```c
uint8_t *_fm2 = fascon_frame_mark();
fsc_Big *_o3 = (fsc_Big *)fascon_frame_alloc(FASCON_OWN_SPAN(fsc_Big), "frame.fsc:19:13");
*_o3 = (fsc_Big){ ... };
fsc_Big *x = _o3;
...
fascon_frame_release(_fm2);    /* x のスコープの終わり（break・continue・return でも） */
```

- 確保はインライン展開した比較 1 回と加算 1 回、解放は先頭ポインタを戻すだけです（スタックと同じ動き）。MMU にもページテーブルにも触りません。
- ループの中の `x` は周回ごとに戻るので、何周してもフレームは伸びません。呼び出し先のフレームは戻る前に解放されるので、後入れ先出しが保たれます。
- 不変の `x` は根を差し替えられないので、根の解放（見出しの確認）も省き、子だけを解放します。
- 返す・構造体やアリーナへ入れる・値で渡す・`drop` する own は、これまでどおりヒープです。フレームがあふれたときはヒープへ切り替わります（結果は同じ）。
- BSP はリンカスクリプトの `.frame`（1 MiB）を `fascon_frame_init` で登録します。PC 実行の既定は 64 KiB です。
- `FASCON2_CPU_LOCAL` は 1 コアのビルドでは空、`FASCON2_SMP` を定義したビルドでは `__thread` になり、フレームリージョンはコアごとに 1 つになります（[SPEC-1.0-SMP.ja.md](SPEC-1.0-SMP.ja.md)）。`FasconFrame` は 128 バイト境界なので、コアごとのフレームが同じラインに乗ることはありません。

## フェーズ 4 — 列: 集計は自分が読む値だけを読む

フェーズ 1〜3 のあと、素の総和には分岐がもう残っていないのに、1 タスクあたり 1.2〜1.3 クロックかかっていました。vruntime の 8 バイトを足すために、32 バイトのスロットをまるごと読んでいたからです。フェーズ 4 はこの余分なバイトをなくします。コンパイラはプログラム中の `for id in @a` の集計ごとに、その集計が読む値だけを並べた列を表に足し、走査はスロットではなくその列を読みます。ソースは変わりません。`@a.get(id)` も今までどおりスロットへのポインタを返します。正本はスロットで、列はコンパイラが同期を保つ写しです（フェーズ 5 では、条件を満たすフィールドについて列が唯一の正本になります）。`--columns off`（環境変数 `FASCON_COLUMNS=off`、VS Code の設定 `fascon.columns`）で 1.0.0 と同じ表と走査に戻ります。

### 4-1 集計と列の対応

| 集計（`reduction_plan` が認める形） | 列 | 1 スロットあたり | 走査 |
| --- | --- | ---: | --- |
| 総和 `s += @a[id].f`（項が複数なら項ごとに 1 本） | 集計の列 `agg[i] = f` | 8 B（u64）/ 4 B（32 ビット以下） | `f20_col_sum_u64` / `f20_col_sum_u32` |
| 条件付き総和 `if c { s += @a[id].f }` | 集計の列 `agg[i] = c ? f : 0` | 8 B / 4 B | 同じ |
| 条件付きの個数 `if c { n += k }` | ビット列: i ビット目 = 生存 && c | 1 ビット | `f20_col_count_bits`（64 スロットに popcount 1 回） |
| 最小 / 最大の選択 `if @a[id].f < best (&& c) { best = ..; pick = Some(id); }` | 鍵の列 `key[i] = 生存 && c ? enc(f) : NONE`（最大は `~enc(f)`） | 8 B | `f20_col_pick_key`（符号付きの最小） |

enc(v) は u64 なら v ^ 2^63（符号なしの順序を符号付きの順序へ写す）、それ以外の整数は int64 へ広げるだけです。NONE は INT64_MAX です。死んだスロットは、集計の列で 0、ビット列で 0、鍵の列で NONE になっているので、走査には条件も生存のマスクも要りません（どちらも列に入っています）。同じ列（種類・値・条件が同じ）を使う集計は 1 本の列を共有します。条件のない個数には列は要りません（`st.live` です）。

次の 2 つはフェーズ 1 の走査のままです。外のローカル変数（引数など）を読む条件（要素を書く時点では値が分からない）と、64 ビットの `<=` / `>=` の選択（NONE と同じ鍵を持つ値がちょうど 1 つあり、最小なら UINT64_MAX。`<=` ではその値が答えになりうるが、`<` ではなりえない）です。`fascon regions` に、列ごとの大きさと、その列を読む行が出ます:

```text
  列を読む集計（スロットを読まず、その集計が読む値だけの列を走査）: 4 （列は表に 1,843,236 B を足す）
    task.agg0_vruntime           集計の列（u64）            8 B/スロット      524,296 B  (行 93)
    task.agg1_prio               集計の列（u32）            4 B/スロット      262,148 B  (行 93)
    task.fb2                     絞り込みのビット列         1 bit/スロット      8,200 B  (行 106)
    task.min3_vruntime           選択の鍵の列（最小）       8 B/スロット      524,296 B  (行 119)
    task.agg4_vruntime           集計の列（u64）            8 B/スロット      524,296 B  (行 131)
  列の写しを作り直す場所（Id での書き込み・&mut の借用の終わり・&mut を渡した呼び出し）: 1
```

列は `live[]` のすぐ後ろ（`F20_DEFINE_TABLE_COLS`）に、それぞれ 128 バイト境界から置きます。走査が読むものは、すべて表の先頭にまとまります:

```c
typedef struct F20_ALIGN128 {
    F20_ALIGN128 uint64_t agg0_vruntime[F20_COL_LEN(ARENA_task_K)]; /* 集計の列: (*v).vruntime（93 行目の総和） */
    F20_ALIGN128 uint32_t agg1_prio[F20_COL_LEN(ARENA_task_K)];     /* 集計の列: (*v).prio（93 行目の総和） */
    F20_ALIGN128 uint64_t fb2[F20_COL_WORDS(ARENA_task_K)];         /* ビット列: 生存 && ((*v).state == fsc_RUNNING) */
    F20_ALIGN128 int64_t min3_vruntime[F20_COL_LEN(ARENA_task_K)];  /* 鍵の列: 生存 && (state == RUNNING) ? enc(vruntime) : NONE */
    F20_ALIGN128 uint64_t agg4_vruntime[F20_COL_LEN(ARENA_task_K)]; /* 集計の列: (state == RUNNING) ? vruntime : 0 */
} f20_cols_task_t;
F20_DEFINE_TABLE_COLS(task, ARENA_task_K, f20_gen16_t, f20_cols_task_t); /* st | live | col | gen | link | slot */
```

### 4-2 列とスロットを同期させる

列を書くのは、要素が変わりうる場所にコンパイラが出すコードだけです:

| 場所 | 出すもの |
| --- | --- |
| `@a.alloc(v)` | 値を書いたあとに `arena_<名前>_mirror(&slot[i].v, i)`（すべての列） |
| `@a.free(id)` / `@a.remove(id)` | `arena_<名前>_unmirror(i)`: 集計 0・ビット 0・鍵 NONE |
| Id での書き込み（`@a[id].f = x`、`+=`、入れ子のフィールド、要素まるごと） | 要素ポインタを場所の式の中で一時変数にも取り、書いたあとで、この書き込みが届く列だけを作り直す（`arena_<名前>_mirror_m<マスク>`）。どの列も読まないフィールドへの書き込みでは何も出さない |
| `if let Some(p) = @a.get_mut(id)` / `match` | スコープのすべての出口（終わり・`break`・`continue`・`return`）で、`p` を通した書き込み（代入、`&mut p.f` の引数、`&mut` として渡した `p`）が届く列 |
| `match @a[id].f { q => .. }`・`match @a[id] { t => .. }`・`if let Some(q) = @a[id].o`（スロットの中の場所の `&mut` の束縛） | アームのすべての出口（終わり・`break`・`continue`・`return`）で、束縛を通した書き込みが届く列（場所までの道筋のあとに、束縛を通して書く道筋を続けたもの）。要素ポインタは、Id での書き込みと同じく場所の式の中で取る |
| `&mut @a[id]` や `&mut @a[id].f` を渡した呼び出し | 呼び出しの直後に、その道筋の下の列 |

借用の間に写しが古くても、それは誰にも見えません。`@a` への `&mut` の参照が生きている間、1.0 は借用した場所と重なる `@a` の読み出し（`get_mut` の束縛や要素まるごとの束縛なら `@a` のすべて）・`@a` を読む関数の呼び出し・重なる Id での書き込みを拒否します（F2502）。集計は自分の列が読むフィールドをすべて読むので、借用が届きうる列を使う集計は借用が終わるまで走れません。そして借用が終わる場所で、ちょうど写しを作り直します。`tests/err/f2502_*` がこの規則を確かめます。

書き込みが届く列: 各列は、自分が読む要素のフィールドの道筋（値と、条件に出てくるすべてのフィールド）を覚えています。道筋 P への書き込みは、P がその道筋のどれかの先頭部分なら届きます。`@a[id] = ..`（P = []）はすべての列に、`@a[id].s = ..` は `s.state` を読む列に届き、`@a[id].files += 1` はどの列にも届きません。`own` の先や配列の中の場所はどの列にも届きません（列はどちらも読まないため）。添字はポインタの差から求めます。古い Id での書き込みはスロットの番兵へ逸れていて（フェーズ 1）、その写しは列の番兵 `F20_COL_SENTINEL(K) = max(2^K, 64)` へ行きます。列の番兵はどの走査も読みません。鍵の列は、64 スロットの語を初めて使う確保が、その語を NONE で埋めます。

```c
static inline FASCON_MAYBE_UNUSED fsc_Id_task arena_task_alloc(const fsc_Task *v) {
    const f20_id_t id = f20_arena_acquire(&arena_task.st, arena_task.gen, arena_task.link, arena_task.live, ARENA_task_CAP, ARENA_task_K);
    if (id != 0u) {
        const uint32_t i = F20_ID_INDEX(id, ARENA_task_K);
        arena_task.slot[i].v = *v;
        if ((i & 63u) == 0u && arena_task.st.next_unused == i + 1u) { /* 新しい 64 スロットの語: 鍵の列を NONE で埋める */
            f20_col_key_fresh(arena_task.col.min3_vruntime, i);
        }
        arena_task_mirror(&arena_task.slot[i].v, i);
    }
    return id;
}
static inline FASCON_MAYBE_UNUSED void arena_task_mirror(const fsc_Task *F20_RESTRICT v, uint32_t i) {
    arena_task.col.agg0_vruntime[i] = (uint64_t)(*v).vruntime;
    arena_task.col.agg1_prio[i] = (uint32_t)(*v).prio;
    F20_BIT_PUT(arena_task.col.fb2, i, ((*v).state == fsc_RUNNING));
    arena_task.col.min3_vruntime[i] = (((*v).state == fsc_RUNNING)) ? F20_KEY_U64((*v).vruntime) : F20_KEY_NONE;
    arena_task.col.agg4_vruntime[i] = (((*v).state == fsc_RUNNING)) ? (uint64_t)(*v).vruntime : 0u;
}
```

### 4-3 走査

集計の関数は 1.0.0 と同じ名前・同じ引数のままなので、呼び出す側は変わりません:

```c
/* 93 行目: for id in @task { if let Some(t) = @task.get(id) { s = s + t.vruntime + (t.prio as u64); } } */
static FASCON_MAYBE_UNUSED uint64_t fsc_reduce_task_1(const f20_arena_t(task) *__restrict__ arena, uint32_t *__restrict__ flt, uint64_t acc) {
    const uint32_t n = F20_COL_SCAN_END(arena->st.next_unused);       /* next_unused を 64 に切り上げ */
    const uint64_t s0 = f20_col_sum_u64(arena->col.agg0_vruntime, arena->live, n, flt);
    const uint64_t s1 = f20_col_sum_u32(arena->col.agg1_prio, arena->live, n);
    uint64_t r = acc;
    uint32_t ov = 0u;
    ov |= (uint32_t)__builtin_add_overflow(r, s0, &r);
    ov |= (uint32_t)__builtin_add_overflow(r, s1, &r);
    *flt |= ov << 1; /* F20_FAULT_ARITH */
    return r;
}
/* 119 行目: pick_next */
static FASCON_MAYBE_UNUSED void fsc_reduce_task_3(const f20_arena_t(task) *__restrict__ arena, uint64_t *__restrict__ acc, fsc_Id_task *__restrict__ pick) {
    int64_t best = F20_KEY_U64(*acc);
    const uint32_t i = f20_col_pick_key(arena->col.min3_vruntime, arena->live, F20_COL_SCAN_END(arena->st.next_unused), &best, 0);
    if (i != 0xFFFFFFFFu) {
        *acc = ((uint64_t)F20_UNKEY_U64(best));
        *pick = F20_ID_MAKE(arena->gen[i], i, ARENA_task_K);
    }
}
```

本体は `runtime/fascon2_rt.h` にあり、GCC / clang のベクトル拡張だけで（組み込み関数なしで）ターゲット本来の幅で書いています。AVX-512VL なら 64 バイト、AVX2 なら 32、それ以外（SSE2・NEON、SIMD のないターゲットでのふつうのスカラ）は 16 です。ハードウェアより広いベクトルにしないのは、GCC がそれを 1 レーンずつのスカラに分解してしまうからです。

- **総和**は 64 スロットの塊ごとに、4 本の独立なベクトルの累計で足します。生存ビットマップの語が 0 の塊は飛ばします（64 スロットに予測の当たる分岐 1 本）。あふれの検査はループの外です。u64 の本体は値の OR も取り、ビット長(OR) + ビット長(n) ≤ 64 なら、どの部分和も 2^64 に届かないので、素の和がそのまま正確です。そうでないときだけ正確な走査（上位・下位の半分を別々に足す）をやり直し、合計が 2^64 に届けば F20_FAULT_ARITH をラッチします。値は符号なしなので、これは逐次の検査付き加算と同じ判定です。u32 の本体は列を u64 のレーンとして読み、マスクとシフトで 2 つに分けます。2^24 スロット × 2^32 未満の値では、64 ビットのレーンは回り込みません。
- **個数**は 64 スロットに popcount 1 回です（x86-64: POPCNT、AArch64: CNT + ADDV）。
- **選択**は、64 スロットの塊ごとに鍵のベクトルの符号付き最小（4 本の鎖。AVX-512VL: VPMINSQ、AVX2: VPCMPGTQ + VPBLENDVB、AArch64: CMGT + BSL）を取り、最良の塊を CMOV / CSEL で選び直します（`<` なら同じ値で先の塊、`<=` なら後の塊が勝つ）。そのあと勝った塊の中だけで「鍵 == 最小」のビットを作り、最初の 1（TZCNT / RBIT+CLZ）か最後の 1（LZCNT / CLZ）を取ります。結果は逐次のループと同じです（`<` なら最小の値のうち添字が最も小さいもの、`<=` なら最も大きいもの）。`*best` にはループの前の値の鍵が入っているので、それより真に良い値（`<=` なら同じ値も）だけが選ばれます。64 ビットのベクトル比較が 1 命令にならないところ（素の SSE2・ARMv7・SIMD なし）では、塊の最小はスカラの CMOV / CSEL の鎖 4 本です（`F20_VEC_I64_CMP=0`）。
- **x86 の GCC** は、ベクトルの読み込み 1 回を 2 つの命令のメモリオペランドに畳み、同じ 16 バイトを 2 回読んでいました。SSE2 の総和はそれで読み込みに詰まっていました（1 タスク 0.29 ns）。中身のない `asm` で読み込んだベクトルをレジスタに置くと（`F20_KEEP`、SSE2 のある GCC だけ）0.20 ns になりました。
- `fascon run` は生成 C を `-march=native`（x86-64）/ `-mcpu=native`（AArch64）でコンパイルするようになり、最も広いベクトルを使います（持ち運べる実行ファイルが要るときは `--cpu baseline`）。

### 4-4 代わりに払うもの

- **メモリ:** 列 1 本につき 4〜8 バイト × 2^K（ビット列は 2^K ビット）。`bench/sched_bench.fsc` では、5 本の列が 2.50 MB の表に 1.84 MB を足します。列も表と同じく `.bss` にあるので、PC では触れていないページの費用はありません。QEMU の BSP は今までどおり起動時に `.bss` を消します。
- **列に届く書き込み:** 1 回の書き込みにストア数個。実行可能なタスクすべてのそのフィールドを書くループ（`charge_running`、`vruntime += 1`）は約 2 倍の時間になりました。原因は命令ではなく、そのループが新たに触れる列のキャッシュラインで、作業領域が 2 MiB の L2 を超えます（増えた命令はシフト・XOR・ストア 3 つで、ストアを 1 つにしても差の大部分が残りました）。どの列も読まないフィールドへの書き込み（`tick_running`、`files += 1`）は変わりません。
- 書き込みが多く、走査がまれなプログラムは `--columns off` を使えます。
- フェーズ 5 で、完全列化できるフィールドではこの二重書き込みがなくなり、完全列化できず書き込みの多いフィールドには写しの列を作らなくなりました（`--columns mirrored` はこの節のまま）。

### 4-5 確かめたこと

- `tests/cols/cols.fsc` は、集計のすべての形（項が 1 つと複数の総和、u8〜u64、条件付き総和、0 のスロットで真になる条件の個数、`<`・`>`・`<=` の鍵の選択、`if let Some(t) = @a.get(id)` の形、1.0.0 の走査に残る 2 種類、列の番兵が 64 になる 4 スロットのアリーナ、正確な走査が要る 2^62 前後の値）を、すべての書き込みの道筋（alloc・free・remove、Id での単純・複合・入れ子・要素まるごとの書き込み、終わり・`break`・`continue`・`return` で抜ける `get_mut` のスコープ、`&mut` の引数、`take()`、ビットマップのループの中の書き込み、配列のフィールド）のあとに取り、第 2 段ではスロットの中の場所を束縛した `match` / `if let` を通した書き込み（フィールド・要素まるごと・入れ子・値になる match・`break`・`continue`・`return` で抜けるアーム・ビットマップのループの中）のあとに取ります。出力は `--columns on` と `--columns off` で同じで、PC でも BIOS なしの QEMU（NEON、`-mstrict-align`）でも同じでした。第 1 段は出荷時の 1.0.0 のコンパイラとも同じ出力です（出荷時の 1.0.0 は第 2 段をビルドできません。その束縛の C の生成がこの改修で直した不具合です。変更履歴を参照）。生成された C から新しい写しの作り直しを取り除くと、第 2 段の出力が 41 行変わります。
- `tests/cols/colfuzz.py` はランダムなプログラム（要素の形・条件・集計・書き込みの道筋がばらばら。ラッチに掛かる古い Id での書き込みや、あふれる総和も含む）を作ります。約 1,400 本（最後の約 300 本は最終版のコード）を `--columns on` と `off` で走らせて比べ、出力はすべて一致しました: GCC と clang、SSE2・SSE4.2・AVX2・AVX-512、GCC の AddressSanitizer + UndefinedBehaviorSanitizer、qemu-user の AArch64。`run_tests.py --fuzz N` で N 本を走らせます（既定 8 本）。
- 生成 C は ARMv7（NEON と soft-float）・i386（SSE2 あり・なし）・RISC-V 32 / 64・`-mstrict-align` の AArch64・SSE2 なしの x86-64 でもコンパイルできます。

## フェーズ 5 — 完全列化（True SoA）・非時間的ストア・書き込みの多いフィールドの除外

フェーズ 4 の列は写し（Mirrored-SoA）で、正本はスロットでした。列が読むフィールドを書くと、スロットと列の両方を書きます（二重書き込み）。表全体でそのフィールドを書くループでは、それが約 2 倍の時間と、L2 を超える作業領域になっていました（4-4）。フェーズ 5 では、条件を満たすフィールドをスロットから外して列を唯一の正本にし（完全列化、True SoA。書き込みは列への 1 回のストア）、大きな列の一括上書きには非時間的ストアを使い、完全列化できないフィールドのうち書き込みの多いものには写しの列を作りません。ソースは変わりません。版も 1.0 のままです。

`--columns` は `on`（既定。完全列化 + 写しの列 + 書き込みの多いフィールドの除外）・`mirrored`（フェーズ 4 のまま。完全列化も除外もしない）・`off`（1.0.0 の表）の 3 つになりました。`tests/ok`・`tests/cols`・`tests/latch`・`bench`・`examples` の 59 本で、`--columns mirrored` と `--columns off` の生成 C は、この改修の前のコンパイラの出力とバイト単位で同じです。

### 5-1 完全列化できる条件

集計が値として読む、要素の 1 段目の 32 / 64 ビットの整数フィールド（u32・i32・u64・i64）が候補です。次のすべてを満たすと完全列化します。どれかを満たさなければ写しの列（Mirrored-SoA）か 1.0.0 の走査（AoS）のままで、理由が `fascon regions` に出ます。

| 条件 | 満たさないときの理由（`fascon regions`） |
| --- | --- |
| 表のアリーナ（shared・task・dma・bump・gc・プロセスの表でない） | `a shared arena (one instance for every core): never True SoA` など |
| 要素が `#[stable_abi]`・`#[wire]`・`#[secret]`・`#[repr(align(N))]` でなく、own を持たない | `the element ... is #[stable_abi] (fixed layout)` など |
| 要素の型とこの表の `Id` が `extern "C"` の関数の引数・戻り値に現れない（1.0 で境界を越えられるのは整数・bool・Id・Option<Id>・FaultError なので、表が境界に出るのは Id を通して） | `the element ... or an Id of @a crosses the extern "C" boundary of ...` |
| `#[on_drop(f)]` の f が同じアリーナに触れない | `#[on_drop(f)] touches the same arena` |
| 要素への参照が外へ出ない: `@a.get(id)` / `get_mut(id)` の束縛はフィールドの読み書きだけ。`&@a[id]`・要素のメソッド呼び出し・要素を束縛する `match` / `if let` がない。要素まるごとの読み書き（`@a[id] = T { .. }`・`@a[id].clone()`）は組み立て直しで扱う | `a reference to the element ...` / `a get / get_mut binding ... escapes` など（行番号つき） |
| フィールドのアドレスを取らない（`&@a[id].f`）・フィールドの `match` / メソッド呼び出しがない | `its address is taken (&@a[id].f, line N)`（このフィールドだけが写しに残る） |
| 条件（絞り込み）に出てこない・1.0.0 の走査に残る集計に読まれない・atomic / `#[no_column]` でない | `appears in a filter` / `read by a reduction that keeps the 1.0.0 scan` |
| usize / isize（ポインタの幅）・32 ビットより狭い整数でない | `usize / isize (pointer-sized) stays a mirrored column` など |

判定は表ごとにプログラム全体を見ます（`src/codegen_tsoa.rs` の `plan_tsoa`）。条件に出てくるフィールドは、今までどおり絞り込みのビット列（生存 && 条件）に写し、スロットにも残ります。

### 5-2 生成される C

`bench/tsoa_bench.fsc.in` の Task（`pid, prio, vruntime, parent, next, files, state`）で、`vruntime` が完全列化されたときの表です。スロットには残りの 24 バイトだけがあり、16 バイト境界には上げません（完全列化の表は FFI の境界を通らないため。32 バイトに上げると、`state` を読んで `vruntime` を書くループが 6 万タスクで 1.03 倍 → 1.34 倍、400 万タスクで 1.14 倍 → 1.32 倍（C 比、GCC 基本、3 回の中央値）に遅くなりました。ほかの行は同じでした）。

```c
#define ARENA_task_TRUE_SOA 1 /* a True-SoA table (field f is arena_<name>.col.ts_f[i]) */
#define ARENA_task_TS_vruntime 1
typedef struct {
    uint32_t pid;
    uint32_t prio;
    fsc_Id_task parent;
    fsc_Id_task next;
    uint32_t files;
    uint32_t state;
} f20_part_task_t;                    /* スロットに残る部分 / the part left in the slot */
typedef struct {
    f20_part_task_t v;
} f20_slot_task_t;                    /* 自然な整列（24 B） / natural alignment (24 B) */
typedef struct F20_ALIGN128 {
    F20_ALIGN128 uint64_t fb0[F20_COL_WORDS(ARENA_task_K)];        /* 写し: 生存 && state == RUNNING */
    F20_ALIGN128 uint64_t ts_vruntime[F20_COL_LEN(ARENA_task_K)];  /* 唯一の正本（死んだスロットは 0） */
} f20_cols_task_t;
```

列は今までどおり `F20_COL_LEN(K) = F20_COL_SENTINEL(K) + 1` 要素で、番兵 `F20_COL_SENTINEL(K) = max(2^K, 64)`（K ≥ 6 なら `col[2^K]`）を末尾に持ちます。どの走査も番兵を読みません。

| 場所 | 完全列化の C |
| --- | --- |
| 証明済みの `@a[id].f` | `arena_task.col.ts_f[F20_COL_OF_SLOT(F20_ID_INDEX(id, K), K)]`。`for id in @a` のループ変数なら、ループのスロットの添字をそのまま使う（`gen[]` を読まない） |
| 証明できない `@a[id].f` の読み出し | `arena_task_ix_r(id, &_flt)`: 世代の比較はラッチへ流すだけ（`_at_r` と同じ）。古い Id で読むのは列の中の同じ型の値か 0 |
| 証明できない `@a[id].f` への書き込み | `arena_task_ix(id, &_flt)`: 世代が違えば番兵の添字へ分岐なしで逸らしてラッチ（`(i & (bad - 1)) \| (F20_SENTINEL(K) & -bad)`、列では `F20_COL_OF_SLOT` で列の番兵）。本物の要素は汚れない。解放処理のある要素（`#[on_drop]`）は `arena_task_ix_strict` でその場で止める（`_at_strict` と同じ） |
| `if let Some(t) = @a.get(id)` の `t.f` | `t` はスロットに残る部分へのポインタ。`t.f` は `ts_f[arena_task_ix_p(t)]`（ポインタの差がスロットの添字） |
| `alloc` | `arena_task_put(i, v)`: 残る部分をスロットへ、完全列化の値を列へ（初期値の書き込み先は列）、写しの列を作り直す |
| `free` / `remove` | スロットを 0 で埋め、`arena_task_ts_zero(i)` で完全列化の値も 0。`remove` が返す値は `arena_task_load(i)`（スロットと列から組み立て直す） |
| `#[on_drop]` | `free` は `arena_X_load(i)` で組み立て直した一時値に解放処理を掛けるので、`on_drop` の関数は列の値を見る。確保に失敗した値は、渡された値そのものを解放する |
| `@a[id] = T { .. }` | 場所（添字）→ 新しい値 → 古い値の解放 → `arena_task_put`（評価の順は今までと同じ） |
| `@a[id].clone()` | `arena_task_load` で組み立て直した一時値の複製 |

書き込みのあとの写しの作り直しは、完全列化したフィールドには要りません（`bench/tsoa_bench.fsc` で「places that rebuild column mirrors」は 3 → 0）。

### 5-3 走査（マスクは走査の時点で）

完全列化した値の条件付きの集計は、条件のビット列（または生存ビットマップ）を走査の時点のマスクにします。写しの集計の列・鍵の列は作りません。

| 集計 | 関数（`runtime/fascon2_rt.h`） |
| --- | --- |
| 総和（条件なし） | `f20_col_sum_u64` / `f20_col_sum_u32`（死んだスロットは列で 0。フェーズ 4 と同じ関数） |
| 条件付き総和 | `f20_col_sum_u64_m` / `f20_col_sum_u32_m`: 64 スロットの塊ごとにマスクの語が 0 なら飛ばし、塊の中はレーンのマスク（AND と 0 からの引き算）を AND する分岐なしのベクトル加算。レーンごとの可変シフトのない x86 の SSE2 / SSE4.2 では、マスクの 4 ビットで 512 B の表を引く（マスクを作る演算が要らない）。あふれはフェーズ 4 と同じ OR の上界 |
| 最小 / 最大の選択 | `f20_col_pick64_m` / `f20_col_pick32_m`: 鍵はその場で作る（`key = (int64)(値 ^ x)`、x は型と向きの定数）。段 1 は 256 スロットごとにレーンの最小を 1 回まとめ、段 2 は勝った 256 スロットで最初（`<` / `>`）か最後（`<=` / `>=`）の一致を引く。素の SSE2 では、語が全部 1 なら 4 本の CMOV の鎖、それ以外は立っているビットだけをたどる。逐次のループと同じ結果 |
| 個数 | `f20_col_count_bits`（変わらない） |

これらの関数は、GCC と clang の SSE2・SSE4.2・AVX2・AVX-512 で、ランダムな列・マスク・型・向き・`<` と `<=`（2 万回 × 各型）を素朴な参照と照合しました。

### 5-4 非時間的ストアとプリフェッチ

`runtime/fascon2_rt.h` に `F20_NT_STORE_U64` / `_U32` / `_I64` / `_I32`（clang は `__builtin_nontemporal_store`、GCC の x86-64 は `MOVNTI` の asm、ほかは普通のストア）、`F20_SFENCE()`（x86 は `SFENCE`、ほかは release の柵）、`F20_PREFETCH_R(p)` を足しました。

コンパイラは、本体がすべて `@a[id].f = 値;`（f は完全列化、値は f を読まない純粋な式）の `for id in @a` を一括上書きのループとみなし、`--nontemporal` で次のように決めます。非時間的ストアにしたループの出口には必ず `F20_SFENCE()` を置きます。

| `--nontemporal` | 非時間的ストアにするループ |
| --- | --- |
| `auto`（既定） | 列が 8 MiB 以上で、表が割り込みハンドラ・タスクと共有されておらず（`E_env`）、後ろの文（次のループまで）が同じ列を読まないもの |
| `on` | 一括上書きのループすべて（割り込みと共有する表も。出口に `F20_SFENCE`）。大きな表の列の走査にはプリフェッチ（`F20_PREFETCH_DIST 1024`）も入れる |
| `off` | なし |

shared arena と dma_arena はそもそも完全列化しないので、非時間的ストアになりません。フィールドの `#[no_column]` は、そのフィールドの列（写しも完全列化も）を作らせません。

8 MiB という下限は測って決めました（課題の例の「4096 要素 / 64 KiB」ではありません）。このホストで `vruntime = x` を全タスクに書くループ: 6 万タスク（480 KB の列）では、非時間的ストアのほうが GCC で 32 % 遅く（0.445 ns と 0.337 ns）、clang ではほぼ同じでした（0.664 ns と 0.680 ns）。400 万タスク（32 MiB の列。このホストの L3 は 260 MiB なので列は L3 に収まる）でも 14〜16 % 遅く（0.445〜0.450 ns と 0.384〜0.394 ns）、2^24 タスクの表 4 つ（列が合わせて 512 MiB。L3 を超える）では 2.3 倍速くなりました（0.49 ns と 1.13〜1.14 ns）。非時間的ストアが効くのは、列が最終段のキャッシュを超えるときです。8 MiB は、ふつうの機械の L3 を超え始める大きさとして選びました。このホストのように L3 の大きな機械では、8 MiB〜L3 の大きさの列で 14 % ほど損をします。

プリフェッチは `auto` では入れません。400 万タスクの列の走査に 256〜2048 B 先のソフトウェアのプリフェッチを入れても、このホストでは速くなりませんでした（素の総和は 0.273 ns のまま、ほかは遅くなったものもある）。ハードウェアのプリフェッチで帯域の上限に届いているためです。ハードウェアのプリフェッチの弱いボードでは `--nontemporal=on` か `-DF20_PREFETCH_DIST=1024` で入ります。

### 5-5 書き込みの多いフィールドの除外

完全列化できず写しの列に残るフィールドについて、プログラム全体で「走査の重み」と「一括書き込みの重み」を比べます。重みは、ループの深さ d の場所で 4^d（表全体の掃引 1 回を 1 とする）: 集計 1 つは 4^d、`for id in @a` の中の書き込みは 4^(d−1)、ループの外の書き込み 1 回は 4^d / 容量。書き込みの重みが走査の重みを上回るフィールドを読む集計には写しの列を作らず、1.0.0 の走査（スロットを読む AoS）に戻します。`tests/cols/cols.fsc` の `task`（要素を束縛する match があり完全列化できない）では `vruntime`（一括書き込み 8.04 > 走査 5.00）と `ticks`（4.06 > 3.00）がこれに当たり、`fascon regions` に次のように出ます。

```text
  reductions kept on the phase-1 scan (live bitmap + slots) without columns:
    task line 28: `vruntime` is write-heavy (bulk-write weight 8.04 > scan weight 5.00; the mirror's dual write would cost more)
```

この除外は `--columns on` だけで働きます（`mirrored` はフェーズ 4 と同じ列を作ります）。

### 5-6 `fascon regions` の出力（`bench/tsoa_bench.fsc.in`、4,194,304 タスク）

改修後:

```text
  arena task        Task[4194304]  slot = 24 B × (2^22 + 1 sentinel) + live/gen/link 25690112 B → 126353664 B (True SoA: vruntime in columns, sized below)
  reductions that read columns (they scan a column of just the values they read, never the slots): 4 (the columns add 34,078,736 B to the tables)
    task.ts_vruntime             True-SoA (single-write)    value column (u64)         8 B/slot       33,554,440 B  (line 41, 49, 70)
    task.fb0                     Mirrored-SoA (dual-write)  filter bit column          1 bit/slot        524,296 B  (line 49, 59, 70)
  places that rebuild column mirrors (writes by Id, ends of &mut borrows, calls given &mut): 0
  bulk-overwrite loops over True-SoA columns:
    task.vruntime line 101: non-temporal stores (F20_NT_STORE) — the column of 33,554,432 B ≥ 8,388,608 B (F20_SFENCE at the loop exit)
```

改修前（フェーズ 4）: スロット 32 B、写しの列 4 本（`agg0_vruntime`・`agg1_vruntime`・`fb2`・`min3_vruntime`、合わせて 101,187,616 B）、写しを作り直す場所 3。

完全列化しなかったフィールドは理由つきで出ます（`tests/ok/tsoa_fallback.fsc` から）:

```text
  fields kept off True SoA (they stay in the slot; any column they have is Mirrored-SoA (dual-write)):
    wire.x: the element `Wire` is #[wire] (a byte layout)
    ffi.v: the element `Ffi` or an Id of @ffi crosses the extern "C" boundary of `ffi_weight`
    esc.v: a reference to the element `&@a[id]` (line 99)
    addr.v: its address is taken (`&@a[id].v`, line 100)
```

### 5-7 測った結果

`bench/tsoa_bench.sh`（`bench/tsoa_bench.fsc.in` と `bench/tsoa_harness.c`）。比較は 3 つ: 手書きの C（AoS: 同じ 32 バイトの構造体の配列を素直なループで読み書きする。あふれの検査なし・`-O2` の自動ベクトル化まかせ）、改修前の Fascon（フェーズ 4 の写しの列）、改修後の Fascon（既定: `--columns on --nontemporal auto`）。機械: Intel Xeon（KVM、2 vCPU、L2 2 MiB / コア、L3 260 MiB）、GCC 13.3 と clang 18.1 の `-O2 -std=c11`、基本（SSE2）と `-march=native`（AVX-512）、Fascon の表と C の配列のどちらにも 2 MiB ページ（`madvise`）。2 つの大きさ × 4 つのビルド × 改修前 / 後の 16 本を交互に 9 回走らせ、数字は 9 回の中央値です（各回はハーネスの中央値: 6 万タスクは 20 回 × 15、400 万タスクは 3 回 × 9）。クロック数は、各回の最初に依存のある IMUL の鎖で測ったその回のクロック（2.87〜3.27 GHz、中央値 3.06 GHz）で換算しました。この共用のホストでは、C も含めてすべてが 1.6 倍ほど遅くなる回があります。

1 タスクあたりのクロック数。各欄は「C（AoS） / 改修前 / 改修後」、右端は改修後 ÷ C の範囲（4 つのビルド）です。

**6 万タスク（キャッシュに収まる）**

| 操作 | GCC 基本 | GCC native | clang 基本 | clang native | 改修後 ÷ C |
| --- | --- | --- | --- | --- | --- |
| 読み: vruntime の総和（vruntime_scan） | 1.62 / 0.41 / 0.40 | 1.56 / 0.18 / 0.18 | 1.54 / 0.35 / 0.34 | 1.60 / 0.23 / 0.24 | 0.11〜0.25 |
| 読み: 実行可能なタスクの vruntime の和 | 3.04 / 0.41 / 0.69 | 3.09 / 0.18 / 0.45 | 1.99 / 0.35 / 0.67 | 1.81 / 0.24 / 0.40 | 0.15〜0.34 |
| 読み: 実行可能なタスクの数 | 1.93 / 0.08 / 0.08 | 1.97 / 0.02 / 0.02 | 1.60 / 0.04 / 0.04 | 1.55 / 0.01 / 0.01 | 0.01〜0.04 |
| 読み: pick_next | 2.08 / 0.76 / 0.75 | 2.52 / 0.36 / 0.58 | 1.97 / 0.71 / 0.52 | 1.81 / 0.28 / 0.62 | 0.23〜0.36 |
| 書き: 実行可能なタスクの vruntime += 1 | 2.89 / 5.48 / 2.98 | 3.09 / 5.68 / 2.99 | 1.86 / 5.53 / 2.73 | 1.52 / 5.51 / 2.89 | 0.97〜1.90 |
| 書き: 全タスクの vruntime += 1 | 2.47 / 8.11 / 2.10 | 2.38 / 8.35 / 2.05 | 2.44 / 8.12 / 2.04 | 2.35 / 8.20 / 1.85 | 0.79〜0.86 |
| 書き: 全タスクの vruntime = x | 2.61 / 8.48 / 1.14 | 2.60 / 8.44 / 1.17 | 2.55 / 7.85 / 1.98 | 2.56 / 7.92 / 1.22 | 0.44〜0.78 |

**4,194,304 タスク（スロットの表 126 MB + 列 34 MB、C の配列 128 MB）**

| 操作 | GCC 基本 | GCC native | clang 基本 | clang native | 改修後 ÷ C |
| --- | --- | --- | --- | --- | --- |
| 読み: vruntime の総和（vruntime_scan） | 8.15 / 0.89 / 0.91 | 8.25 / 0.91 / 0.87 | 8.05 / 0.85 / 0.83 | 5.64 / 0.90 / 0.85 | 0.10〜0.15 |
| 読み: 実行可能なタスクの vruntime の和 | 9.91 / 0.90 / 0.97 | 10.06 / 0.89 / 0.85 | 8.63 / 0.84 / 0.99 | 7.67 / 0.88 / 0.86 | 0.08〜0.11 |
| 読み: 実行可能なタスクの数 | 10.49 / 0.10 / 0.10 | 10.45 / 0.02 / 0.02 | 8.58 / 0.04 / 0.04 | 5.83 / 0.01 / 0.01 | 0.00〜0.01 |
| 読み: pick_next | 10.10 / 1.08 / 1.03 | 10.27 / 0.87 / 0.86 | 8.95 / 1.03 / 0.93 | 8.28 / 0.88 / 0.84 | 0.08〜0.10 |
| 書き: 実行可能なタスクの vruntime += 1 | 9.94 / 16.61 / 11.00 | 9.96 / 17.04 / 11.01 | 8.93 / 15.57 / 10.92 | 8.53 / 15.39 / 10.81 | 1.11〜1.27 |
| 書き: 全タスクの vruntime += 1 | 8.79 / 20.98 / 2.12 | 9.39 / 19.68 / 2.16 | 9.00 / 19.12 / 1.74 | 9.06 / 18.74 / 1.61 | 0.18〜0.24 |
| 書き: 全タスクの vruntime = x | 13.30 / 19.60 / 1.36 | 12.96 / 19.10 / 1.42 | 13.33 / 18.99 / 1.38 | 13.61 / 19.70 / 1.38 | 0.10〜0.11 |

- **読み出しは、6 万タスクではどのビルドでも 1 タスク 1 クロック未満（0.01〜0.75）。** 400 万タスクでは 0.01〜1.03 クロックで、u64 の列はメモリ帯域の上限（このホストで 8 B / 0.27〜0.30 ns ≈ 27〜30 GB/s）で流れます。同じ読み出しを C（AoS）は 32 バイトの構造体を読むので 5.6〜10.5 クロックかかりました。
- **改修後のほうが改修前より遅くなった読み出しがあります。** 条件付き総和（GCC native で 0.18 → 0.45 クロック）と pick_next（clang native で 0.28 → 0.62）は、条件を走査の時点でマスクとして掛けるようになった分です。どれも 1 クロック未満です。素の総和と個数は変わりません。
- **一括更新（全タスクの `+= 1`）と一括上書き（`= x`）は、どの大きさ・ビルドでも C（AoS）より速くなりました**（6 万タスクで 0.44〜0.86 倍、400 万タスクで 0.10〜0.24 倍）。改修前は C の 3.1〜3.5 倍（6 万）と 1.4〜2.3 倍（400 万）でした。400 万タスクの上書きは非時間的ストアです（5-4 のとおり、このホストでは普通のストアのほうが 14 % 速い大きさですが、それでも C の 0.10 倍です）。
- **条件付きの書き込み（実行可能なタスクだけ `+= 1`）は、GCC では C と同等（6 万タスクで 0.97〜1.03 倍、400 万タスクで 1.11 倍）、clang では C より遅いままです**（6 万タスクで 1.47〜1.90 倍、400 万タスクで 1.22〜1.27 倍）。改修前の C 比 1.9〜3.6 倍（6 万）・1.7〜1.8 倍（400 万）からは縮みました。clang は C のループを 2〜6 段に展開し、C の時間が GCC の 0.5〜0.6 倍になります。Fascon の側は生存ビットマップをたどる一般のループ（ctz + blsr）で、1 タスクあたりの命令が C より多いのが残りの差です（5-9）。

### 5-8 確かめたこと

- 既存のテストはすべて通ります（`python3 tests/run_tests.py --qemu`: 338 件、失敗 0）。`tests/cols/cols.fsc` は `--columns on` / `mirrored` / `off` のどれでも `cols.expected` と同じで、写しの列の形の検査は `mirrored` の C で（この表は完全列化の条件を満たさないため、`on` でも `task` は写しの列）、完全列化の形の検査は `on` の C で行います。`tests/latch/harness.c` の 113 件の検査（古い Id の書き込みが番兵へ逸れ、生きているスロットが汚れないこと、など）は 3 つのモードのどれでも通ります。スロットの整列の検査だけは、完全列化の表では「スロット = 残った部分（自然な整列）」を確かめるように分けました。
- `tests/ok/tsoa_life.fsc`: alloc の初期値、Id での単純・複合の書き込み、get_mut の束縛を通した書き込み、要素まるごとの書き込みと `clone()`、free、`remove`（取り出した値が列の値を持つ）、総和・条件付き総和・個数・u64 の最小・i32 の最大・i32 の `<=` の選択、`#[on_drop]`（free・要素まるごとの書き込みでの古い値・満杯の表で確保に失敗した値）、ABA（解放したスロットが新しい世代で再利用されたあとの古い Id の書き込みと読み出しが ESTALE で捕まり、新しい要素が壊れない）。
- `tests/ok/tsoa_fallback.fsc`: `#[stable_abi]`（AoS に戻る）・`#[wire]`・`extern "C"` の境界を通る Id・外へ出る要素への参照・アドレスを取られるフィールド（そのフィールドだけ写しに残り、同じ表のほかのフィールドは完全列化）。
- `tests/ok/tsoa_nt.fsc`: 8 MiB の列への一括上書き（`auto` で非時間的ストア + `F20_SFENCE`）、次のループが同じ列を読む上書き（普通のストア）、割り込みハンドラと共有する表（`auto` では普通のストア、`on` では非時間的ストア + `F20_SFENCE`）、shared arena（完全列化しない）。
- この 3 本は `--columns on` / `mirrored` / `off` × `--nontemporal auto` / `on` / `off` の 9 通りすべてで同じ出力で、`--emit-strict-fascon` の往復（厳格形をもう一度コンパイルした C が元と同じ）も通ります。生成 C は GCC と clang の `-Wall -Wextra -Werror` で警告なしです。
- `tests/cols/colfuzz.py --tsoa`: 完全列化を壊す使い方を外したランダムなプログラム（アドレスを取るフィールド・場所の束縛・要素まるごとの読み書き・古い Id・一括上書きは残る）500 本（300 本は途中の版、最後の 200 本は最終版のコード）を、`on` / `on --nontemporal on` / `mirrored` / `off` で走らせ、出力はすべて一致しました（完全列化の表を持つものは 57 本）。`on` の生成 C は GCC と clang の `-Wall -Wextra -Werror` で警告なしでした（乱数の定数の比較が出す `-Wtype-limits` は改修前の C と同じなので除外）。`run_tests.py` は既定で 8 本ずつ走らせます。
- `tools/cross_check.py`（ARMv7・MIPS64・RISC-V64・AArch64・x86_64 向けに `-Werror` で翻訳しリンク）は、`tests/ok/tsoa_life.fsc`・`tests/ok/tsoa_fallback.fsc`・`bench/sched_bench.fsc` を含めて通ります。

### 5-9 まだやっていないこと・限界

- **条件付きの書き込みと clang**（5-7）。一般の `for id in @a` は生存ビットマップを ctz でたどります。密な表で 64 スロットの語がすべて 1 のとき単純な添字のループにすれば、clang native の 6 万タスクで 1.90 倍 → 1.23 倍（C 比、1 回の計測）になることを手で書き換えた C で確かめましたが、まばらな表では遅くなるので入れていません。
- **完全列化の条件は保守的です。** 要素への参照を関数へ渡す・要素を束縛する match がある・要素の型や Id が `extern "C"` を通る表は、写しの列のままです。32 ビットより狭い整数と usize / isize のフィールドも写しのままです。
- **非時間的ストアの下限は 1 台の測定から決めました**（5-4）。最終段のキャッシュの大きさは C を生成する時点では分からないので、L3 の大きな機械では 8 MiB〜L3 の大きさの列で少し損をします。`--nontemporal=off` で止められます。
- **AArch64・RISC-V・MIPS では時間を測っていません。** 生成 C がこれらのターゲットで `-Werror` でコンパイル・リンクできること（`tools/cross_check.py`）までです。

## フェーズ 6 — 条件付き一括更新（Masked Column RMW）と浮動小数点の列

フェーズ 5 のあとも、C より遅い書き込みが 1 つ残っていました。`for id in @task { if @task[id].state == RUNNING { @task[id].vruntime += 1; } }` のような条件付きの更新です。条件はすでにビット列（`fb0`。スロットと同期）にあり、フィールドも完全列化の列なのに、一般のループが生存スロットを 1 つずつたどっていました。フェーズ 6 はこの形をビット列の直線の走査に変え（docs/SPEC-1.0-HPC.ja.md §2）、`#[fast_math]` の下で f32 / f64 にも整数と同じ列と走査関数を与えます（§1）。

### 6-1 走査関数

`runtime/fascon2_rt.h` の `f20_col_masked_add_{u32,u64,i32,i64,f32,f64}` と `f20_col_masked_set_*` は、GCC / clang のベクトル拡張で 1 度だけ書いてあり（`F20_VECTOR_BYTES` はターゲットにより 16 / 32 / 64 バイト）、SSE2・AVX2・AVX-512・NEON が同じコードになります。

```c
for (uint32_t base = 0u; base < n; base += 64u) {
    const uint64_t w = m[base >> 6];
    if (w == 0u) continue;                                /* その 64 スロットは読みも書きもしない */
    T *const q = c + base;
    UVT of = {0};
    if (w == ~UINT64_C(0)) {                              /* 全部選ばれている */
        if (nt) { /* 先に全レーンのあふれを確かめ、F20_NT_STORE で流し込む */ }
        for (j = 0; j < 64; j += L) { v = load(q + j); r = v + xv; of |= OVF(v, xv, r); store(q + j, r); }
    } else {                                              /* ばらばら: 述語付きでその場で。非時間的ストアは使わない */
        for (j = 0; j < 64; j += L) { v = load(q + j); xm = xv & MASK(w, j, sel); r = v + xm; of |= OVF(v, xm, r); store(q + j, r); }
    }
    if (FASCON_UNLIKELY(top_bit_set(of))) { /* 同じ xm を引き直す: 語は元どおり */ return base; }
}
return F20_RMW_NONE;
```

- `MASK(w, j, sel)` は `(uint32_t)(w >> j)` を全レーンに配り、`1 << レーン番号` と AND して 0 と比べます。SSE2 には 64 ビットの比較がないので、64 ビットのレーンも 32 ビットの比較で作ります（レーンの 2 つの半分が同じビットを選ぶ）。最初の版は 64 ビットの比較を使い、SSE2 で 1 タスク 2.1〜2.4 ns（6 万タスク、試しの 1 回）かかっていました。今は 0.64〜0.66 ns（下の中央値）です。
- あふれは比較を使わずに求めます（SSE2 には符号なしの比較も 64 ビットの比較もない）。符号なしは繰り上がり `(v & x) | ((v | x) & ~r)`、符号付きは `(v ^ r) & (x ^ r)` で、どちらも最上位ビットに出ます。マスクで外したレーンは 0 を足すのであふれません。
- 書き込みは先に行います。あふれる語では同じ `xm` を引き直し（剰余の算術なので元の値に正確に戻る）、その語の先頭の添字を返します。生成 C はそこから逐次のループで続け、`--columns off` と同じスロットで同じ出力のまま panic します。最初の版は語ごとに 2 回目の走査で確かめてから書いていました。下の数字は 1 回の走査 + 元に戻す版のものです。
- 浮動小数点の加算と代入はビットの混ぜ合わせで選ぶので、選ばれないレーンはビットをそのまま保ちます（`-0.0` も NaN の中身も）。

### 6-2 測った結果

フェーズ 5 と同じ種類のホスト（Intel Xeon @ 2.10 GHz、KVM、2 vCPU、コアごとに 2 MiB の L2、260 MiB の L3）、GCC 13.3 と clang 18.1 の `-O2`、SSE2 と `-march=native`（AVX-512）、交互に 9 回の中央値、その回のクロック（2.66〜3.85 GHz）で換算した 1 タスクあたりのクロック数です。全部の表は [bench/RESULTS.md](../bench/RESULTS.md) にあります。

**実行可能なタスクだけの書き込み `vruntime += 1`**（4 分の 1 ほどのタスクなので、ほぼすべての語がばらばら）。各欄は「C（AoS）/ フェーズ 1 / 今」:

| タスク | GCC 基本 | GCC native | clang 基本 | clang native | 今 ÷ C |
| --- | --- | --- | --- | --- | --- |
| 60,000 | 2.72 / 3.02 / 2.22 | 2.97 / 2.90 / 0.70 | 2.10 / 2.84 / 2.20 | 1.58 / 3.03 / 0.68 | 0.24〜1.05 |
| 4,194,304 | 3.98 / 4.02 / 2.27 | 3.98 / 3.70 / 0.95 | 3.89 / 3.81 / 2.16 | 3.85 / 3.92 / 0.96 | 0.24〜0.57 |

**浮動小数点（`bench/fsimd_bench.fsc.in`、32 バイトの粒子）**、1 粒子あたりのクロック数、clang native の「-O2 の C / -ffast-math の C / Fascon」（ほかのビルドは bench/RESULTS.md）:

| 操作 | 6 万粒子 | 4,194,304 粒子 |
| --- | --- | --- |
| 読み出し: mass（f64）の総和 | 2.09 / 1.62 / 0.20 | 4.01 / 3.77 / 0.89 |
| 読み出し: x（f32）の総和 | 2.08 / 1.41 / 0.11 | 3.91 / 3.69 / 0.44 |
| 読み出し: HOT の energy の和（f64、条件付き） | 1.53 / 1.86 / 0.35 | 3.77 / 3.85 / 0.89 |
| 書き込み: HOT の energy += 0.5（f64 の条件付き加算） | 1.59 / 1.54 / 0.53 | 4.00 / 4.04 / 0.98 |
| 書き込み: HOT の x += 1.0（f32 の条件付き加算） | 1.58 / 1.55 / 0.26 | 4.32 / 3.96 / 0.51 |
| 書き込み: COLD の energy = e（f64 の条件付き代入） | 1.55 / 1.57 / 0.51 | 4.16 / 4.22 / 0.98 |
| 書き込み: 全粒子の mass = m（f64） | 2.63 / 2.58 / 2.03 | 4.94 / 6.81 / 1.32 |

- **条件付きの書き込みは C の 0.24〜1.05 倍**（6 万タスク）**・0.24〜0.57 倍**（400 万タスク）になりました。同じ実行のフェーズ 1 は 0.97〜1.91 倍・0.93〜1.02 倍です。AVX-512 ではばらばらの語 1 つが 8 レーンのベクトル演算 8 回、SSE2 では 2 レーンの 32 回で、clang の基本が C と同じくらい（1.05 倍）なのはそのためです。
- **浮動小数点の操作は、どのビルドでも 2 つの C より速くなりました**（読み出しは素直な C の 0.04〜0.55 倍、条件付きの書き込みは 0.02〜0.82 倍、一括上書きは 0.15〜0.78 倍）。素直な `-O2` の C は浮動小数点の和を書いた順に保つのでベクトル化できず、`-ffast-math` の C はできても、AoS のループは 32 バイトごとに 4 か 8 バイトを読みます。
- タスクのベンチマークのほかの行（読み出し・条件なしの書き込み）は、この共用のホストの揺れを超えては変わっていません。`tools/compare_c.py` でその C が同じことも確かめました。

### 6-3 どう確かめたか

- `tests/cols/rmw_kernels.c`: すべての走査関数を、0・全部 1・ばらばらの語、端数、`-0.0` と NaN の値、あふれる語を含む乱数の列でスカラの模型と比べる。-O2（GCC、clang、SSE2 / AVX2 / AVX-512）と ASan / UBSan で。
- `tests/ok/tsoa_rmw.fsc`（u32 / u64 / i32 / i64 / f32 / f64 の加算と代入）と `tests/ok/tsoa_float*.fsc` は、`--columns` × `--nontemporal` の 9 つの組み合わせで同じ出力。`tests/cols/rmw_overflow.fsc` は `--columns on` と `off` で同じスロット・同じ出力で panic する。
- `tests/cols/colfuzz.py --rmw` と `--float`: 6 種類の列への条件付きの `+=` / `=` を確保・解放と混ぜたランダムなプログラムと、NaN・`-0.0`・無限大を含む浮動小数点の列のプログラムを、`on`・`on --nontemporal on`・`mirrored`・`off` で走らせる（最後の実行でそれぞれ 32 本、すべて一致）。
- `tools/compare_c.py`: 既存のすべてのプログラムの C は、列の 3 つのモードのどれでも変わらない。

### 6-4 まだやっていないこと・限界

- **SSE2 の 64 ビットの列では、ばらばらの語が 64 スロットあたり 32 回のベクトル演算**になります。まばらで散らばった条件（ほとんどの語に立っているビットが少し）は、ビットごとのループのほうが速く走りますが、語あたりの手間を一定にし、データに依存する分岐を入れないために使っていません。
- **ループの中で変わらない値だけ**（`x` がリテラル・ローカル・static・定数）を扱います。`@a[id].f += @a[id].g` は一般のループのままです。
- **AArch64 / RISC-V の時間は測っていません**。走査関数はベクトル拡張なのでそちらでもコンパイルできますが、測ったのは x86-64 だけです。

## 測った結果

[bench/RESULTS.md](../bench/RESULTS.md) に全部の行があります。Intel Xeon @ 2.10 GHz（KVM、2 vCPU、Linux 6.18、THP は `madvise`）、GCC 13.3 と clang 18.1 の `-O2`、6 万タスク、1 タスクあたりの ns です。交互に 9 回走らせる回を 2 回行い、合わせた 18 回の中央値を載せています（各回は 15 回の計測の中央値）。「4 KiB」は新しいコードを `FASCON_NO_HUGEPAGE=1` で、「2 MiB」は `madvise(MADV_HUGEPAGE)` で走らせたものです。2 MiB のときは `/proc/<pid>/smaps` で表に `AnonHugePages: 2048 kB` が付いていることを確かめました。1.0.0 はいつも 4 KiB ページです。

| 走査 | 1.0.0 gcc | いま gcc 4 KiB | いま gcc 2 MiB | 1.0.0 clang | いま clang 4 KiB | いま clang 2 MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 素の総和（`for id in @task` の総和の特殊化） | 0.74 | 0.72 | 0.63 | 0.71 | 0.69 | 0.57 |
| 実行可能なタスクの数（条件付きの個数） | 1.10 | 0.79 | 0.71 | 1.10 | 0.80 | 0.70 |
| 実行可能なタスクの vruntime の和（条件付き総和） | 1.08 | 1.09 | 1.04 | 1.12 | 0.74 | 0.66 |
| pick_next（実行可能で vruntime 最小） | 1.31 | 0.82 | 0.73 | 1.23 | 0.73 | 0.69 |
| 一般のループ（条件付きの書き込み） | 1.12 | 1.19 | 1.15 | 1.12 | 1.12 | 1.10 |
| まばらな表（64 スロットの塊の 9 割を解放）の個数 | 0.49 | 0.07 | 0.07 | 0.48 | 0.07 | 0.07 |
| まばらな表の pick_next | 0.48 | 0.07 | 0.07 | 0.51 | 0.07 | 0.07 |
| `get` による Id の連鎖・ばらばらの順 | 17.43 | 16.95 | 14.23 | 22.91 | 23.21 | 15.93 |
| `@task[id]` による Id の連鎖・ばらばらの順 | 18.23 | 18.04 | 13.67 | 18.78 | 18.48 | 14.43 |

- **コードの変更だけの効果（4 KiB の列）:** 条件付きの個数は 27〜28 %、pick_next は 37 %（GCC）/ 41 %（clang）速くなりました。条件付き総和は clang で 34 % 速く、GCC では変わりません。まばらな表では、全部 0 の語を飛ばすので 7 倍です。
- **大きなページの上乗せ（2 MiB と 4 KiB の比較）:** 素の総和が 13 %（GCC）/ 17 %（clang）、条件付きの走査が 5〜13 %、ばらばらの順の Id の連鎖が 16〜31 % 速くなりました。
- **素の総和**は 0.74 → 0.63 ns（GCC）、0.71 → 0.57 ns（clang）です。2.10 GHz で 1 タスク 1.32 / 1.20 クロックにあたり、課題の目標（0.48〜0.70 ns）の範囲に入りました。フェーズ 1〜3 では 1 クロック未満（このクロックで 0.476 ns）に届きませんでした。走査にはもう分岐がなく、1 タスク 32 バイトを読む帯域（50〜56 GB/s、1 クロック 24〜27 バイト）が残りの遅さの元でした。それをなくすのが下のフェーズ 4 です。
- **一般のループ**（条件付きの書き込み・全スロットが生存）は、clang では変わらず、GCC では 3〜6 % 遅くなりました。密な表では 1.0.0 のスロットごとの `link[]` の判定がいつも予測どおりだった一方、ビットマップではスロットごとに ctz と blsr が加わるためです。ビットマップが効くのはまばらな表です。本体に書いた `if` はソースのロジックそのものなので、分岐のまま残ります（スロットを無条件に書くと、マルチコアでは余計なラインの奪い合いになるためです）。
- **大きなページは環境で変わります。** 同じ日のもっと早い回では、4 KiB と 2 MiB の列が同じになりました。そのときは表に大きなページが付いていなかったと考えられます。
- `FASCON2_PICK_BRANCHLESS` では pick_next が 1.5 ns（clang）/ 2.6 ns（GCC）です（1-2 を参照）。
- 作成順の連鎖は、1.0.0 と誤差の範囲で同じです。
- AArch64 は実機で時間を測っていません（QEMU の時間は意味がありません）。命令の数は上のアセンブリのとおりです。

### フェーズ 4（列）

同じハーネスで、別のホストの後日の回: Intel Xeon（KVM、2 vCPU）、GCC 13.3 と clang 18.1 の `-O2`、2 MiB ページ、6 万タスク。同じ回に依存のある 64 ビット乗算の鎖で測ったコアのクロックは 3.85 GHz（3.80〜3.94）で、1 クロックは約 0.26 ns です。8 つのビルド（`--columns off` / `on` × GCC / clang × 基本（SSE2）/ `-march=native`（このホストでは AVX-512））を交互に 9 回走らせました。数字は 9 回の中央値で、1 タスクあたりの ns です。「off」はフェーズ 3 までのすべてを含みます。

| 走査 | off GCC | on GCC | off clang | on clang | off GCC native | on GCC native | off clang native | on clang native |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 素の総和（vruntime + prio） | 0.54 | 0.21 | 0.45 | 0.19 | 0.51 | 0.10 | 0.45 | 0.11 |
| 実行可能なタスクの数（条件付きの個数） | 0.56 | 0.02 | 0.56 | 0.01 | 0.60 | 0.00 | 0.28 | 0.00 |
| 実行可能なタスクの vruntime の和（条件付き総和） | 0.79 | 0.10 | 0.52 | 0.09 | 0.82 | 0.05 | 0.48 | 0.06 |
| pick_next（実行可能で vruntime 最小） | 0.60 | 0.21 | 0.63 | 0.18 | 0.61 | 0.10 | 0.49 | 0.07 |
| まばらな表: 総和 | 0.56 | 0.03 | 0.45 | 0.02 | 0.51 | 0.02 | 0.43 | 0.02 |
| まばらな表: 個数 | 0.05 | 0.02 | 0.05 | 0.01 | 0.06 | 0.00 | 0.03 | 0.00 |
| まばらな表: pick_next | 0.06 | 0.03 | 0.07 | 0.02 | 0.06 | 0.02 | 0.05 | 0.01 |
| `files` を書く一般のループ（どの列も読まない） | 0.87 | 0.89 | 0.84 | 0.77 | 0.94 | 0.90 | 0.76 | 0.75 |
| `vruntime` を書く一般のループ（3 本の列が読む） | 0.94 | 2.13 | 0.81 | 1.79 | 0.93 | 1.78 | 0.78 | 1.82 |

- **どのビルドでも、32 バイトのタスク 1 つあたり 1 クロック未満。** 列ありで、3.85 GHz で: 素の総和 0.73〜0.81 クロック（基本）/ 0.39〜0.42（native）、条件付き総和 0.35〜0.39 / 0.19〜0.23、pick_next 0.69〜0.81 / 0.27〜0.39、条件付きの個数 0.1 未満。列なしでは同じ走査が、条件付きの個数を含めると 1.1〜3.2 クロック（0.28〜0.82 ns。[bench/RESULTS.md](../bench/RESULTS.md) と同じ基準）、素の総和・条件付き総和・pick_next だけなら 1.7〜3.2 クロック（0.45〜0.82 ns）かかります。素の総和が読むのは 1 タスク 12 バイト（vruntime 8 + prio 4）で、32 バイトではありません。
- **書き込み:** どの列も読まないフィールドを書くループは変わりません。実行可能なタスクすべての `vruntime` を書くループは約 2 倍の時間になります（4-4）。
- **キャッシュより大きな表。** 6 万タスクでは、1 回の走査が読む列（最大 0.8 MB）が 2 MiB の L2 に収まります。4,194,304 タスク（スロットだけで 128 MB、`-march=native`、GCC、9 回の中央値）では、1.0.0 型の走査が 1 タスク 1.9〜2.9 ns、列の走査が 0.29 ns（総和・条件付き総和・pick_next）と 0.004 ns（個数）でした。約 7 倍速くなりますが、このとき u64 の列はメモリの帯域（このホストで約 28 GB/s）で流れ、1 タスク 1.1 クロックです。これ以上は、1 タスクあたりのバイト数を減らす（値を狭くする）しかありません。
- **AArch64** は実機で時間を測っていません。コンパイルしたループ（GCC、`-mcpu=cortex-a72`）から: u64 の総和は 1 スロットに SIMD 演算 1 つ（ADD + ORR）、u32 の総和は 0.75（AND・USRA・ADD）、選択は 1（CMGT + BSL）、個数は 64 スロットに数命令（CNT + ADDV）です。128 ビットの SIMD の実行ユニットが 2 つのコアで 1 スロットおよそ 0.5 / 0.38 / 0.5 / 0.03 クロック、4 つあればその半分です。
- 列に関係しない行（Id の連鎖、ふつうの C のポインタのリスト）は `bench/RESULTS.md` にあります。この回は共用のホストで回ごとに最大 ±50 % 揺れました（両方のビルドで同じように）。

## まだやっていないこと・限界

- **写しの列（Mirrored-SoA）は、書き込みの時間とメモリを走査の時間と引き換えにします。** 写しに届く書き込みはストア数個と、表全体でそのフィールドを書くループでは余分なキャッシュラインを払います。完全列化できるフィールドではこれがなくなりました（フェーズ 5）。完全列化の条件と残る限界は 5-1 と 5-9 にあります。外のローカル変数を読む条件と、64 ビットの `<=` / `>=` の選択はフェーズ 1 の走査のままです。`--columns off` / `mirrored` はプログラム全体に効きます。アリーナごとの切り替えはまだありません。
- **キャッシュに収まらない表では、列はメモリの帯域で流れます**（400 万タスクで u64 の列が 1 タスク約 0.85〜1.0 クロック。5-7）。次の手は、1 タスクあたりの値を狭くすることです。
- **マルチコア。** 偽共有の起きない配置と単位（512 スロット）を決めました。マルチコアのプログラムは [SPEC-1.0-SMP.ja.md](SPEC-1.0-SMP.ja.md) と [SPEC-1.0-INTEGRATION.ja.md](SPEC-1.0-INTEGRATION.ja.md) の範囲です。この文書の計測は 1 コアで走らせたもので（`bench/harness.c` はスレッドを作りません）、コア間の偽共有はここでは測っていません。
- **コミット点の場所。** 回数の決まったループの中で古い Id を読んだとき、止まるのはループの出口です（それまで外へ影響の出る操作はすべて止められるので、メモリが壊れないことと、外から見えないことは同じです）。回数の多いループでは、止まるまでの時間が 1.0.0 より長くなります。
- **数字は 1 台の仮想マシンのものです。** 時間帯で揺れます。Linux が大きなページを割り当てるかどうかも、回によって変わることがあります。

## ソースのどこを変えたか

| 場所 | 変更 |
| --- | --- |
| `src/codegen.rs` | `loop_stmt`（ビットマップ + ctz、出口のコミット点、while / loop へのラッチの混ぜ込み）、`body_changes_liveness`、`reduction_plan` ほかマスク付き集計、`gen_arena`（置き場所・`live[]`・アリーナ表）、`emit_struct`（整列）、`stmt` / `emit_scope_drops` / `own_expr`（フレームリージョン） |
| `src/parser.rs` · `src/sema.rs` · `src/types.rs` · `src/ast.rs` | `#[repr(align(N))]`、F2031・F2032、`StructInfo::align`、`type_align_attr` |
| `src/flow.rs` | アリーナ効果に `allocs` を追加（推移閉包） |
| `src/driver.rs` · `src/dict.rs` · `src/qemu.rs` | 配置の計算・`fascon regions` の新しい行・辞書の項目・BSP のビルドの既定値 |
| `runtime/fascon2_rt.h` | `live[]`・ビット走査・`F20_SEL64` / `F20_PICK`・新しい表の配置・`F20_ARENA_STORAGE`・アリーナ表・フレームリージョン |
| `bsp/qemu-virt-aarch64/` | 1 GiB / 2 MiB + 連続ビット、`.fascon_arena`・`.frame`、起動時の表示 |
| `bsp/host/fascon2_plat_host.c` | `madvise(MADV_HUGEPAGE)`（`FASCON_NO_HUGEPAGE=1` で止める） |
| `vscode-extension/` | 辞書の項目（生存ビットマップ・`#[repr(align(N))]`・フレームリージョン・コミット点・F2031・F2032）、スニペット `repralign` / `forpick` / `forcount`、`#[repr(align(N))]` の色付け |
| `tests/` | `ok/frame.fsc`・`ok/align.fsc`・`err/f2031`・`err/f2032`、ラッチのハーネスに 10 項目（乱数 3 万歩の照合を含む）、QEMU の起動 2 件。全部で 38 件 |
| `bench/` | 条件付きの走査・まばらな表の行。ハーネスも `fascon run` と同じく大きなページを頼む（4 KiB の列は `FASCON_NO_HUGEPAGE=1`） |
| `src/codegen.rs`（フェーズ 4） | `collect_columns` / `plan_columns` / `add_col`（すべての集計から列を決め、種類・値・条件が同じなら共有）、`gen_arena`（列の構造体・`F20_DEFINE_TABLE_COLS`・`_mirror` / `_mirror_p` / `_unmirror`・列を保つ alloc / free / remove）、`gen_mirror_masks`（一部だけの作り直し）、`emit_col_reduction`、`arena_ptr` / `assign` / `call`（書き込みの要素ポインタを取り、届く列を作り直す）、`lower_match` / `emit_scope_drops` / `ref_writes`（get_mut の借用と、スロットの中の場所を束縛した match / if let）、`arena_write_root` / `place_root_path` / `col_mask`（書き込みが届く列） |
| `runtime/fascon2_rt.h`（フェーズ 4） | `F20_COL_*`・`F20_KEY_*`・`F20_BIT_PUT`・本来の幅のベクトル型・`F20_KEEP`・`f20_col_sum_u64` / `_exact`・`f20_col_sum_u32`・`f20_col_count_bits`・`f20_col_pick_key`・`f20_col_key_fresh`・`F20_DEFINE_TABLE_COLS` |
| `src/main.rs` · `src/driver.rs` · `src/qemu.rs` · `src/dict.rs`（フェーズ 4） | `--columns on\|off` と `FASCON_COLUMNS`、`--cpu native\|baseline`（`fascon run` は `-march=native` / `-mcpu=native` を付け、だめならなしでやり直す）、`fascon regions` の列の行、辞書の項目「列」 |
| `vscode-extension/`（フェーズ 4） | 設定 `fascon.columns` と `fascon.cpu`、辞書の項目 |
| `tests/`（フェーズ 4） | `cols/cols.fsc`（と `.expected`。QEMU でも）、`cols/colfuzz.py`（`--fuzz N`）、写しが頼る借用の規則の F2502 の 4 件。全部で 52 件（場所の束縛の修正で足した `ok/match_place.fsc` と F0507 の 1 件を含む） |
| `bench/`（フェーズ 4） | `charge_running`（列が読むフィールドを書くループ） |
| `src/codegen_tsoa.rs`（フェーズ 5、新規） | `plan_tsoa`（完全列化の判定: 表・要素・extern "C"・#[on_drop]・参照の流出・アドレス・条件・1.0.0 の走査・書き込みの重み）、`ts_lv` / `arena_ix`（列の左辺値と、証明済み・ラッチ・番兵・その場で止める添字）、`gen_tsoa_part` / `gen_tsoa_fns` / `gen_tsoa_cols`（スロットに残る部分・`_ix` / `_ix_r` / `_ix_p` / `_ix_strict`・`_load` / `_put` / `_ts_zero`・列）、`ts_red_body`（マスク付きの走査）、`nt_fields` / `nt_decide` / `nt_scan_block`（一括上書きと非時間的ストアの判定） |
| `src/codegen.rs`（フェーズ 5） | `CgOpts::true_soa` / `nontemporal`、`ColUse::SumT` / `PickT`、`plan_columns`（完全列化の項・書き込みの多いフィールドの除外）、`gen_arena`（スロットに残る部分の型・自然な整列・`_put` / `_load` を使う alloc / free / remove、確保に失敗した値の解放）、`lv_raw`（完全列化のフィールドと要素まるごとの読み出し）、`lower_match`（ルックアップの束縛の型）、`assign`（要素まるごとの書き込み・非時間的ストア）、`arena_ptr`（写しのための要素ポインタの型・ループのスロットの添字）、`loop_stmt`（`F20_SFENCE`）、`generate`（`--nontemporal=on` の `F20_PREFETCH_DIST`） |
| `runtime/fascon2_rt.h`（フェーズ 5） | `f20_col_sum_u64_m` / `_exact`・`f20_col_sum_u32_m`・`f20_col_pick64_m`・`f20_col_pick32_m`・`F20_LANE_BITS` / `F20_LANE_MASK`・x86 の SSE2 / SSE4.2 のマスクの表・`F20_NT_STORE_*`・`F20_SFENCE`・`F20_PREFETCH_R`・`F20_PF_BLOCK`（`F20_PREFETCH_DIST` を定義したときだけ） |
| `src/main.rs` · `src/driver.rs`（フェーズ 5） | `--columns on\|mirrored\|off`（`FASCON_COLUMNS=mirrored`）、`--nontemporal auto\|on\|off`（`FASCON_NONTEMPORAL`）、`fascon regions` の True-SoA (single-write) / Mirrored-SoA (dual-write)・完全列化しなかった理由・一括上書きの判定・完全列化の表のスロットの大きさ |
| `tests/`（フェーズ 5） | `ok/tsoa_life.fsc`・`ok/tsoa_fallback.fsc`・`ok/tsoa_nt.fsc`（と `.expected`）、`cols/colfuzz.py --tsoa`、`run_tests.py`（`--columns` 3 通り × `--nontemporal` 3 通り、C の形、`fascon regions`、ラッチのハーネスを 3 つのモードで）、`latch/harness.c`（完全列化の表のスロットの整列の検査） |
| `bench/`（フェーズ 5） | `tsoa_bench.fsc.in`・`tsoa_harness.c`（手書きの C の AoS との比較）・`tsoa_bench.sh`・`tsoa_summary.py`、`harness.c`（完全列化の表の参照の値を `ARENA_task_TS_vruntime` で読む） |
| `src/lexer.rs` · `src/parser.rs` · `src/sema.rs` · `src/types.rs` · `src/strict.rs`（フェーズ 6） | `f32` / `f64`（リテラル・推論・変換・`to_bits` / `from_bits` / `is_nan` / `is_finite` / `abs`・定数）、ループと関数の `#[fast_math]`、`--fast-math` |
| `src/codegen.rs` · `src/codegen_tsoa.rs`（フェーズ 6） | 浮動小数点の完全列化の列と `ColKind::FKey`（IEEE-754 の順序の鍵）、`ts_fsum_body`、`rmw_plan` / `plan_rmw` / `emit_rmw`（条件付き一括更新と、あふれたときの逐次のループへの切り替え）、`fascon regions` 用の `CgReport::rmw_loops` |
| `runtime/fascon2_rt.h`（フェーズ 6） | `FP_CONTRACT` を止める・浮動小数点の補助と最短の往復できる出力・`F20_KEY_F32` / `F20_KEY_F64`・`f20_col_sum_f32` / `_f64`（`_m`）・`F20_NT_STORE_F32` / `_F64`・`f20_col_masked_add_*` / `f20_col_masked_set_*` |
| `tests/` · `bench/`（フェーズ 6） | `ok/float_basic.fsc`・`ok/tsoa_float*.fsc`・`ok/tsoa_rmw.fsc`・`cols/rmw_kernels.c`・`cols/rmw_overflow.fsc`・`float/fmtfuzz.c` + `fmtcheck.py`・`cols/colfuzz.py --float` / `--rmw`、`fsimd_bench.fsc.in`・`fsimd_harness.c`・`fsimd_c.c` / `.h`・`fsimd_bench.sh`・`tsoa_summary.py --float` |
