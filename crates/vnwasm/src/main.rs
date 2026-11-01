//! vnwasm: turns a Wine PE DLL compiled to wasm (by hangover/build-wasm-dll.py, linked with
//! `wasm-ld -shared`) into the module vnvm runs instead of the DLL's x86 code
//! (see docs/wasm-dlls.md).
//!
//! The x86 DLL stays the shell: Wine's loader maps it, resolves its imports and finds its
//! resources as usual. When it is mapped, vnhost loads this module next to it and points the
//! DLL's export table and entry point at the module's functions. What this tool does:
//!
//! - **Function pointers are x86 addresses.** A pointer to one of the module's functions is
//!   `BIAS + table slot`, an address in the x86 core's trap range: x86 code calling it stops
//!   the core and vnhost calls the function (an "entry"). `__table_base` is that biased value;
//!   the element segment is placed with `vn.table_slot` instead.
//! - **Every indirect call is checked** (like ARM64EC's call checker): `call_indirect` becomes
//!   a call to `icall<type>`, which calls into the table for the module's (and other wasm
//!   DLLs') functions and otherwise copies the arguments onto the x86 stack and runs the x86
//!   code there (`vn.x86_call`).
//! - **Imports go through the x86 DLL's import address table**: each imported function becomes
//!   a function that loads its IAT slot (filled in by Wine's loader) and calls it as above.
//!   GOT entries (addresses of imported data and functions) are set from the IAT by vnhost.
//! - **Entries** `__vn_in<k>(slot, argp, ecx, edx) -> i64 | f64` read the arguments from the x86
//!   stack and call the slot; the metadata says which entry each slot uses and how many bytes
//!   it pops (stdcall unless the .spec file says otherwise).
//! - `NtCurrentTeb()` (`__vnwasm_teb()`) reads the per-thread global `vn.teb`.
//!
//! The metadata goes into a custom section "vnwasm" (see `Meta`).

mod pe;
mod spec;

use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, HashMap};
use walrus::ir::{BinaryOp, Instr, InstrLocId, InstrSeq, LoadKind, MemArg, StoreKind, UnaryOp};
use walrus::{
    ConstExpr, ElementItems, ElementKind, ExportItem, FunctionBuilder, FunctionId, GlobalId, ImportKind,
    LocalFunction, MemoryId, Module, RawCustomSection, TableId, TypeId, ValType,
};

/// x86 addresses of the wasm DLLs' functions start here (vnhost: wasmdll::BIAS)
const BIAS: u32 = 0xfe00_0000;

struct Args {
    input: String,
    pe: String,
    spec: String,
    output: String,
}

fn args() -> Result<Args> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 6 || a[4] != "-o" {
        bail!("usage: vnwasm <in.wasm> <x86.dll> <dll.spec> -o <out.wasm>");
    }
    Ok(Args { input: a[1].clone(), pe: a[2].clone(), spec: a[3].clone(), output: a[5].clone() })
}

/// dylink.0's memory info: (memory size, alignment log2, table size)
fn dylink(bytes: &[u8]) -> Result<(u32, u32, u32)> {
    fn leb(b: &[u8], at: &mut usize) -> u32 {
        let (mut v, mut shift) = (0u32, 0);
        loop {
            let c = b[*at];
            *at += 1;
            v |= ((c & 0x7f) as u32) << shift;
            if c & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    }
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = leb(bytes, &mut at) as usize;
        let end = at + size;
        if id == 0 {
            let mut p = at;
            let nlen = leb(bytes, &mut p) as usize;
            let name = &bytes[p..p + nlen];
            p += nlen;
            if name == b"dylink.0" {
                while p < end {
                    let sub = bytes[p];
                    p += 1;
                    let len = leb(bytes, &mut p) as usize;
                    if sub == 1 {
                        let mut q = p;
                        let mem = leb(bytes, &mut q);
                        let align = leb(bytes, &mut q);
                        let table = leb(bytes, &mut q);
                        return Ok((mem, align, table));
                    }
                    p += len;
                }
            }
        }
        at = end;
    }
    bail!("no dylink.0 memory info (link with wasm-ld -shared)")
}

/// How an entry reads one parameter from the x86 stack.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Arg {
    /// a value of this type at the next stack offset
    Val(ValType),
    /// a struct passed by value: the wasm function gets its address on the x86 stack; it takes
    /// this many bytes there
    ByVal(u32),
    /// fastcall/thiscall: from ECX / EDX
    Ecx,
    Edx,
    /// varargs: the address of what follows on the stack
    Rest,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct EntryKind {
    ty: TypeId,
    args: Vec<Arg>,
}

