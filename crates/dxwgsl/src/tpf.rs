//! Shader model 4/5 bytecode (the "tokenized program format" of DXBC's SHDR/SHEX chunk),
//! decoded into instructions with their operands. wgsl.rs writes WGSL of them.

/// Program types in the version token
pub const PIXEL: u32 = 0;
pub const VERTEX: u32 = 1;

// operand types
pub const TEMP: u32 = 0;
pub const INPUT: u32 = 1;
pub const OUTPUT: u32 = 2;
pub const INDEXABLE_TEMP: u32 = 3;
pub const IMM32: u32 = 4;
pub const IMM64: u32 = 5;
pub const SAMPLER: u32 = 6;
pub const RESOURCE: u32 = 7;
pub const CONSTANT_BUFFER: u32 = 8;
pub const IMM_CONSTANT_BUFFER: u32 = 9;
pub const OUTPUT_DEPTH: u32 = 12;
pub const NULL: u32 = 13;
pub const OUTPUT_COVERAGE_MASK: u32 = 15;
pub const OUTPUT_DEPTH_GE: u32 = 38;
pub const OUTPUT_DEPTH_LE: u32 = 39;

#[derive(Clone, Debug, PartialEq)]
pub enum Sel {
    /// write mask (destinations, declarations)
    Mask(u8),
    Swizzle([u8; 4]),
    /// one component
    Scalar(u8),
    /// a scalar or component-less operand
    None,
}

#[derive(Clone, Debug)]
pub enum Index {
    Imm(u32),
    /// a register's value plus an offset
    Rel(Box<Operand>, u32),
}

#[derive(Clone, Debug)]
pub struct Operand {
    pub ty: u32,
    /// 0, 1 or 4
    pub comps: u8,
    pub sel: Sel,
    pub idx: Vec<Index>,
    /// immediate values (IMM32)
    pub imm: [u32; 4],
    /// 0 none, 1 negate, 2 absolute value, 3 both
    pub modifier: u8,
}

