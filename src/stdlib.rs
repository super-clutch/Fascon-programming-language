//! コンパイラに埋め込んだ標準ライブラリ（純粋な Fascon。`import sync::spinlock;` などで取り込む）
//! the standard library embedded in the compiler (pure Fascon; imported with `import sync::spinlock;` etc.)
//!
//! 同じ名前のファイルが検索パス（このファイルのフォルダ・fsc_modules/・FASCON_PATH・実行ファイルの横の lib/）にあれば、
//! そちらが優先する。追加はこの表に 1 行足すだけ（コンパイラの他の部分は変えない）
//! a file of the same name on the search path (this file's folder, fsc_modules/, FASCON_PATH, lib/ next to the executable)
//! takes precedence. Adding a module is one more line in this table (no other part of the compiler changes)

pub const FILES: &[(&str, &str)] = &[
    ("sync/spinlock", include_str!("../lib/sync/spinlock.fsc")),
    ("sync/mutex", include_str!("../lib/sync/mutex.fsc")),
    ("collections/vec", include_str!("../lib/collections/vec.fsc")),
    ("time/wheel", include_str!("../lib/time/wheel.fsc")),
    ("sched/hmp", include_str!("../lib/sched/hmp.fsc")),
    ("net/wire", include_str!("../lib/net/wire.fsc")),
    ("net/udp", include_str!("../lib/net/udp.fsc")),
    ("net/tcp", include_str!("../lib/net/tcp.fsc")),
    ("fs/pagecache", include_str!("../lib/fs/pagecache.fsc")),
    ("fs/cowfs", include_str!("../lib/fs/cowfs.fsc")),
    ("ipc/rpc", include_str!("../lib/ipc/rpc.fsc")),
    ("loader/elf", include_str!("../lib/loader/elf.fsc")),
    ("pm/power", include_str!("../lib/pm/power.fsc")),
    ("hal/console", include_str!("../lib/hal/console.fsc")),
    ("hal/timer", include_str!("../lib/hal/timer.fsc")),
    ("hal/block", include_str!("../lib/hal/block.fsc")),
    ("hal/net", include_str!("../lib/hal/net.fsc")),
];