fn size(t: ValType) -> u32 {
    match t {
        ValType::I64 | ValType::F64 => 8,
        ValType::V128 => 16,
        _ => 4,
    }
}

/// The metadata section, all little-endian u32 unless noted:
///   "VNW1", x86 SizeOfImage, x86 TimeDateStamp, name (u32 length + bytes)
///   memory size, memory alignment (log2), table size, entry point slot
///   slots: count, then per slot u16 entry kind, u16 bytes popped
///   entries: count, then per entry kind u32 flags (1 = returns f64)
///   exports: count, then (export address table index, kind 0 func / 1 data, slot or offset)
///   got: count, then (u32 kind 0 mem / 1 func: the IAT slot's value, 2: the image address + RVA;
///        name, RVA)
#[derive(Default)]
struct Meta {
    b: Vec<u8>,
}

impl Meta {
    fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, v: u16) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.b.extend_from_slice(s.as_bytes());
    }
}

fn main() -> Result<()> {
    let a = args()?;
    let bytes = std::fs::read(&a.input).with_context(|| a.input.clone())?;
    let (mem_size, mem_align, _) = dylink(&bytes)?;
    let pe_bytes = std::fs::read(&a.pe).with_context(|| a.pe.clone())?;
    let pe = pe::parse(&pe_bytes).with_context(|| a.pe.clone())?;
    let spec = spec::parse(&std::fs::read_to_string(&a.spec).with_context(|| a.spec.clone())?);
    let mut m = Module::from_buffer(&bytes)?;

    let memory: MemoryId = m.memories.iter().next().context("no memory")?.id();
    let table: TableId = m.tables.iter().next().context("no table")?.id();

    // globals we use or rewrite
    let mut gimport = HashMap::new();
    for imp in m.imports.iter() {
        if let ImportKind::Global(g) = imp.kind {
            gimport.insert((imp.module.clone(), imp.name.clone()), g);
        }
    }
    let table_base = *gimport.get(&("env".into(), "__table_base".into())).context("no __table_base")?;
    let stack_pointer = *gimport.get(&("env".into(), "__stack_pointer".into())).context("no __stack_pointer")?;
    let (table_slot, _) = m.add_import_global("vn", "table_slot", ValType::I32, false, false);
    let (image_base, _) = m.add_import_global("vn", "image_base", ValType::I32, false, false);
    let (teb, _) = m.add_import_global("vn", "teb", ValType::I32, false, false);
    let (limit, _) = m.add_import_global("vn", "limit", ValType::I32, true, false);
    let x86_call_ty = m.types.add(&[ValType::I32, ValType::I32], &[ValType::I64]);
    let x86_call_f_ty = m.types.add(&[ValType::I32, ValType::I32], &[ValType::F64]);
    let (x86_call, _) = m.add_import_func("vn", "x86_call", x86_call_ty);
    let (x86_call_f, _) = m.add_import_func("vn", "x86_call_f", x86_call_f_ty);

    // 1. indirect calls -> icall<type>
    let mut icalls: BTreeMap<TypeId, FunctionId> = BTreeMap::new();
    {
        let mut types = Vec::new();
        for f in m.funcs.iter() {
            if let walrus::FunctionKind::Local(l) = &f.kind {
                collect_call_indirect(l, &mut types);
            }
        }
        // the imports' stubs call through icall too
        for imp in m.imports.iter() {
            if let ImportKind::Function(fid) = imp.kind {
                if imp.module == "env" {
                    if let walrus::FunctionKind::Import(i) = &m.funcs.get(fid).kind {
                        types.push(i.ty);
                    }
                }
            }
        }
        types.sort();
        types.dedup();
        for ty in types {
            let f = make_icall(&mut m, ty, table, memory, limit, stack_pointer, x86_call, x86_call_f);
            icalls.insert(ty, f);
        }
    }
    let ids: Vec<FunctionId> = m.funcs.iter().map(|f| f.id()).collect();
    for fid in ids {
        if icalls.values().any(|&f| f == fid) {
            continue;
        }
        if let walrus::FunctionKind::Local(l) = &mut m.funcs.get_mut(fid).kind {
            rewrite_call_indirect(l, &icalls);
        }
    }

    // 2. imported functions -> IAT stubs (or teb)
    let iat: HashMap<&str, u32> = pe.imports.iter().map(|(_, s, rva)| (s.as_str(), *rva)).collect();
    let mut missing = Vec::new();
    let fimports: Vec<(FunctionId, String, String)> = m
        .imports
        .iter()
        .filter_map(|i| match i.kind {
            ImportKind::Function(f) => Some((f, i.module.clone(), i.name.clone())),
            _ => None,
        })
        .collect();
    for (fid, module, name) in fimports {
        if module != "env" {
            continue;
        }
        if name == "__vnwasm_teb" {
            m.replace_imported_func(fid, |(b, _)| {
                b.global_get(teb);
            })?;
            continue;
        }
        let Some(&rva) = iat.get(name.as_str()) else {
            missing.push(name);
            continue;
        };
        let ty = match &m.funcs.get(fid).kind {
            walrus::FunctionKind::Import(i) => i.ty,
            _ => unreachable!(),
        };
        let icall = icalls[&ty];
        m.replace_imported_func(fid, |(b, args)| {
            for &l in args {
                b.local_get(l);
            }
            b.global_get(image_base)
                .i32_const(rva as i32)
                .binop(BinaryOp::I32Add)
                .load(memory, LoadKind::I32 { atomic: false }, MemArg { align: 4, offset: 0 })
                .call(icall);
        })?;
    }
    if !missing.is_empty() {
        missing.sort();
        bail!("functions neither defined nor imported by {}: {}", a.pe, missing.join(" "));
    }

    // GOT entries: from the IAT
    let mut got = Vec::new();
    for imp in m.imports.iter() {
        if imp.module == "GOT.mem" || imp.module == "GOT.func" {
            if imp.name == "__ImageBase" {
                got.push((2, imp.name.clone(), 0));
                continue;
            }
            let rva = *iat.get(imp.name.as_str()).with_context(|| format!("{} {} is not imported by {}", imp.module, imp.name, a.pe))?;
            got.push(((imp.module == "GOT.func") as u32, imp.name.clone(), rva));
        }
    }

    // 3. the table: the element segment at vn.table_slot, exported functions in it
    let elem_id = {
        let mut found = None;
        for e in m.elements.iter_mut() {
            if let ElementKind::Active { offset, .. } = &mut e.kind {
                if matches!(offset, ConstExpr::Global(g) if *g == table_base) {
                    *offset = ConstExpr::Global(table_slot);
                    found = Some(e.id());
                }
            }
        }
        match found {
            Some(id) => id,
            None => m.elements.add(
                ElementKind::Active { table, offset: ConstExpr::Global(table_slot) },
                ElementItems::Functions(Vec::new()),
            ),
        }
    };
    let mut slot_of: HashMap<FunctionId, u32> = HashMap::new();
    let mut slots: Vec<FunctionId> = match &m.elements.get(elem_id).items {
        ElementItems::Functions(v) => v.clone(),
        _ => bail!("element segment of expressions"),
    };
    for (i, f) in slots.iter().enumerate() {
        slot_of.entry(*f).or_insert(i as u32);
    }
    let exported: HashMap<String, ExportItem> = m.exports.iter().map(|e| (e.name.clone(), e.item)).collect();
    let mut slot = |f: FunctionId, slots: &mut Vec<FunctionId>| -> u32 {
        *slot_of.entry(f).or_insert_with(|| {
            slots.push(f);
            slots.len() as u32 - 1
        })
    };

    // entries: per slot, how x86 code calls it
    let mut kinds: Vec<EntryKind> = Vec::new();
    let mut kind_ids: HashMap<EntryKind, u16> = HashMap::new();
    let mut slot_info: HashMap<u32, (EntryKind, u32)> = HashMap::new();
    let entry_point = match exported.get("__vnwasm_entry") {
        Some(ExportItem::Function(f)) => slot(*f, &mut slots),
        _ => bail!("no __vnwasm_entry (hangover/wasmdll/crt.c)"),
    };
    let mut eat = Vec::new(); // (index, kind, value)
    let eat_index = |name: &str, ordinal: Option<u32>| -> Option<u32> {
        if !name.is_empty() {
            if let Some((_, i)) = pe.exports.iter().find(|(n, _)| n == name) {
                return Some(*i);
            }
        }
        ordinal.map(|o| o - pe.ordinal_base).filter(|&i| i < pe.nfuncs)
    };
    let mut unimpl = Vec::new();
    for e in &spec.funcs {
        let Some(index) = eat_index(&e.name, e.ordinal) else { continue };
        let Some(ExportItem::Function(f)) = exported.get(&e.symbol) else {
            unimpl.push(e.symbol.clone());
            continue;
        };
        let ty = match &m.funcs.get(*f).kind {
            walrus::FunctionKind::Local(l) => l.ty(),
            _ => continue,
        };
        let params = m.types.get(ty).params().to_vec();
        let (args, pop) = spec_args(e, &params).with_context(|| format!("export {}", e.name))?;
        let s = slot(*f, &mut slots);
        slot_info.insert(s, (EntryKind { ty, args }, pop));
        eat.push((index, 0u32, s));
    }
    let mut data = Vec::new();
    for d in &spec.data {
        let Some(index) = eat_index(&d.name, d.ordinal) else { continue };
        match exported.get(&d.symbol) {
            Some(ExportItem::Global(g)) => match m.globals.get(*g).kind {
                walrus::GlobalKind::Local(ConstExpr::Value(walrus::ir::Value::I32(v))) => data.push((index, 1u32, v as u32)),
                _ => bail!("data export {} is not a constant", d.symbol),
            },
            _ => unimpl.push(d.symbol.clone()),
        }
    }
    eat.extend(data);
    if !unimpl.is_empty() {
        eprintln!("vnwasm: {}: {} exports stay x86: {}", a.pe, unimpl.len(), unimpl.join(" "));
    }
    if let ElementItems::Functions(v) = &mut m.elements.get_mut(elem_id).items {
        *v = slots.clone();
    }
    let table_size = slots.len() as u32;

    // every slot's entry (the default: stdcall with the wasm parameters)
    let mut slot_kind = Vec::new();
    for (i, f) in slots.iter().enumerate() {
        let (kind, pop) = match slot_info.get(&(i as u32)) {
            Some(k) => k.clone(),
            None => {
                let ty = match &m.funcs.get(*f).kind {
                    walrus::FunctionKind::Local(l) => l.ty(),
                    walrus::FunctionKind::Import(i) => i.ty,
                    walrus::FunctionKind::Uninitialized(t) => *t,
                };
                let params = m.types.get(ty).params().to_vec();
                let pop = params.iter().map(|&t| size(t)).sum();
                (EntryKind { ty, args: params.into_iter().map(Arg::Val).collect() }, pop)
            }
        };
        let k = *kind_ids.entry(kind.clone()).or_insert_with(|| {
            kinds.push(kind);
            kinds.len() as u16 - 1
        });
        slot_kind.push((k, pop as u16));
    }
    let mut entry_flags = Vec::new();
    for (k, kind) in kinds.iter().enumerate() {
        let float = make_entry(&mut m, k, kind, table, memory)?;
        entry_flags.push(float as u32);
    }

    // exports: only what vnhost calls
    let keep = ["__wasm_apply_data_relocs", "__wasm_call_ctors"];
    let drop: Vec<_> = m
        .exports
        .iter()
        .filter(|e| !keep.contains(&e.name.as_str()) && !e.name.starts_with("__vn_in"))
        .map(|e| e.id())
        .collect();
    for e in drop {
        m.exports.delete(e);
    }
    walrus::passes::gc::run(&mut m);

    let mut meta = Meta::default();
    meta.b.extend_from_slice(b"VNW1");
    meta.u32(pe.size_of_image);
    meta.u32(pe.timestamp);
    meta.str(&pe.name);
    meta.u32(mem_size);
    meta.u32(mem_align);
    meta.u32(table_size);
    meta.u32(entry_point);
    meta.u32(slot_kind.len() as u32);
    for (k, pop) in &slot_kind {
        meta.u16(*k);
        meta.u16(*pop);
    }
    meta.u32(entry_flags.len() as u32);
    for f in &entry_flags {
        meta.u32(*f);
    }
    meta.u32(eat.len() as u32);
    for (i, k, v) in &eat {
        meta.u32(*i);
        meta.u32(*k);
        meta.u32(*v);
    }
    meta.u32(got.len() as u32);
    for (k, name, rva) in &got {
        meta.u32(*k);
        meta.str(name);
        meta.u32(*rva);
    }
    // drop dylink.0 (vnhost loads the module itself) and debug names
    let customs: Vec<_> = m.customs.iter().map(|(id, _)| id).collect();
    for id in customs {
        m.customs.delete(id);
    }
    m.customs.add(RawCustomSection { name: "vnwasm".into(), data: meta.b });
    m.emit_wasm_file(&a.output)?;
    eprintln!(
        "vnwasm: {} -> {}: {} slots, {} entries, {} exports, {} GOT, {} KB data",
        a.input,
        a.output,
        table_size,
        kinds.len(),
        eat.len(),
        got.len(),
        mem_size / 1024
    );
    Ok(())
}

