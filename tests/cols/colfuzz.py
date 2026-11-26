#!/usr/bin/env python3
"""colfuzz.py — random Fascon programs for differential testing of the column optimization.

Each program has one arena with a random element layout, a set of `for id in @a` reductions
(sums, filtered sums, counts, min / max picks with random filters) and a main loop driven by an
LCG that exercises every way an element can change: alloc, free, remove, writes by Id (plain,
compound, nested, whole element), get_mut scopes (if let / match / loop with break and continue /
early return), match bindings of places inside the slot (`match @a[id].n { q => .. }`, whole element,
nested, as a value, break / continue / return from an arm, in bitmap loops), &mut arguments
(element, field, nested struct), take() on an Option field, writes in bitmap loops, array-field
writes, and optionally a final write through a stale Id.

With --tsoa the program keeps only the element accesses True SoA accepts (writes and reads by Id, get / get_mut
bindings used through their fields, alloc / free / remove, whole-element reads and writes, bulk overwrites, stale Ids),
while still taking the address of some fields and binding places inside the slot, so that the arena is True SoA and
some fields stay mirrored. The default output is unchanged.

With --float the program is about f32 / f64 fields instead: random values that include NaN, -0.0, +0.0, +inf and -inf,
sums (plain and filtered, by integer and float conditions) and min / max picks (`<`, `<=`, `>`, `>=`, with NaN / +-inf /
finite starting values), each written once inside `#[fast_math]` and once without it. Sums only add small integers (f32)
or quarters (f64) whose totals stay exact, so every association order gives the same bits: the outputs of --columns on
(float True-SoA columns and float key columns), on + --nontemporal on, mirrored and off must all agree, and the
`#[fast_math]` result must equal the written-order one.

With --rmw the program aims at masked column RMW: u32 / u64 / i32 / i64 / f32 / f64 columns (made True SoA by the
reductions that print them) receive random `if cond { @a[id].c += x / = x }` loops (the float ones under #[fast_math])
between allocations and frees; on (the vector kernels), on + --nontemporal on, mirrored and off must agree.

usage: colfuzz.py <seed> <out.fsc> [--tsoa | --float | --rmw]
"""
import random
import sys

U = ["u8", "u16", "u32", "u64", "usize"]
S = ["i32", "i64"]
RANGE = {"u8": 200, "u16": 60000, "u32": 1 << 20, "u64": 1 << 40, "usize": 1 << 20, "i32": 1 << 20, "i64": 1 << 40}
MAXV = {"u8": "255", "u16": "65535", "u32": "4294967295", "u64": "18446744073709551615", "usize": "18446744073709551615", "i32": "2147483647", "i64": "9223372036854775807"}
MINV = {"u8": "0", "u16": "0", "u32": "0", "u64": "0", "usize": "0", "i32": "-2147483647 - 1", "i64": "-9223372036854775807 - 1"}
BITS = {"u8": 8, "u16": 16, "u32": 32, "u64": 64, "usize": 64, "i32": 32, "i64": 64}


