//! Structured layout of a function's blocks (jit::FN): the control flow graph from the
//! function's entry, its dominator tree, and wasm `block`/`loop` nesting by Ramsey's
//! "Beyond Relooper" (ICFP 2022): every dominator tree child of a node gets a `block` around
//! the node whose end it follows, a loop header a `loop`. Every jump is then a `br` to a label
//! (no dispatch on a block number), and a `jmp [index*4 + table]` goes to one dispatch node
//! per table (a `br_table` on the index). Edges that would make the graph irreducible leave
//! the function.

use super::*;

/// The jump table of `jmp [index*4 + table]`.
pub(super) fn jump_table(ins: &Instruction) -> Option<u32> {
    (ins.flow_control() == FlowControl::IndirectBranch
        && ins.op0_kind() == OpKind::Memory
        && ins.memory_base() == Register::None
        && ins.memory_index().is_gpr32()
        && ins.memory_index_scale() == 4)
        .then(|| ins.memory_displacement32())
}

/// The jump table of a block's last jump: `jmp [index*4 + table]` (true: the index register
/// is there at the jump) or `mov r, [index*4 + table]; jmp r` (false: dispatched by the target).
pub(super) fn block_table(list: &[Instruction]) -> Option<(u32, bool)> {
    let last = list.last()?;
    if let Some(t) = jump_table(last) {
        return Some((t, true));
    }
    let prev = list.len().checked_sub(2).map(|i| &list[i])?;
    (last.flow_control() == FlowControl::IndirectBranch
        && last.op0_kind() == OpKind::Register
        && last.op0_register().is_gpr32()
        && prev.mnemonic() == Mnemonic::Mov
        && prev.op0_kind() == OpKind::Register
        && prev.op0_register() == last.op0_register()
        && prev.op1_kind() == OpKind::Memory
        && prev.memory_base() == Register::None
        && prev.memory_index().is_gpr32()
        && prev.memory_index_scale() == 4)
        .then(|| (prev.memory_displacement32(), false))
}

/// A perfect hash of addresses `keys` for a dispatch by target: (multiplier, shift, slot ->
/// key's index or keys.len()), slot = (address * multiplier) >> shift.
pub(super) fn perfect_hash(keys: &[u32]) -> (u32, u32, Vec<u32>) {
    let mut bits = 1;
    while (1usize << bits) < 2 * keys.len() {
        bits += 1;
    }
    loop {
        let mut m = 0x9e37_79b1u32;
        for _ in 0..64 {
            let shift = 32 - bits;
            let mut slots = alloc::vec![keys.len() as u32; 1 << bits];
            if keys.iter().enumerate().all(|(i, &k)| {
                let s = (k.wrapping_mul(m) >> shift) as usize;
                let free = slots[s] == keys.len() as u32;
                slots[s] = i as u32;
                free
            }) {
                return (m, shift, slots);
            }
            m = m.wrapping_mul(0x2545_f491).wrapping_add(0x6a09_e667) | 1;
        }
        bits += 1;
    }
}

/// The entries of the jump table at `t` (of a jump at `near`): while they look like code
/// addresses near it.
pub(super) fn table_entries<M: Memory>(mem: &mut M, t: u32, near: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for i in 0..1024u32 {
        let mut b = [0u8; 4];
        if mem.fetch(t.wrapping_add(4 * i), &mut b) < 4 {
            break;
        }
        let v = u32::from_le_bytes(b);
        if v.abs_diff(near) > 0x10_0000 {
            break;
        }
        out.push(v);
    }
    out
}

/// A dispatch node: (context, table) and the table's entries; `hash`: dispatched by the target
/// (see `perfect_hash`) instead of the index.
pub(super) struct Table {
    pub ctx: u32,
    pub entries: Vec<u32>,
    pub hash: Option<(u32, u32, Vec<u32>)>,
}

pub(super) struct Layout {
    /// node -> successors (nodes 0..n are blocks, n.. tables)
    pub(super) succ: Vec<Vec<usize>>,
    rpo: Vec<usize>,
    /// node -> position in `rpo` (usize::MAX: unreachable)
    order: Vec<usize>,
    idom: Vec<usize>,
    header: Vec<bool>,
}