/// How x86 code passes an export's arguments (from its .spec entry) against the wasm
/// function's parameters, and how many bytes the function pops.
fn spec_args(e: &spec::Entry, params: &[ValType]) -> Result<(Vec<Arg>, u32)> {
    let mut args = Vec::new();
    let mut p = params.iter();
    let mut regs = match e.conv {
        spec::Conv::Fastcall => vec![Arg::Ecx, Arg::Edx],
        spec::Conv::Thiscall => vec![Arg::Ecx],
        _ => vec![],
    };
    regs.reverse();
    let mut stack = 0;
    for &bytes in &e.args {
        let Some(&t) = p.next() else { bail!("{} has more .spec arguments than C parameters", e.symbol) };
        if bytes == 4 && t == ValType::I32 && !regs.is_empty() {
            args.push(regs.pop().unwrap());
            continue;
        }
        if t == ValType::I32 && bytes > 4 {
            args.push(Arg::ByVal(bytes));
        } else {
            args.push(Arg::Val(t));
        }
        stack += bytes;
    }
    if e.conv == spec::Conv::Varargs {
        if p.next().is_some() {
            args.push(Arg::Rest);
        }
    }
    for &t in p {
        // C parameters the .spec file doesn't list (it should): read them anyway
        args.push(Arg::Val(t));
    }
    let pop = match e.conv {
        spec::Conv::Stdcall | spec::Conv::Thiscall | spec::Conv::Fastcall => stack,
        _ => 0,
    };
    Ok((args, pop))
}

