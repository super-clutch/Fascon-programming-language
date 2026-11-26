//! `fascon stack`: `--target rtos` のスタックの最大消費量の静的な証明
//! `fascon stack`: a static proof of the maximum stack use for `--target rtos`
//!
//!   1. Fascon の呼び出しグラフ（関数ポインタの候補を含む）に閉路が無いこと（再帰禁止: F3001）
//!   2. 生成 C を GCC の `-fcallgraph-info=su,da` でコンパイルし、関数ごとの静的なフレームの大きさと、実際に残った呼び出しの
//!      辺（インライン化の後）を読む。動的な確保（alloca・VLA）が 1 つでもあれば証明しない
//!   3. 間接呼び出し（__indirect_call）は、値として使われた関数（コールバックの候補）すべてへの辺に置き換える
//!   4. 根（main と割り込みハンドラ）ごとに最長の経路 S(f) = frame(f) + max S(callee) を求め、
//!      最悪 = S(main) + max S(irq)（割り込みはいまのスタックの上に積まれる）を上限と比べる
//!   1. the Fascon call graph (function-pointer candidates included) has no cycle (no recursion: F3001)
//!   2. the generated C is compiled with GCC `-fcallgraph-info=su,da`, which gives each function's static frame size and the
//!      calls that really remain (after inlining); any dynamic allocation (alloca, VLA) stops the proof
//!   3. an indirect call (__indirect_call) becomes edges to every function used as a value (the callback candidates)
//!   4. per root (main and interrupt handlers) the longest path S(f) = frame(f) + max S(callee) is computed, and the worst
//!      case S(main) + max S(irq) (interrupts stack on top of the current stack) is compared with the limit
use crate::diag::Lang;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct Node {
    frame: u64,
    dynamic: bool,
    defined: bool,
}

/// .ci（VCG）を読む / read a .ci (VCG) file
fn parse_ci(text: &str, nodes: &mut HashMap<String, Node>, edges: &mut Vec<(String, String)>) {
    for line in text.lines() {
        let l = line.trim();
        let field = |key: &str| -> Option<String> {
            let k = format!("{}: \"", key);
            let i = l.find(&k)? + k.len();
            let j = l[i..].find('"')? + i;
            let v = &l[i..j];
            // static な関数は「ファイル:名前」になる（名前に `:` は無い） / static functions are titled "file:name"
            Some(if key == "label" { v.to_string() } else { v.rsplit(':').next().unwrap_or(v).to_string() })
        };
        if l.starts_with("node:") {
            let Some(t) = field("title") else { continue };
            let label = field("label").unwrap_or_default();
            let n = nodes.entry(t).or_default();
            // "112 bytes (static)" / "48 bytes (dynamic,bounded)" / "dynamic"
            for part in label.split("\\n") {
                if let Some(b) = part.strip_suffix(")").and_then(|p| p.split_once(" bytes (")) {
                    n.frame = n.frame.max(b.0.trim().parse::<u64>().unwrap_or(0));
                    n.defined = true;
                    if b.1.contains("dynamic") {
                        n.dynamic = true;
                    }
                }
                if part.trim_end().ends_with("dynamic objects") && !part.trim_start().starts_with('0') {
                    n.dynamic = true;
                }
            }
        } else if l.starts_with("edge:") {
            if let (Some(a), Some(b)) = (field("sourcename"), field("targetname")) {
                edges.push((a, b));
            }
        }
    }
}

pub struct Report {
    pub text: String,
    pub worst: u64,
    pub ok: bool,
}