/// Builds the graph of blocks `blocks` (contexts `ctxs`, found by `targets`) and dispatch
/// nodes `tables` (each block's in `jnode`), from block `entry`.
pub(super) fn layout(
    blocks: &[(u32, Vec<Instruction>)],
    ctxs: &[u32],
    targets: &BTreeMap<(u32, u32), u32>,
    inlined: &BTreeMap<usize, (usize, Option<u32>)>,
    ret_to: &[u32],
    tables: &[Table],
    jnode: &BTreeMap<usize, usize>,
    entry: usize,
    extra: &[usize],
) -> Layout {
    let n = blocks.len();
    // (more entries: a node before them all, the last, going to each)
    let total = n + tables.len() + !extra.is_empty() as usize;
    let find = |ctx: u32, a: u32| targets.get(&(ctx, a)).or_else(|| targets.get(&(0, a))).map(|&b| b as usize);
    let mut succ: Vec<Vec<usize>> = alloc::vec![Vec::new(); total];
    for k in 0..n {
        let last = blocks[k].1.last().unwrap();
        let ctx = ctxs[k];
        let mut s = Vec::new();
        match last.flow_control() {
            FlowControl::Next | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch if last.flow_control() == FlowControl::Next || last.op0_kind() == OpKind::NearBranch32 => {
                s.extend(successors(last).into_iter().filter_map(|a| find(ctx, a)));
            }
            FlowControl::Call | FlowControl::IndirectCall => {
                if let Some(&(e, _)) = inlined.get(&k) {
                    s.push(e);
                }
                s.extend(find(ctx, last.next_ip32()));
            }
            FlowControl::Return if ctx != 0 => {
                s.extend(find(0, ret_to[ctx as usize]));
            }
            FlowControl::IndirectBranch => {
                if let Some(&(e, _)) = inlined.get(&k) {
                    s.push(e);
                }
                if let Some(&d) = jnode.get(&k) {
                    s.push(d);
                }
            }
            _ => {}
        }
        // an inlined callee can leave anywhere to a function call of its rest, which returns
        // to the call's return address (Gen::call_rest)
        if ctx != 0 {
            s.extend(find(0, ret_to[ctx as usize]));
            s.sort_unstable();
            s.dedup();
        }
        succ[k] = s;
    }
    for (i, t) in tables.iter().enumerate() {
        let mut s: Vec<usize> = t.entries.iter().filter_map(|&a| find(t.ctx, a)).collect();
        s.sort_unstable();
        s.dedup();
        succ[n + i] = s;
    }
    let entry = if extra.is_empty() {
        entry
    } else {
        succ[total - 1] = core::iter::once(entry).chain(extra.iter().copied()).collect();
        total - 1
    };
    // reverse postorder (iterative DFS)
    let mut post = Vec::with_capacity(total);
    let mut seen = alloc::vec![false; total];
    let mut stack = alloc::vec![(entry, 0usize)];
    seen[entry] = true;
    while let Some(&mut (v, ref mut i)) = stack.last_mut() {
        if *i < succ[v].len() {
            let w = succ[v][*i];
            *i += 1;
            if !seen[w] {
                seen[w] = true;
                stack.push((w, 0));
            }
        } else {
            post.push(v);
            stack.pop();
        }
    }
    post.reverse();
    let rpo = post;
    let mut order = alloc::vec![usize::MAX; total];
    for (i, &v) in rpo.iter().enumerate() {
        order[v] = i;
    }
    // dominators (Cooper, Harvey, Kennedy)
    let mut preds: Vec<Vec<usize>> = alloc::vec![Vec::new(); total];
    for &v in &rpo {
        for &w in &succ[v] {
            preds[w].push(v);
        }
    }
    let mut idom = alloc::vec![usize::MAX; total];
    idom[entry] = entry;
    loop {
        let mut changed = false;
        for &v in rpo.iter().skip(1) {
            let mut new = usize::MAX;
            for &p in &preds[v] {
                if idom[p] == usize::MAX {
                    continue;
                }
                new = if new == usize::MAX {
                    p
                } else {
                    let (mut a, mut b) = (p, new);
                    while a != b {
                        while order[a] > order[b] {
                            a = idom[a];
                        }
                        while order[b] > order[a] {
                            b = idom[b];
                        }
                    }
                    a
                };
            }
            if idom[v] != new {
                idom[v] = new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let dominates = |a: usize, mut b: usize| loop {
        if a == b {
            return true;
        }
        if b == entry {
            return false;
        }
        b = idom[b];
    };
    let mut header = alloc::vec![false; total];
    for &v in &rpo {
        for &w in &succ[v] {
            if order[w] <= order[v] && dominates(w, v) {
                header[w] = true;
            }
        }
    }
    Layout { succ, rpo, order, idom, header }
}

impl Layout {
    /// The blocks (below `n`) for which `want` holds that the function can also be entered at:
    /// reached, and in no loop but as the header of the outermost.
    pub(super) fn entries(&self, n: usize, want: impl Fn(usize) -> bool) -> Vec<usize> {
        let total = self.succ.len();
        let mut preds: Vec<Vec<usize>> = alloc::vec![Vec::new(); total];
        for &v in &self.rpo {
            for &w in &self.succ[v] {
                preds[w].push(v);
            }
        }
        // natural loops' bodies but their headers
        let mut inside = alloc::vec![false; total];
        for &u in &self.rpo {
            for &h in &self.succ[u] {
                if !self.back(u, h) {
                    continue;
                }
                let mut seen = alloc::vec![false; total];
                seen[h] = true;
                let mut stack = alloc::vec![u];
                while let Some(v) = stack.pop() {
                    if seen[v] {
                        continue;
                    }
                    seen[v] = true;
                    inside[v] = true;
                    stack.extend(preds[v].iter().copied());
                }
            }
        }
        (0..n.min(total)).filter(|&k| self.order[k] != usize::MAX && !inside[k] && want(k)).collect()
    }

    /// Each loop header's natural loop: the nodes in it (the header too).
    pub(super) fn loop_bodies(&self) -> BTreeMap<usize, Vec<bool>> {
        let total = self.succ.len();
        let mut preds: Vec<Vec<usize>> = alloc::vec![Vec::new(); total];
        for &v in &self.rpo {
            for &w in &self.succ[v] {
                preds[w].push(v);
            }
        }
        let mut out: BTreeMap<usize, Vec<bool>> = BTreeMap::new();
        for &u in &self.rpo {
            for &h in &self.succ[u] {
                if !self.back(u, h) {
                    continue;
                }
                let seen = out.entry(h).or_insert_with(|| {
                    let mut v = alloc::vec![false; total];
                    v[h] = true;
                    v
                });
                let mut stack = alloc::vec![u];
                while let Some(v) = stack.pop() {
                    if seen[v] {
                        continue;
                    }
                    seen[v] = true;
                    stack.extend(preds[v].iter().copied());
                }
            }
        }
        out
    }

    /// The first node.
    pub(super) fn entry(&self) -> usize {
        self.rpo[0]
    }

    /// The reached nodes, in reverse postorder.
    pub(super) fn order_iter(&self) -> core::slice::Iter<'_, usize> {
        self.rpo.iter()
    }

    /// Whether node `k` heads a loop.
    pub(super) fn header(&self, k: usize) -> bool {
        self.header[k]
    }

    /// Whether the edge u -> v is a back edge (to a loop header dominating u).
    fn back(&self, u: usize, v: usize) -> bool {
        if self.order[v] > self.order[u] {
            return false;
        }
        let mut b = u;
        loop {
            if b == v {
                return true;
            }
            if self.idom[b] == b {
                return false;
            }
            b = self.idom[b];
        }
    }

    /// Irreducible edges (retreating but not back): these jumps leave the function.
    pub fn irreducible(&self) -> usize {
        let mut n = 0;
        for &v in &self.rpo {
            for &w in &self.succ[v] {
                if self.order[w] <= self.order[v] && !self.back(v, w) {
                    n += 1;
                }
            }
        }
        n
    }
}

enum Act {
    Tree(usize),
    Within(usize, usize),
    AfterBlock(usize),
    EndLoop(usize),
}

/// Emits the function body (inside the exit/dyn blocks): every reachable node once, nested
/// as described above. `emit(g, node)` emits a node's code (its jumps go through
/// Gen::goto_block, which finds the open labels in `g.fwd`/`g.back`).
pub(super) fn emit<'a>(g: &mut Gen<'a>, l: &Layout, mut emit_node: impl FnMut(&mut Gen<'a>, usize)) {
    let total = l.succ.len();
    let mut children: Vec<Vec<usize>> = alloc::vec![Vec::new(); total];
    for &v in l.rpo.iter().skip(1) {
        children[l.idom[v]].push(v);
    }
    for c in &mut children {
        // the outermost block for the latest in reverse postorder
        c.sort_unstable_by_key(|&v| core::cmp::Reverse(l.order[v]));
    }
    let mut stack = alloc::vec![Act::Tree(l.rpo[0])];
    while let Some(a) = stack.pop() {
        match a {
            Act::Tree(x) => {
                if l.header[x] {
                    // (the checks hoisted out of it, on the way in: see Gen::loop_checks)
                    g.loop_checks(x);
                    g.c.loop_();
                    g.depth += 1;
                    g.back[x] = Some(g.depth);
                    stack.push(Act::EndLoop(x));
                }
                stack.push(Act::Within(x, 0));
            }
            Act::Within(x, i) => {
                if i == children[x].len() {
                    emit_node(g, x);
                } else {
                    let y = children[x][i];
                    g.c.block();
                    g.depth += 1;
                    g.fwd[y] = Some(g.depth);
                    stack.push(Act::AfterBlock(y));
                    stack.push(Act::Within(x, i + 1));
                }
            }
            Act::AfterBlock(y) => {
                g.c.end();
                g.depth -= 1;
                g.fwd[y] = None;
                stack.push(Act::Tree(y));
            }
            Act::EndLoop(x) => {
                g.back[x] = None;
                g.c.end();
                g.depth -= 1;
            }
        }
    }
}
