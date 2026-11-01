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

/// A dispatch node: (context, table) and the table's entries.
pub(super) struct Table {
    pub ctx: u32,
    pub entries: Vec<u32>,
}

pub(super) struct Layout {
    /// node -> successors (nodes 0..n are blocks, n.. tables)
    succ: Vec<Vec<usize>>,
    rpo: Vec<usize>,
    /// node -> position in `rpo` (usize::MAX: unreachable)
    order: Vec<usize>,
    idom: Vec<usize>,
    header: Vec<bool>,
}

/// Builds the graph of blocks `blocks` (contexts `ctxs`, found by `targets`) and dispatch
/// nodes `tables` (by (context, table address) in `jtab`), from block `entry`.
pub(super) fn layout(
    blocks: &[(u32, Vec<Instruction>)],
    ctxs: &[u32],
    targets: &BTreeMap<(u32, u32), u32>,
    inlined: &BTreeMap<usize, (usize, Option<u32>)>,
    ret_to: &[u32],
    tables: &[Table],
    jtab: &BTreeMap<(u32, u32), usize>,
    entry: usize,
) -> Layout {
    let n = blocks.len();
    let total = n + tables.len();
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
                if let Some(&d) = jump_table(last).and_then(|t| jtab.get(&(ctx, t))) {
                    s.push(d);
                }
            }
            _ => {}
        }
        succ[k] = s;
    }
    for (i, t) in tables.iter().enumerate() {
        let mut s: Vec<usize> = t.entries.iter().filter_map(|&a| find(t.ctx, a)).collect();
        s.sort_unstable();
        s.dedup();
        succ[n + i] = s;
    }
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