fn collect_call_indirect(l: &LocalFunction, out: &mut Vec<TypeId>) {
    let mut stack = vec![l.entry_block()];
    while let Some(id) = stack.pop() {
        for (instr, _) in &l.block(id).instrs {
            match instr {
                Instr::CallIndirect(c) => out.push(c.ty),
                Instr::Block(b) => stack.push(b.seq),
                Instr::Loop(b) => stack.push(b.seq),
                Instr::IfElse(b) => {
                    stack.push(b.consequent);
                    stack.push(b.alternative);
                }
                _ => {}
            }
        }
    }
}

fn rewrite_call_indirect(l: &mut LocalFunction, icalls: &BTreeMap<TypeId, FunctionId>) {
    let mut stack = vec![l.entry_block()];
    while let Some(id) = stack.pop() {
        let seq: &mut InstrSeq = l.block_mut(id);
        for (instr, _loc) in seq.instrs.iter_mut() {
            let _: &mut InstrLocId = _loc;
            match instr {
                Instr::CallIndirect(c) => {
                    *instr = Instr::Call(walrus::ir::Call { func: icalls[&c.ty] });
                }
                Instr::Block(b) => stack.push(b.seq),
                Instr::Loop(b) => stack.push(b.seq),
                Instr::IfElse(b) => {
                    stack.push(b.consequent);
                    stack.push(b.alternative);
                }
                _ => {}
            }
        }
    }
}