/// 生成 C のファイルを解析して報告を作る / analyse a generated C file and build the report
pub fn analyse(c_file: &std::path::Path, cc: &str, extra: &[String], callbacks: &[String], irqs: &[String], ext_budget: u64, limit: u64, lang: Lang) -> Result<Report, String> {
    let dir = c_file.parent().unwrap_or(std::path::Path::new("."));
    let obj = dir.join("fascon_stack.o");
    let mut cmd = std::process::Command::new(cc);
    cmd.args(["-O2", "-std=c11", "-fno-stack-protector", "-fcallgraph-info=su,da", "-c"]).args(extra).arg(format!("-I{}", dir.display())).arg(c_file).arg("-o").arg(&obj);
    let out = cmd.output().map_err(|e| format!("{}: {}", cc, e))?;
    if !out.status.success() {
        return Err(format!("{} failed:\n{}", cc, String::from_utf8_lossy(&out.stderr)));
    }
    // GCC は <出力の名前>.ci を作る（-o の拡張子を ci に） / GCC writes <output>.ci
    let ci = obj.with_extension("ci");
    let text = std::fs::read_to_string(&ci).map_err(|e| format!("{}: {}", ci.display(), e))?;
    let mut nodes: HashMap<String, Node> = HashMap::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    parse_ci(&text, &mut nodes, &mut edges);
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for (a, b) in &edges {
        if b == "__indirect_call" {
            for cb in callbacks {
                if nodes.contains_key(cb) {
                    adj.entry(a.clone()).or_default().push(cb.clone());
                }
            }
        } else {
            adj.entry(a.clone()).or_default().push(b.clone());
        }
    }
    // 最長の経路（メモ化 DFS。閉路を見つけたら証明しない） / the longest path (memoised DFS; a cycle stops the proof)
    fn depth(f: &str, nodes: &HashMap<String, Node>, adj: &HashMap<String, Vec<String>>, ext: u64, memo: &mut HashMap<String, (u64, Vec<String>)>, on: &mut HashSet<String>, err: &mut Option<String>) -> (u64, Vec<String>) {
        if let Some(m) = memo.get(f) {
            return m.clone();
        }
        if !on.insert(f.to_string()) {
            *err = Some(format!("cycle through `{}`", f));
            return (0, vec![]);
        }
        let n = nodes.get(f);
        let own = match n {
            Some(x) if x.defined => x.frame,
            _ => ext, // この翻訳単位に無い関数（BSP・libc）は予算の大きさ / functions outside this unit get the budget
        };
        if n.map_or(false, |x| x.dynamic) {
            *err = Some(format!("`{}` allocates dynamically on the stack", f));
        }
        let mut best = (0u64, Vec::new());
        for c in adj.get(f).cloned().unwrap_or_default() {
            let d = depth(&c, nodes, adj, ext, memo, on, err);
            if d.0 > best.0 {
                best = d;
            }
        }
        on.remove(f);
        let mut path = vec![format!("{} ({} B)", f, own)];
        path.extend(best.1);
        let r = (own + best.0, path);
        memo.insert(f.to_string(), r.clone());
        r
    }
    let mut memo = HashMap::new();
    let mut err = None;
    let mut on = HashSet::new();
    let main = depth("fascon2_main", &nodes, &adj, ext_budget, &mut memo, &mut on, &mut err);
    let mut irq_max = (0u64, Vec::new(), String::new());
    for i in irqs {
        if nodes.contains_key(i) {
            let d = depth(i, &nodes, &adj, ext_budget, &mut memo, &mut on, &mut err);
            if d.0 > irq_max.0 {
                irq_max = (d.0, d.1, i.clone());
            }
        }
    }
    let worst = main.0 + irq_max.0;
    let mut t = String::new();
    t.push_str(&format!("{} {} ({}, -O2)\n", lang.pick("スタックの最大消費量の証明:", "proof of the maximum stack use:"), c_file.display(), cc));
    t.push_str(&format!("  main: {} B\n    {}\n", main.0, main.1.join(" → ")));
    if !irq_max.2.is_empty() {
        t.push_str(&format!("  {}: {} B ({})\n    {}\n", lang.pick("最も深い割り込みハンドラ", "deepest interrupt handler"), irq_max.0, irq_max.2, irq_max.1.join(" → ")));
    }
    let ext: Vec<&String> = nodes.iter().filter(|(k, n)| !n.defined && k.as_str() != "__indirect_call" && adj.values().any(|v| v.contains(k))).map(|(k, _)| k).collect();
    if !ext.is_empty() {
        let mut e: Vec<&String> = ext;
        e.sort();
        t.push_str(&format!("  {} {} B: {}\n", lang.pick("翻訳単位の外の関数（予算）", "functions outside the unit (budget)"), ext_budget, e.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    t.push_str(&format!("  {}: {} B / {}: {} B\n", lang.pick("最悪", "worst case"), worst, lang.pick("上限", "limit"), limit));
    let ok = err.is_none() && worst <= limit;
    if let Some(e) = &err {
        t.push_str(&format!("  {}: {}\n", lang.pick("証明できません", "cannot prove"), e));
    } else if ok {
        t.push_str(&format!("  {}\n", lang.pick("OK: 上限に収まることを証明しました", "OK: proven to fit the limit")));
    } else {
        t.push_str(&format!("  {}\n", lang.pick("NG: 上限を超えます（F3003）", "NG: exceeds the limit (F3003)")));
    }
    Ok(Report { text: t, worst, ok })
}