class Gen:
    def __init__(self, seed, tsoa=False):
        self.r = random.Random(seed)
        self.seed = seed
        self.tsoa = tsoa
        r = self.r
        self.cap = r.choice([1, 2, 3, 4, 5, 7, 8, 13, 31, 32, 33, 63, 64, 65, 100, 127, 128, 129, 200, 255, 256, 257, 700, 1000, 4096, 5000])
        self.big = r.random() < 0.12  # u64 values near 2^62 (exercises the exact pass / overflow faults)
        self.own = r.random() < 0.3
        if tsoa:
            self.own = False  # own makes the arena ineligible for True SoA
        self.nf = r.randint(2, 6)
        self.fields = []  # (name, type)
        for i in range(self.nf):
            self.fields.append((f"f{i}", r.choice(U + U + S)))
        # nested struct
        self.inner = [("a", r.choice(U + S)), ("b", r.choice(U))]
        self.statics = {}
        self.funcs = []
        self.reds = []  # (name, has_param, param_type)
        self.lines = []
        self.lim_t = None

    # ── values ──
    def val(self, t, shift=None):
        r = self.r
        sh = shift if shift is not None else r.randint(1, 12)
        if t == "bool":
            return f"((seed >> {sh}) % 2 == 1)"
        if t in ("u64", "usize") and self.big and t == "u64":
            return f"((seed >> {sh}) % 4 + 4611686018427387904)"
        rg = min(RANGE[t], 1 << 20) if r.random() < 0.7 else RANGE[t]
        if rg > (1 << 31) - 1:
            rg = (1 << 30)
        if t in S:
            return f"((((seed >> {sh}) % {rg}) as i64 - {rg // 2}) as {t})"
        return f"(((seed >> {sh}) % {rg}) as {t})"

    def lit(self, t):
        r = self.r
        if t in S:
            v = r.randint(-(RANGE[t] // 2), RANGE[t] // 2)
            return str(v)
        if t == "u64" and self.big:
            return str(4611686018427387904 + r.randint(0, 4))
        return str(r.randint(0, min(RANGE[t], 1 << 20)))

    def static(self, t):
        name = f"S_{t.upper()}_{len(self.statics)}"
        self.statics[name] = (t, self.lit(t) if t not in S else str(self.r.randint(-1000, 1000)))
        return name

    # ── element access ──
    def all_int_fields(self):
        out = [(n, t) for n, t in self.fields]
        out += [(f"n.{n}", t) for n, t in self.inner]
        return out

    def uint_fields(self):
        return [(n, t) for n, t in self.all_int_fields() if t in U]

    def atom(self, base, param):
        r = self.r
        f, t = r.choice(self.all_int_fields())
        k = r.random()
        cmp = r.choice(["==", "!=", "<", "<=", ">", ">="])
        if k < 0.30:
            return f"{base}{f} {cmp} {self.lit(t)}"
        if k < 0.42:
            return f"{base}{f} {cmp} {self.static(t)}"
        if k < 0.52:
            return f"{base}flag" if r.random() < 0.5 else f"!{base}flag"
        if k < 0.62 and t in U:
            return f"({base}{f} & {r.choice([1, 2, 3, 7, 12, 255])}) != 0"
        if k < 0.74:
            same = [(g, u) for g, u in self.all_int_fields() if u == t and g != f]
            if same:
                g, _ = r.choice(same)
                return f"{base}{f} {cmp} {base}{g}"
        if k < 0.84:
            return f"({base}{f} as i64) {cmp} {self.lit('i32')}"
        if k < 0.90 and param is not None and self.lim_t in (None, t):
            self.lim_t = t
            return f"{base}{f} {cmp} lim"  # a local: the 1.0.0 scan
        return f"{base}{f} {cmp} {self.lit(t)}"

    def filt(self, base, param):
        r = self.r
        n = r.choice([0, 1, 1, 1, 2, 2, 3])
        atoms = [self.atom(base, param) for _ in range(n)]
        if n >= 2 and r.random() < 0.2:
            return [f"({atoms[0]} || {atoms[1]})"] + atoms[2:]
        return atoms

    # ── reductions ──
    def reduction(self, i):
        r = self.r
        kind = r.choice(["sum", "sum", "fsum", "count", "pick", "pick", "pick"])
        use_param = r.random() < 0.15
        lookup = r.random() < 0.2
        base = "t." if lookup else "@a[id]."
        ptype = None
        name = f"r{i}"
        body = []
        self.lim_t = None
        if kind in ("sum", "fsum"):
            uf = self.uint_fields()
            if not uf:
                kind = "count"
        if kind in ("sum", "fsum"):
            acc = "u64"
            if r.random() < 0.15 and all(BITS[t] <= 16 for _, t in self.uint_fields()):
                acc = "u32"
            terms = []
            for _ in range(r.choice([1, 1, 1, 2, 3])):
                f, t = r.choice([x for x in self.uint_fields() if BITS[x[1]] <= BITS[acc]] or self.uint_fields())
                if BITS[t] > BITS[acc]:
                    acc = "u64"
                terms.append(f"{base}{f}" if t == acc else f"{base}{f} as {acc}")
            conds = self.filt(base, "p" if use_param else None) if kind == "fsum" or r.random() < 0.3 else []
            ptype = self.lim_t
            act = f"s += {' + '.join(terms)};" if r.random() < 0.7 else f"s = s + {' + '.join(terms)};"
            inner = act if not conds else f"if {' && '.join(conds)} {{ {act} }}"
            body = [f"    let mut s: {acc} = 0;", f"    for id in @a {{ {self.wrap(lookup, inner)} }}", "    s"]
            ret = acc
        elif kind == "count":
            c = r.choice([1, 1, 2, 3])
            conds = self.filt(base, "p" if use_param else None) or [self.atom(base, None)]
            ptype = self.lim_t
            acc = r.choice(["u64", "u32", "usize"])
            inner = f"if {' && '.join(conds)} {{ n += {c}; }}"
            body = [f"    let mut n: {acc} = 0;", f"    for id in @a {{ {self.wrap(lookup, inner)} }}", "    n"]
            ret = acc
        else:
            f, t = r.choice(self.all_int_fields())
            op = r.choice(["<", "<", "<=", ">", ">", ">="])
            less = op in ("<", "<=")
            init = MAXV[t] if less else MINV[t]
            if r.random() < 0.25:
                init = self.lit(t)
            conds = self.filt(base, "p" if use_param else None)
            ptype = self.lim_t
            cmpx = f"{base}{f} {op} best" if r.random() < 0.7 else f"best {self.mirror(op)} {base}{f}"
            allc = conds[:]
            allc.insert(r.randint(0, len(allc)), cmpx)
            with_pick = r.random() < 0.8
            act = f"best = {base}{f};" + (" pick = Some(id);" if with_pick else "")
            if with_pick and r.random() < 0.3:
                act = (" pick = Some(id);" if with_pick else "") + f" best = {base}{f};"
            inner = f"if {' && '.join(allc)} {{ {act} }}"
            body = [f"    let mut best: {t} = {init};", "    let mut pick: Option<Id<@a>> = None;", f"    for id in @a {{ {self.wrap(lookup, inner)} }}"]
            body.append("    let mut out: i64 = best as i64;" if t not in ("u64", "usize") else "    let mut out: i64 = (best % 9223372036854775807) as i64;")
            body.append("    match pick { Some(q) => { out = out + (@a[q].tag as i64) * 7; } None => { out = 0 - (out % 1000) - 3; } }")
            body.append("    out")
            ret = "i64"
        params = f"lim: {ptype}" if ptype else ""
        self.funcs.append(f"fn {name}({params}) -> {ret} {{\n" + "\n".join(body) + "\n}")
        self.reds.append((name, ptype))

    def find_lim_type(self, conds):
        # the type of the field compared with lim
        for c in conds:
            for part in c.replace("(", " ").replace(")", " ").split("||"):
                if "lim" in part:
                    lhs = part.split()[0]
                    fname = lhs.split(".", 1)[1] if lhs.startswith("@a[id].") else lhs[2:]
                    for n, t in self.all_int_fields():
                        if n == fname:
                            return t
        return "u32"

    @staticmethod
    def mirror(op):
        return {"<": ">", "<=": ">=", ">": "<", ">=": "<="}[op]

    @staticmethod
    def wrap(lookup, inner):
        if lookup:
            return f"if let Some(t) = @a.get(id) {{ {inner} }}"
        return inner

    # ── element literal ──
    def elem_lit(self, tag):
        parts = [f"tag: {tag}"]
        for n, t in self.fields:
            parts.append(f"{n}: {self.val(t)}")
        parts.append(f"flag: {self.val('bool')}")
        parts.append(f"n: In {{ a: {self.val(self.inner[0][1])}, b: {self.val(self.inner[1][1])} }}")
        parts.append("o: None")
        parts.append("arr: [1, 2, 3, 4]")
        if self.own:
            parts.append(f"child: own Node {{ x: {self.val('u32')} }}")
        return "E { " + ", ".join(parts) + " }"

    def rand_field_write(self, base):
        r = self.r
        k = r.random()
        if k < 0.12:
            return f"{base}flag = {self.val('bool')};"
        f, t = r.choice(self.all_int_fields())
        return f"{base}{f} = {self.val(t)};"

    def compound(self, base):
        cands = [(n, t) for n, t in self.all_int_fields() if t in ("u32", "u64", "usize", "i32", "i64")]
        if not cands:
            return self.rand_field_write(base)
        f, t = self.r.choice(cands)
        if self.big and t == "u64":
            return f"{base}{f} = {self.val(t)};"
        return f"{base}{f} += {self.r.randint(1, 5)};"

    def program(self):
        r = self.r
        nred = r.randint(3, 9)
        for i in range(nred):
            self.reduction(i)
        out = []
        out.append(f"// colfuzz seed {self.seed}: cap {self.cap}, big {self.big}, own {self.own}")
        if self.own:
            out.append("struct Node { x: u32 }")
        out.append(f"struct In {{ a: {self.inner[0][1]}, b: {self.inner[1][1]} }}")
        fl = ", ".join(f"{n}: {t}" for n, t in self.fields)
        own = ", child: own Node" if self.own else ""
        out.append(f"struct E {{ tag: u32, {fl}, flag: bool, n: In, o: Option<u32>, arr: [u32; 4]{own} }}")
        out.append(f"arena @a: E[{self.cap}];")
        body_funcs = list(self.funcs)
        # writers through references (never touch the arena)
        f, t = r.choice(self.all_int_fields())
        wf, wt = r.choice(self.fields)
        body_funcs.append(f"fn wr_elem(e: &mut E, v: u64) {{ e.{f} = (v % {min(RANGE[t], 1 << 20)}) as {t}; e.flag = v % 3 == 0; }}")
        body_funcs.append(f"fn wr_field(x: &mut {wt}, v: u64) {{ *x = (v % {min(RANGE[wt], 1 << 20)}) as {wt}; }}")
        ia, ta = self.inner[0]
        body_funcs.append(f"fn wr_nested(x: &mut In, v: u64) {{ x.{ia} = (v % 1000) as {ta}; x.b = (v % 200) as {self.inner[1][1]}; }}")
        g, gt = r.choice(self.all_int_fields())
        body_funcs.append(
            f"fn upd(id: Id<@a>, v: u64) -> u64 {{\n    if let Some(p) = @a.get_mut(id) {{\n        if v % 5 == 0 {{ p.{g} = (v % 100) as {gt}; return 1; }}\n        p.flag = v % 2 == 0;\n    }}\n    0\n}}"
        )
        # match の束縛の中からの return（借用の終わりで写しを作り直す） / a return from inside a match binding
        g2, gt2b = r.choice(self.all_int_fields())
        if not self.tsoa:
            body_funcs.append(
                f"fn upd2(id: Id<@a>, v: u64) -> u64 {{\n    match @a[id] {{\n        t => {{\n            if v % 5 == 0 {{ t.{g2} = (v % 100) as {gt2b}; return 1; }}\n            t.flag = v % 2 == 0;\n        }}\n    }}\n    0\n}}"
            )
        rep = ["fn report(tag: u64) {", "    print_int(tag as i64);"]
        for name, pt in self.reds:
            if pt:
                rep.append(f"    print_int({name}({self.lit(pt)}) as i64);")
            else:
                rep.append(f"    print_int({name}() as i64);")
        rep.append("    print_int(@a.len() as i64);")
        rep.append("}")
        body_funcs.append("\n".join(rep))
        for name, (t, v) in self.statics.items():
            out.append(f"static {name}: {t} = {v};")
        out.extend(body_funcs)
        M = min(self.cap + r.choice([0, 0, 1, 3]), 512)
        steps = r.choice([300, 800, 1500, 3000])
        every = r.choice([50, 100, 250])
        m = []
        m.append("fn main() {")
        m.append(f"    let mut seed: u64 = {r.randint(1, 2**31 - 1)};")
        m.append(f"    let mut ids: [Option<Id<@a>>; {M}] = [None; {M}];")
        m.append("    let mut dead: Option<Id<@a>> = None;")
        m.append("    let mut step: u64 = 0;")
        m.append(f"    while step < {steps} {{")
        m.append("        seed = (seed * 1103515245 + 12345) % 2147483648;")
        m.append("        let r = (seed >> 3) % 1000;")
        m.append(f"        let k = ((seed >> 9) % {M}) as usize;")
        ops = []
        ops.append(("25", f"if ids[k].is_none() {{ ids[k] = @a.alloc({self.elem_lit('(k as u32)')}); }}"))
        ops.append(("35", "if let Some(id) = ids[k] { @a.free(id); ids[k] = None; dead = Some(id); }"))
        ops.append(("39", "if let Some(id) = ids[k] { let rm = @a.remove(id); ids[k] = None; dead = Some(id); if let Some(x) = rm { print_int(x.tag as i64); } }"))
        ops.append(("49", f"if let Some(id) = ids[k] {{ {self.rand_field_write('@a[id].')} }}"))
        ops.append(("54", f"if let Some(id) = ids[k] {{ {self.compound('@a[id].')} {self.rand_field_write('@a[id].')} }}"))
        ops.append(("58", f"if let Some(id) = ids[k] {{ @a[id] = {self.elem_lit('((k as u32) + 1000)')}; }}"))
        f1, t1 = r.choice(self.all_int_fields())
        ops.append(("66", f"if let Some(id) = ids[k] {{ if let Some(p) = @a.get_mut(id) {{ {self.rand_field_write('p.')} if p.{f1} > {self.lit(t1)} {{ {self.rand_field_write('p.')} }} }} }}"))
        ops.append(
            (
                "70",
                "if let Some(id) = ids[k] { let mut j: u32 = 0; loop { if j >= 3 { break; } if let Some(p) = @a.get_mut(id) { "
                + self.rand_field_write("p.")
                + " if j == 1 { j += 1; continue; } if j == 2 { "
                + self.rand_field_write("p.")
                + " break; } } j += 1; } }",
            )
        )
        ops.append(("73", f"if let Some(id) = ids[k] {{ match @a.get_mut(id) {{ Some(p) => {{ {self.compound('p.')} }} None => {{ print_int(0); }} }} }}"))
        ops.append(("75", "if let Some(id) = ids[k] { wr_elem(&mut @a[id], seed >> 4); }"))
        ops.append(("76", "if let Some(id) = ids[k] { if let Some(p) = @a.get_mut(id) { if p.flag { wr_elem(p, seed >> 3); } else { wr_nested(&mut p.n, seed >> 2); } } }"))
        ops.append(("765", f"if let Some(id) = ids[k] {{ match @a.get_mut(id) {{ Some(p) => {{ wr_field(&mut p.{wf}, seed >> 6); if let Some(z) = p.o {{ if z == 2 {{ print_int(2); }} }} }} None => {{}} }} }}"))
        ops.append(("77", f"if let Some(id) = ids[k] {{ wr_field(&mut @a[id].{wf}, seed >> 5); }}"))
        ops.append(("78", "if let Some(id) = ids[k] { wr_nested(&mut @a[id].n, seed >> 6); }"))
        ops.append(("81", "if let Some(id) = ids[k] { let x = upd(id, seed >> 7); if x == 1 { print_int(1); } }"))
        ops.append(("83", "if let Some(id) = ids[k] { @a[id].o = Some((seed % 9) as u32); let x = @a[id].o.take(); if let Some(y) = x { if y == 3 { print_int(3); } } }"))
        ff, ft = r.choice(self.all_int_fields())
        ops.append(("86", f"if r % 3 == 0 {{ let x = {self.val(ft)}; for id in @a {{ @a[id].{ff} = x; }} }}"))
        fg, gt2 = r.choice(self.all_int_fields())
        ops.append(("89", f"if r % 3 == 1 {{ let x = {self.val(gt2)}; for id in @a {{ if let Some(p) = @a.get_mut(id) {{ p.{fg} = x; }} }} }}"))
        ops.append(("92", "if let Some(id) = ids[k] { @a[id].arr[2] = (seed % 77) as u32; }"))
        if self.own:
            ops.append(("95", "if let Some(id) = ids[k] { @a[id].child.x = (seed % 50) as u32; }"))
        # match / if let の束縛（スロットの中の場所を参照で束縛）を通した書き込み。アームの中からの break / continue /
        # return、値になる match、入れ子、ビットマップのループの中も
        # writes through match / if-let bindings (a place inside the slot bound by reference): break / continue / return
        # from inside an arm, a match used as a value, nesting, and inside a bitmap loop
        tb = self.inner[1][1]
        ops.append(("955", f"if let Some(id) = ids[k] {{ match @a[id].n {{ q => {{ q.{ia} = {self.val(ta)}; q.b = {self.val(tb)}; }} }} }}"))
        f2, t2 = r.choice(self.all_int_fields())
        ops.append(("961", f"if let Some(id) = ids[k] {{ match @a[id] {{ t => {{ {self.rand_field_write('t.')} if t.{f2} > {self.lit(t2)} {{ {self.rand_field_write('t.')} }} }} }} }}"))
        ops.append(
            (
                "967",
                "if let Some(id) = ids[k] { let mut j: u32 = 0; loop { if j >= 3 { break; } match @a[id] { t => { "
                + self.rand_field_write("t.")
                + " if j == 1 { j += 1; continue; } if j == 2 { "
                + self.rand_field_write("t.")
                + " break; } } } j += 1; } }",
            )
        )
        ops.append(("973", "if let Some(id) = ids[k] { let x = upd2(id, seed >> 7); if x == 1 { print_int(1); } }"))
        ops.append(("979", f"if let Some(id) = ids[k] {{ let v = match @a[id].n {{ q => {{ q.b = {self.val(tb)}; q.{ia} }} }}; if v > 5 {{ print_int(4); }} }}"))
        ops.append(("985", f"if r % 3 == 2 {{ for id in @a {{ match @a[id].n {{ q => {{ if q.b > 3 {{ q.{ia} = {self.val(ta)}; }} }} }} }} }}"))
        ops.append(("991", f"if let Some(id) = ids[k] {{ match @a[id] {{ t => {{ match t.n {{ q => {{ q.b = {self.val(tb)}; }} }} t.flag = !t.flag; }} }} }}"))
        ops.append(("996", "if let Some(id) = ids[k] { match @a[id] { t => { if t.flag { print_int(6); } } } }"))
        if self.tsoa:
            # 完全列化を壊す使い方（要素への参照・要素を束縛する match）を外し、要素まるごとの読み出しと古い Id の読み出しを足す
            # drop the uses that rule True SoA out (references to the element, match binding the element) and add
            # whole-element reads and reads through a stale Id
            banned = {"75", "76", "961", "967", "973", "991", "996"}
            ops = [(lim, code) for lim, code in ops if lim not in banned]
            f3, t3 = r.choice(self.all_int_fields())
            ops.append(("997", f"if let Some(id) = ids[k] {{ let c = @a[id].clone(); if c.{f3} > {self.lit(t3)} {{ print_int(7); }} if c.flag {{ print_int(8); }} }}"))
            # 条件付き一括更新（Masked Column RMW）: `+=` は小さな定数、`=` はローカルの値 / masked column RMW: `+=` with a small
            # constant, `=` with a local value
            rf = [(n, t) for n, t in self.fields if t in ("u32", "u64", "i32", "i64")]
            if rf:
                f4, t4 = r.choice(rf)
                # 条件は static を作らない形（static はもう書き出してある） / a condition that makes no static (statics are already out)
                f5, t5 = r.choice(self.all_int_fields())
                cnd = r.choice(["@a[id].flag", "!@a[id].flag", "@a[id].flag", f"@a[id].{f5} {r.choice(['<', '>=', '!='])} {self.lit(t5)}"])
                if r.random() < 0.5 and not (self.big and t4 == "u64"):
                    ops.append(("998", f"if r % 2 == 0 {{ for id in @a {{ if {cnd} {{ @a[id].{f4} += {r.randint(1, 5)}; }} }} }}"))
                else:
                    ops.append(("998", f"if r % 2 == 0 {{ let x = {self.val(t4)}; for id in @a {{ if {cnd} {{ @a[id].{f4} = x; }} }} }}"))
            ops.append(("999", "if let Some(d) = dead { if let Some(p) = @a.get(d) { print_int(p.tag as i64); } }"))
        first = True
        # r is 0..999 (permille) so that thresholds like 76.5 % fit
        for lim, code in ops:
            kw = "if" if first else "} else if"
            lv = int(lim) * 10 if len(lim) <= 2 else int(lim)
            m.append(f"        {kw} r < {lv} {{")
            m.append(f"            {code}")
            first = False
        m.append("        }")
        m.append(f"        if step % {every} == 0 {{ report(step); }}")
        m.append("        step += 1;")
        m.append("    }")
        m.append("    report(999999);")
        m.append("    if dead.is_some() { print_int(5); }")
        if r.random() < (0.3 if self.tsoa else 0.1) and not self.own:
            fw, tw = r.choice(self.all_int_fields())
            m.append(f"    if let Some(d) = dead {{ @a[d].{fw} = {self.val(tw)}; }}")
            m.append("    report(1000000);")
        elif self.tsoa and r.random() < 0.3:
            fw, tw = r.choice(self.all_int_fields())
            m.append(f"    if let Some(d) = dead {{ let x = @a[d].{fw}; print_int(x as i64); }}")
        m.append("}")
        out.extend(m)
        return "\n".join(out) + "\n"


class FloatGen:
    """f32 / f64 の列（#[fast_math]）の差分テスト / differential tests of float columns (#[fast_math])"""

    def __init__(self, seed):
        self.r = random.Random(seed)
        self.seed = seed
        r = self.r
        self.cap = r.choice([1, 3, 31, 64, 65, 127, 200, 256, 257, 1000, 3000, 4096])
        self.fields = [("tag", "u32"), ("k", "u32")]
        for i in range(r.randint(2, 4)):
            self.fields.append((f"x{i}", r.choice(["f32", "f64"])))
        self.floats = [(n, t) for n, t in self.fields if t in ("f32", "f64")]
        self.funcs = []
        self.names = []

    def fval(self, t, sh):
        """乱数からの値: 小さな整数（f64 は 1/4 刻みも）、ときどき NaN・-0.0・+0.0・+inf・-inf / a value from the seed: small
        integers (quarters for f64 too), sometimes NaN, -0.0, +0.0, +inf, -inf"""
        base = f"((((seed >> {sh}) % 2001) as i64 - 1000) as {t})"
        if t == "f64" and self.r.random() < 0.5:
            base = f"((((seed >> {sh}) % 8001) as i64 - 4000) as f64 * 0.25)"
        sp = self.r.randint(1, 9)
        return (
            f"(if (seed >> {sp}) % 37 == 0 {{ {t}::NAN }} else if (seed >> {sp}) % 37 == 1 {{ -0.0{t} }} else if (seed >> {sp}) % 37 == 2 "
            f"{{ 0.0{t} }} else if (seed >> {sp}) % 53 == 3 {{ {t}::INFINITY }} else if (seed >> {sp}) % 53 == 4 {{ {t}::NEG_INFINITY }} else {{ {base} }})"
        )

    def cond(self, base):
        r = self.r
        k = r.random()
        if k < 0.4:
            return f"{base}k == {r.randint(0, 6)}u32"
        if k < 0.6:
            return f"{base}k {r.choice(['<', '>=', '!='])} {r.randint(0, 6)}u32"
        f, t = r.choice(self.floats)
        lit = f"{r.randint(-800, 800)}.0{t}" if r.random() < 0.8 else f"-0.0{t}"
        return f"{base}{f} {r.choice(['<', '<=', '>', '>=', '==', '!='])} {lit}"

    def reduction(self, i):
        r = self.r
        f, t = r.choice(self.floats)
        lookup = r.random() < 0.2
        base = "t." if lookup else "@a[id]."
        conds = [self.cond(base) for _ in range(r.choice([0, 0, 1, 1, 2]))]
        wrap = (lambda x: f"if let Some(t) = @a.get(id) {{ {x} }}") if lookup else (lambda x: x)
        if r.random() < 0.45:
            same = [g for g, u in self.floats if u == t]
            terms = [f"{base}{r.choice(same)}" for _ in range(r.choice([1, 1, 2]))]
            act = f"s += {' + '.join(terms)};" if r.random() < 0.7 else f"s = s + {' + '.join(terms)};"
            inner = act if not conds else f"if {' && '.join(conds)} {{ {act} }}"
            for fast in (True, False):
                name = f"r{i}{'f' if fast else 'p'}"
                attr = "    #[fast_math]\n" if fast else ""
                self.funcs.append(f"fn {name}() -> {t} {{\n    let mut s: {t} = 0.0;\n{attr}    for id in @a {{ {wrap(inner)} }}\n    s\n}}")
                self.names.append((name, False))
        else:
            op = r.choice(["<", "<", "<=", ">", ">", ">="])
            init = r.choice([f"{t}::INFINITY" if op in ("<", "<=") else f"{t}::NEG_INFINITY", f"{t}::NAN", f"{r.randint(-900, 900)}.0{t}", f"{t}::MAX" if op in ("<", "<=") else f"{t}::MIN"])
            cmpx = f"{base}{f} {op} best" if r.random() < 0.7 else f"best {dict(zip(['<', '<=', '>', '>='], ['>', '>=', '<', '<=']))[op]} {base}{f}"
            allc = conds[:]
            allc.insert(r.randint(0, len(allc)), cmpx)
            act = f"best = {base}{f}; pick = Some(id);"
            inner = f"if {' && '.join(allc)} {{ {act} }}"
            for fast in (True, False):
                name = f"r{i}{'f' if fast else 'p'}"
                attr = "    #[fast_math]\n" if fast else ""
                self.funcs.append(
                    f"fn {name}() {{\n    let mut best: {t} = {init};\n    let mut pick: Option<Id<@a>> = None;\n{attr}    for id in @a {{ {wrap(inner)} }}\n"
                    f"    put(best); put(\" \"); put_hex(best.to_bits());\n    match pick {{ Some(q) => {{ put(\" at \"); put(@a[q].tag); }} None => {{ put(\" none\"); }} }}\n    put(\"\\n\");\n}}"
                )
                self.names.append((name, True))

    def elem(self, tag):
        parts = [f"tag: {tag}", "k: ((seed >> 5) % 7) as u32"]
        for n, t in self.floats:
            parts.append(f"{n}: {self.fval(t, self.r.randint(1, 12))}")
        return "E { " + ", ".join(parts) + " }"

    def program(self):
        r = self.r
        for i in range(r.randint(3, 7)):
            self.reduction(i)
        out = [f"// colfuzz --float seed {self.seed}: cap {self.cap}"]
        out.append("struct E { " + ", ".join(f"{n}: {t}" for n, t in self.fields) + " }")
        out.append(f"arena @a: E[{self.cap}];")
        out.extend(self.funcs)
        rep = ["fn report(step: u64) {", "    print_int(step as i64);"]
        for name, is_pick in self.names:
            # NaN の総和はビットの並び（どの NaN が伝わるか）が足す順で変わりうるので、NaN かどうかだけを比べる
            # a NaN sum may carry a different payload depending on the order, so only its NaN-ness is compared
            rep.append(f"    {name}();" if is_pick else f"    let v_{name} = {name}(); put(v_{name}); if !v_{name}.is_nan() {{ put(\" \"); put_hex(v_{name}.to_bits()); }} put(\"\\n\");")
        rep.append("    print_int(@a.len() as i64);")
        rep.append("}")
        out.append("\n".join(rep))
        M = min(self.cap + r.choice([0, 1, 3]), 512)
        steps = r.choice([300, 800, 2000])
        every = r.choice([50, 100, 250])
        m = ["fn main() {", f"    let mut seed: u64 = {r.randint(1, 2**31 - 1)};", f"    let mut ids: [Option<Id<@a>>; {M}] = [None; {M}];", "    let mut step: u64 = 0;", f"    while step < {steps} {{"]
        m.append("        seed = (seed * 1103515245 + 12345) % 2147483648;")
        m.append("        let r = (seed >> 3) % 100;")
        m.append(f"        let k = ((seed >> 9) % {M}) as usize;")
        f1, t1 = r.choice(self.floats)
        f2, t2 = r.choice(self.floats)
        ops = [
            ("30", f"if ids[k].is_none() {{ ids[k] = @a.alloc({self.elem('(k as u32)')}); }}"),
            ("45", "if let Some(id) = ids[k] { @a.free(id); ids[k] = None; }"),
            ("70", f"if let Some(id) = ids[k] {{ @a[id].{f1} = {self.fval(t1, r.randint(1, 12))}; }}"),
            ("80", f"if let Some(id) = ids[k] {{ @a[id].{f2} += 1.0; @a[id].k = ((seed >> 4) % 7) as u32; }}"),
            ("88", f"if let Some(id) = ids[k] {{ if let Some(p) = @a.get_mut(id) {{ p.{f1} = {self.fval(t1, r.randint(1, 12))}; }} }}"),
            ("94", f"if let Some(id) = ids[k] {{ @a[id] = {self.elem('((k as u32) + 1000)')}; }}"),
            ("97", f"if r % 2 == 0 {{ let x = {self.fval(t2, r.randint(1, 12))}; for id in @a {{ @a[id].{f2} = x; }} }}"),
            # #[fast_math] の条件付き一括更新（浮動小数点の Masked Column RMW） / a #[fast_math] masked RMW on floats
            ("99", f"if r % 2 == 1 {{ #[fast_math] for id in @a {{ if @a[id].k == {r.randint(0, 6)}u32 {{ @a[id].{f1} += {r.choice(['2.0', '-0.5', '0.0', '-0.0'])}{t1}; }} }} }}"),
        ]
        first = True
        for lim, code in ops:
            m.append(f"        {'if' if first else '} else if'} r < {lim} {{")
            m.append(f"            {code}")
            first = False
        m.append("        }")
        m.append(f"        if step % {every} == 0 {{ report(step); }}")
        m.append("        step += 1;")
        m.append("    }")
        m.append("    report(999999);")
        m.append("}")
        out.extend(m)
        return "\n".join(out) + "\n"


class RmwGen:
    """条件付き一括更新（Masked Column RMW）の差分テスト: 総和で完全列化される u32 / u64 / i32 / i64 / f32 / f64 の列へ、
    ランダムな条件（めったに書かない k と flag）と値で `+=` / `=` を繰り返し、確保・解放を混ぜる
    differential tests of masked column RMW: u32 / u64 / i32 / i64 / f32 / f64 columns made True SoA by sums get `+=` /
    `=` with random conditions (on the rarely written k and flag) and values, mixed with alloc and free"""

    def __init__(self, seed):
        self.r = random.Random(seed)
        self.seed = seed
        self.cap = self.r.choice([1, 63, 64, 65, 128, 300, 1000, 4096])

    def program(self):
        r = self.r
        cols = [("cu", "u32"), ("cw", "u64"), ("ci", "i32"), ("cl", "i64"), ("cf", "f32"), ("cd", "f64")]
        out = [f"// colfuzz --rmw seed {self.seed}: cap {self.cap}"]
        out.append("struct E { tag: u32, k: u32, flag: bool, " + ", ".join(f"{n}: {t}" for n, t in cols) + " }")
        out.append(f"arena @a: E[{self.cap}];")
        rep = ["fn report(step: u64) {", "    print_int(step as i64);"]
        for n, t in cols:
            if t in ("u32", "u64"):
                rep.append(f"    let mut s_{n}: u64 = 0;")
                rep.append(f"    for id in @a {{ s_{n} += @a[id].{n}{' as u64' if t == 'u32' else ''}; }}")
                rep.append(f"    print_int((s_{n} % 1000000007) as i64);")
            elif t in ("i32", "i64"):
                lo = "2147483647i32" if t == "i32" else "9223372036854775807i64"
                rep.append(f"    let mut m_{n}: {t} = {lo};")
                rep.append(f"    for id in @a {{ if @a[id].{n} < m_{n} {{ m_{n} = @a[id].{n}; }} }}")
                rep.append(f"    print_int(m_{n} as i64);")
            else:
                rep.append(f"    let mut s_{n}: {t} = 0.0;")
                rep.append(f"    #[fast_math]\n    for id in @a {{ s_{n} += @a[id].{n}; }}")
                rep.append(f"    put(s_{n}); put(\"\\n\");")
        rep.append("    print_int(@a.len() as i64);")
        rep.append("}")
        out.append("\n".join(rep))
        M = min(self.cap + 1, 512)
        m = ["fn main() {", f"    let mut seed: u64 = {r.randint(1, 2**31 - 1)};", f"    let mut ids: [Option<Id<@a>>; {M}] = [None; {M}];", "    let mut step: u64 = 0;", f"    while step < {r.choice([200, 600, 1500])} {{"]
        m.append("        seed = (seed * 1103515245 + 12345) % 2147483648;")
        m.append("        let r = (seed >> 3) % 100;")
        m.append(f"        let k = ((seed >> 9) % {M}) as usize;")
        ops = [("35", "if ids[k].is_none() { ids[k] = @a.alloc(E { tag: k as u32, k: ((seed >> 5) % 5) as u32, flag: (seed >> 7) % 2 == 0, cu: 1, cw: 2, ci: -3, cl: 4, cf: 0.5, cd: -0.25 }); }"),
               ("45", "if let Some(id) = ids[k] { @a.free(id); ids[k] = None; }")]
        lim = 45
        for _ in range(r.randint(4, 9)):
            n, t = r.choice(cols)
            cond = r.choice(["@a[id].flag", "!@a[id].flag", f"@a[id].k == {r.randint(0, 4)}u32", f"@a[id].k < {r.randint(1, 4)}u32 && @a[id].flag", f"@a[id].k != {r.randint(0, 4)}u32 || @a[id].flag"])
            if t in ("f32", "f64"):
                val = r.choice(["1.5", "-0.25", "0.0", "-0.0", "2.0"]) + t
                body = f"@a[id].{n} += {val};" if r.random() < 0.6 else f"@a[id].{n} = {val};"
                loop = f"#[fast_math] for id in @a {{ if {cond} {{ {body} }} }}"
            else:
                val = {"u32": f"{r.randint(0, 9)}u32", "u64": f"{r.randint(0, 9)}u64", "i32": f"{r.randint(-9, 9)}i32", "i64": f"{r.randint(-9, 9)}i64"}[t]
                body = f"@a[id].{n} += x;" if r.random() < 0.6 else f"@a[id].{n} = x;"
                loop = f"let x = {val}; for id in @a {{ if {cond} {{ {body} }} }}"
            lim += r.randint(4, 7)
            ops.append((str(min(lim, 99)), loop))
        ops.append(("100", "if let Some(id) = ids[k] { @a[id].k = ((seed >> 4) % 5) as u32; @a[id].flag = !@a[id].flag; }"))
        first = True
        for li, code in ops:
            m.append(f"        {'if' if first else '} else if'} r < {li} {{")
            m.append(f"            {code}")
            first = False
        m.append("        }")
        m.append(f"        if step % {r.choice([50, 100])} == 0 {{ report(step); }}")
        m.append("        step += 1;")
        m.append("    }")
        m.append("    report(999999);")
        m.append("}")
        out.extend(m)
        return "\n".join(out) + "\n"


def main():
    seed = int(sys.argv[1])
    if "--rmw" in sys.argv[3:]:
        g = RmwGen(seed)
    elif "--float" in sys.argv[3:]:
        g = FloatGen(seed)
    else:
        g = Gen(seed, tsoa="--tsoa" in sys.argv[3:])
    with open(sys.argv[2], "w") as f:
        f.write(g.program())


if __name__ == "__main__":
    main()