fn load_kind(t: ValType) -> LoadKind {
    match t {
        ValType::I32 => LoadKind::I32 { atomic: false },
        ValType::I64 => LoadKind::I64 { atomic: false },
        ValType::F32 => LoadKind::F32,
        ValType::F64 => LoadKind::F64,
        _ => LoadKind::V128,
    }
}

fn store_kind(t: ValType) -> StoreKind {
    match t {
        ValType::I32 => StoreKind::I32 { atomic: false },
        ValType::I64 => StoreKind::I64 { atomic: false },
        ValType::F32 => StoreKind::F32,
        ValType::F64 => StoreKind::F64,
        _ => StoreKind::V128,
    }
}

/// icall<ty>(params..., ptr): a wasm DLL function (ptr - BIAS below vn.limit) through the
/// table, anything else as x86 code with the parameters on the x86 stack.
#[allow(clippy::too_many_arguments)]
fn make_icall(
    m: &mut Module,
    ty: TypeId,
    table: TableId,
    memory: MemoryId,
    limit: GlobalId,
    sp: GlobalId,
    x86_call: FunctionId,
    x86_call_f: FunctionId,
) -> FunctionId {
    let t = m.types.get(ty);
    let (params, results) = (t.params().to_vec(), t.results().to_vec());
    let mut all = params.clone();
    all.push(ValType::I32);
    let mut b = FunctionBuilder::new(&mut m.types, &all, &results);
    b.name(format!("icall{}", ty.index()));
    let args: Vec<_> = all.iter().map(|&t| m.locals.add(t)).collect();
    let ptr = *args.last().unwrap();
    let idx = m.locals.add(ValType::I32);
    let argp = m.locals.add(ValType::I32);
    let nbytes: u32 = params.iter().map(|&t| size(t)).sum();
    let mut body = b.func_body();
    body.local_get(ptr).i32_const(BIAS as i32).binop(BinaryOp::I32Sub).local_tee(idx).global_get(limit).binop(BinaryOp::I32LtU);
    body.if_else(
        walrus::ir::InstrSeqType::Simple(None),
        |then| {
            for &l in &args[..args.len() - 1] {
                then.local_get(l);
            }
            then.local_get(idx).call_indirect(ty, table).return_();
        },
        |_| {},
    );
    // x86: arguments at sp - nbytes, as pushed right to left
    body.global_get(sp).i32_const(nbytes as i32).binop(BinaryOp::I32Sub).local_set(argp);
    let mut off = 0;
    for (i, &t) in params.iter().enumerate() {
        body.local_get(argp).local_get(args[i]).store(memory, store_kind(t), MemArg { align: 1, offset: off });
        off += size(t);
    }
    let float = matches!(results.first(), Some(ValType::F32 | ValType::F64));
    body.local_get(ptr).local_get(argp).call(if float { x86_call_f } else { x86_call });
    match results.first() {
        None => {
            body.drop();
        }
        Some(ValType::I32) => {
            body.unop(UnaryOp::I32WrapI64);
        }
        Some(ValType::I64) | Some(ValType::F64) => {}
        Some(ValType::F32) => {
            body.unop(UnaryOp::F32DemoteF64);
        }
        Some(_) => {
            body.unreachable();
        }
    }
    b.finish(args, &mut m.funcs)
}