impl Operand {
    /// The register number (first immediate index).
    pub fn reg(&self) -> u32 {
        match self.idx.first() {
            Some(Index::Imm(i)) => *i,
            _ => 0,
        }
    }
    /// The write mask (destinations): components 0-3.
    pub fn mask(&self) -> u8 {
        match self.sel {
            Sel::Mask(m) => m,
            Sel::Scalar(c) => 1 << c,
            _ => if self.comps == 1 { 1 } else { 0xf },
        }
    }
    /// The swizzle (sources): source component for each of x, y, z, w.
    pub fn swizzle(&self) -> [u8; 4] {
        match self.sel {
            Sel::Swizzle(s) => s,
            Sel::Scalar(c) => [c; 4],
            _ => [0, 1, 2, 3],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Inst {
    pub op: u32,
    /// the opcode token's bits 11-23 (opcode-specific controls)
    pub ctl: u32,
    pub sat: bool,
    /// for if/breakc/continuec/retc/discard: true when the test is "nonzero"
    pub nz: bool,
    /// immediate texel offsets (aoffimmi)
    pub offset: [i32; 3],
    pub ops: Vec<Operand>,
    /// declaration words after the operands (counts, return types, names...)
    pub extra: Vec<u32>,
    /// immediate constant buffer data
    pub data: Vec<u32>,
}

pub struct Program {
    pub kind: u32,
    pub major: u32,
    pub insts: Vec<Inst>,
}

// opcodes the decoder treats specially
pub const OP_CUSTOMDATA: u32 = 0x35;
pub const OP_DCL_RESOURCE: u32 = 0x58;
pub const OP_DCL_CONSTANT_BUFFER: u32 = 0x59;
pub const OP_DCL_SAMPLER: u32 = 0x5a;
pub const OP_DCL_INDEX_RANGE: u32 = 0x5b;
pub const OP_DCL_INPUT: u32 = 0x5f;
pub const OP_DCL_INPUT_SGV: u32 = 0x60;
pub const OP_DCL_INPUT_SIV: u32 = 0x61;
pub const OP_DCL_INPUT_PS: u32 = 0x62;
pub const OP_DCL_INPUT_PS_SGV: u32 = 0x63;
pub const OP_DCL_INPUT_PS_SIV: u32 = 0x64;
pub const OP_DCL_OUTPUT: u32 = 0x65;
pub const OP_DCL_OUTPUT_SGV: u32 = 0x66;
pub const OP_DCL_OUTPUT_SIV: u32 = 0x67;
pub const OP_DCL_TEMPS: u32 = 0x68;
pub const OP_DCL_INDEXABLE_TEMP: u32 = 0x69;
pub const OP_DCL_GLOBAL_FLAGS: u32 = 0x6a;
pub const OP_DCL_RESOURCE_RAW: u32 = 0xa1;
pub const OP_DCL_RESOURCE_STRUCTURED: u32 = 0xa2;

/// The operand count of each instruction (d3d11TokenizedProgramFormat.hpp; declarations and
/// the hull/geometry shader instructions left out): a program that gives another count is
/// rejected.
pub fn arity(op: u32) -> Option<usize> {
    Some(match op {
        0x02 | 0x07 | 0x09 | 0x0a | 0x12 | 0x13 | 0x14 | 0x15 | 0x16 | 0x17 | 0x30 | 0x3a | 0x3e => 0,
        0x03 | 0x04 | 0x06 | 0x08 | 0x0d | 0x1f | 0x2c | 0x3f | 0x4c => 1,
        0x05 | 0x0b | 0x0c | 0x19 | 0x1a | 0x1b | 0x1c | 0x28 | 0x2b | 0x2f | 0x36 | 0x3b | 0x40..=0x44 | 0x4b | 0x56 | 0x6f | 0x79..=0x7d | 0x81..=0x83
        | 0x86..=0x89 | 0x8d => 2,
        0x00 | 0x01 | 0x0e | 0x0f | 0x10 | 0x11 | 0x18 | 0x1d | 0x1e | 0x20 | 0x21 | 0x22 | 0x24 | 0x25 | 0x27 | 0x29 | 0x2a | 0x2d | 0x31 | 0x33 | 0x34
        | 0x38 | 0x39 | 0x3c | 0x3d | 0x4d | 0x4f | 0x50 | 0x53 | 0x54 | 0x55 | 0x57 | 0x6e => 3,
        0x23 | 0x26 | 0x2e | 0x32 | 0x37 | 0x45 | 0x4e | 0x51 | 0x52 | 0x6c | 0x6d | 0x84 | 0x85 | 0x8a | 0x8b => 4,
        0x46 | 0x47 | 0x48 | 0x4a | 0x7e | 0x7f | 0x8c | 0x8e => 5,
        0x49 | 0x80 => 6,
        _ => return None,
    })
}

struct Reader<'a> {
    w: &'a [u32],
    p: usize,
    end: usize,
}

impl Reader<'_> {
    fn next(&mut self) -> Result<u32, String> {
        if self.p >= self.end {
            return Err("truncated instruction".into());
        }
        self.p += 1;
        Ok(self.w[self.p - 1])
    }

    fn operand(&mut self) -> Result<Operand, String> {
        let t = self.next()?;
        let num = t & 3;
        let comps = [0u8, 1, 4, 4][num as usize];
        let sel = if num == 2 {
            match (t >> 2) & 3 {
                0 => Sel::Mask(((t >> 4) & 0xf) as u8),
                1 => Sel::Swizzle([((t >> 4) & 3) as u8, ((t >> 6) & 3) as u8, ((t >> 8) & 3) as u8, ((t >> 10) & 3) as u8]),
                _ => Sel::Scalar(((t >> 4) & 3) as u8),
            }
        } else {
            Sel::None
        };
        if num == 3 {
            return Err("N-component operand".into());
        }
        let ty = (t >> 12) & 0xff;
        let dims = (t >> 20) & 3;
        let mut modifier = 0;
        if t >> 31 != 0 {
            let mut e = self.next()?;
            loop {
                if e & 0x3f == 1 {
                    modifier = ((e >> 6) & 0xff) as u8;
                }
                if e >> 31 == 0 {
                    break;
                }
                e = self.next()?;
            }
        }
        let mut imm = [0u32; 4];
        match ty {
            IMM32 => {
                for v in imm.iter_mut().take(if comps == 4 { 4 } else { 1 }) {
                    *v = self.next()?;
                }
                if comps == 1 {
                    imm = [imm[0]; 4];
                }
            }
            IMM64 => return Err("64-bit immediates".into()),
            _ => {}
        }
        let mut idx = Vec::new();
        for i in 0..dims {
            let rep = (t >> (22 + 3 * i)) & 7;
            idx.push(match rep {
                0 => Index::Imm(self.next()?),
                1 => {
                    let lo = self.next()?;
                    self.next()?;
                    Index::Imm(lo)
                }
                2 => Index::Rel(Box::new(self.operand()?), 0),
                3 => {
                    let off = self.next()?;
                    Index::Rel(Box::new(self.operand()?), off)
                }
                _ => return Err(format!("index representation {rep}")),
            });
        }
        Ok(Operand { ty, comps, sel, idx, imm, modifier })
    }
}

/// Decode SHDR/SHEX.
pub fn decode(chunk: &[u8]) -> Result<Program, String> {
    let w: Vec<u32> = chunk.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    if w.len() < 2 {
        return Err("empty shader".into());
    }
    let kind = w[0] >> 16;
    let major = (w[0] >> 4) & 0xf;
    let len = (w[1] as usize).min(w.len());
    let mut insts = Vec::new();
    let mut p = 2;
    while p < len {
        let t = w[p];
        let op = t & 0x7ff;
        if op == OP_CUSTOMDATA {
            let n = *w.get(p + 1).ok_or("truncated customdata")? as usize;
            if n < 2 || p + n > len {
                return Err("bad customdata".into());
            }
            insts.push(Inst {
                op,
                ctl: t >> 11,
                sat: false,
                nz: false,
                offset: [0; 3],
                ops: Vec::new(),
                extra: Vec::new(),
                data: w[p + 2..p + n].to_vec(),
            });
            p += n;
            continue;
        }
        let n = ((t >> 24) & 0x1f) as usize;
        if n == 0 || p + n > len {
            return Err(format!("bad instruction length at {p}"));
        }
        let mut r = Reader { w: &w, p: p + 1, end: p + n };
        let mut inst = Inst {
            op,
            ctl: (t >> 11) & 0x1fff,
            sat: (t >> 13) & 1 != 0,
            nz: (t >> 18) & 1 != 0,
            offset: [0; 3],
            ops: Vec::new(),
            extra: Vec::new(),
            data: Vec::new(),
        };
        if t >> 31 != 0 {
            loop {
                let e = r.next()?;
                if e & 0x3f == 1 {
                    let s4 = |v: u32| ((v as i32) << 28) >> 28;
                    inst.offset = [s4(e >> 9), s4(e >> 13), s4(e >> 17)];
                }
                if e >> 31 == 0 {
                    break;
                }
            }
        }
        // declarations that are not just operands
        let (operands, words) = match op {
            OP_DCL_TEMPS | OP_DCL_GLOBAL_FLAGS => (0, true),
            OP_DCL_INDEXABLE_TEMP => (0, true),
            0x5c..=0x5e | 0x93..=0x9b | 0x8f | 0x90..=0x92 => (0, true),
            OP_DCL_RESOURCE | OP_DCL_CONSTANT_BUFFER | OP_DCL_SAMPLER | OP_DCL_INDEX_RANGE | OP_DCL_INPUT..=OP_DCL_OUTPUT_SIV
            | OP_DCL_RESOURCE_RAW | OP_DCL_RESOURCE_STRUCTURED | 0x9c..=0xa0 => (1, true),
            _ => (usize::MAX, false),
        };
        let mut k = 0;
        while r.p < r.end && k < operands {
            inst.ops.push(r.operand()?);
            k += 1;
        }
        if words {
            while r.p < r.end {
                inst.extra.push(r.next()?);
            }
        }
        if operands == 1 && inst.ops.len() != 1 {
            return Err(format!("declaration {op:#x} at {p} without its operand"));
        }
        if let Some(k) = arity(op) {
            if inst.ops.len() != k || r.p != r.end {
                return Err(format!("instruction {op:#x} at {p}: {} operands, {k} wanted", inst.ops.len()));
            }
        }
        p += n;
        insts.push(inst);
    }
    Ok(Program { kind, major, insts })
}