/// __vn_in<k>(slot, argp, ecx, edx) -> i64 (EDX:EAX) or f64 (ST0): calls `slot` with the
/// arguments as `kind` says. Returns whether it returns f64.
fn make_entry(m: &mut Module, k: usize, kind: &EntryKind, table: TableId, memory: MemoryId) -> Result<bool> {
    let t = m.types.get(kind.ty);
    let results = t.results().to_vec();
    let float = matches!(results.first(), Some(ValType::F32 | ValType::F64));
    let ret = if float { ValType::F64 } else { ValType::I64 };
    let ps = [ValType::I32; 4];
    let mut b = FunctionBuilder::new(&mut m.types, &ps, &[ret]);
    let locals: Vec<_> = ps.iter().map(|&t| m.locals.add(t)).collect();
    let (slot, argp, ecx, edx) = (locals[0], locals[1], locals[2], locals[3]);
    let mut body = b.func_body();
    let mut off = 0;
    for a in &kind.args {
        match *a {
            Arg::Val(t) => {
                body.local_get(argp).load(memory, load_kind(t), MemArg { align: 1, offset: off });
                off += size(t);
            }
            Arg::ByVal(n) => {
                body.local_get(argp).i32_const(off as i32).binop(BinaryOp::I32Add);
                off += n;
            }
            Arg::Ecx => {
                body.local_get(ecx);
            }
            Arg::Edx => {
                body.local_get(edx);
            }
            Arg::Rest => {
                body.local_get(argp).i32_const(off as i32).binop(BinaryOp::I32Add);
            }
        }
    }
    body.local_get(slot).call_indirect(kind.ty, table);
    match results.first() {
        None => {
            body.i64_const(0);
        }
        Some(ValType::I32) => {
            body.unop(UnaryOp::I64ExtendUI32);
        }
        Some(ValType::I64) | Some(ValType::F64) => {}
        Some(ValType::F32) => {
            body.unop(UnaryOp::F64PromoteF32);
        }
        Some(t) => bail!("entry returning {t:?}"),
    }
    let f = b.finish(locals, &mut m.funcs);
    m.exports.add(&format!("__vn_in{k}"), f);
    Ok(float)
}
