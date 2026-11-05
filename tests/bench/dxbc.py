#!/usr/bin/env python3
"""Shader model 4 bytecode (DXBC) for tests/bench/src/d3d11.c, assembled here by hand: sake has no
HLSL compiler (games ship compiled shaders) and the CI has no fxc. Writes src/d3d11_shaders.h.

    python3 tests/bench/dxbc.py

Check what sake makes of them: VNVM_DXBC_DIR=<dir with the .dxbc files this writes with --dump>
VNVM_DXBC_PRINT=1 cargo test -p kura --release corpus -- --nocapture"""
import os
import struct
import sys

# operand types, opcodes (d3d10tokenizedprogramformat.hpp)
TEMP, INPUT, OUTPUT, IMM32, SAMPLER, RESOURCE, CB = 0, 1, 2, 4, 6, 7, 8
ADD, MAD, MOV, MUL, RET, SAMPLE = 0x00, 0x32, 0x36, 0x38, 0x3E, 0x45
DCL_RESOURCE, DCL_CB, DCL_SAMPLER, DCL_INPUT, DCL_INPUT_PS = 0x58, 0x59, 0x5A, 0x5F, 0x62
DCL_OUTPUT, DCL_OUTPUT_SIV, DCL_TEMPS = 0x65, 0x67, 0x68
XYZW = 0xE4


def swz(s):
    return sum("xyzw".index(c) << (2 * i) for i, c in enumerate(s.ljust(4, s[-1])))


def mask(s):
    return sum(1 << "xyzw".index(c) for c in s)


def reg(ty, *idx, m=None, s=None, neg=False, comps=4):
    """an operand: m = write mask ("xy"), s = swizzle ("xyxx"), idx = immediate indices"""
    t = {0: 0, 1: 1, 4: 2}[comps]
    if comps == 4:
        t |= (mask(m) << 4) if m is not None else (1 << 2) | (swz(s or "xyzw") << 4)
    t |= ty << 12 | len(idx) << 20
    if neg:
        return [t | 1 << 31, 1 | 1 << 6, *idx]
    return [t, *idx]


def imm(*v):
    return [2 | IMM32 << 12, *(struct.unpack("<I", struct.pack("<f", x))[0] for x in v)]


def inst(op, *operands, ctl=0):
    body = [w for o in operands for w in o]
    return [op | ctl << 11 | (1 + len(body)) << 24, *body]


def sig(elems):
    """ISGN/OSGN: (name, sysval, register, mask, rw)"""
    head = struct.pack("<II", len(elems), 8)
    names, offs = b"", []
    base = 8 + 24 * len(elems)
    for e in elems:
        offs.append(base + len(names))
        names += e[0].encode() + b"\0"
    while len(names) % 4:
        names += b"\xab"
    body = b"".join(struct.pack("<IIIIIBBH", off, 0, e[1], 3, e[2], e[3], e[4], 0) for off, e in zip(offs, elems))
    return head + body + names


def container(kind, isgn, osgn, code):
    shdr = [(kind << 16) | 0x40, 0] + code
    shdr[1] = len(shdr)
    chunks = [(b"ISGN", sig(isgn)), (b"OSGN", sig(osgn)), (b"SHDR", struct.pack(f"<{len(shdr)}I", *shdr))]
    off = 32 + 4 * len(chunks)
    offsets, blob = [], b""
    for tag, data in chunks:
        offsets.append(off + len(blob))
        blob += tag + struct.pack("<I", len(data)) + data
    total = off + len(blob)
    # (the checksum is left zero: sake does not check it)
    return b"DXBC" + bytes(16) + struct.pack("<III", 1, total, len(chunks)) + struct.pack(f"<{len(chunks)}I", *offsets) + blob


VS_IN = [("POSITION", 0, 0, 0x3, 0x3), ("TEXCOORD", 0, 1, 0x3, 0x3), ("COLOR", 0, 2, 0xF, 0xF)]
VS_OUT = [("SV_Position", 1, 0, 0xF, 0), ("TEXCOORD", 0, 1, 0x3, 0xC), ("COLOR", 0, 2, 0xF, 0)]
PS_IN = [("SV_Position", 1, 0, 0xF, 0), ("TEXCOORD", 0, 1, 0x3, 0x3), ("COLOR", 0, 2, 0xF, 0xF)]
PS_OUT = [("SV_Target", 0, 0, 0xF, 0)]
LINEAR = 2


def vs_quad():
    """o0 = (v0 * cb0[0].zw + cb0[0].xy, 0.5, 1): a unit quad placed by the constant buffer"""
    return container(1, VS_IN, VS_OUT, [
        *inst(DCL_CB, reg(CB, 0, 1, s="xyzw")),
        *inst(DCL_INPUT, reg(INPUT, 0, m="xy")),
        *inst(DCL_INPUT, reg(INPUT, 1, m="xy")),
        *inst(DCL_INPUT, reg(INPUT, 2, m="xyzw")),
        *inst(DCL_OUTPUT_SIV, reg(OUTPUT, 0, m="xyzw"), [1]),
        *inst(DCL_OUTPUT, reg(OUTPUT, 1, m="xy")),
        *inst(DCL_OUTPUT, reg(OUTPUT, 2, m="xyzw")),
        *inst(MAD, reg(OUTPUT, 0, m="xy"), reg(INPUT, 0, s="xyxx"), reg(CB, 0, 0, s="zwzz"), reg(CB, 0, 0, s="xyxx")),
        *inst(MOV, reg(OUTPUT, 0, m="zw"), imm(0, 0, 0.5, 1)),
        *inst(MOV, reg(OUTPUT, 1, m="xy"), reg(INPUT, 1, s="xyxx")),
        *inst(MOV, reg(OUTPUT, 2, m="xyzw"), reg(INPUT, 2)),
        *inst(RET),
    ])


def ps_common(textures, cb, temps):
    code = []
    if cb:
        code += inst(DCL_CB, reg(CB, 0, 1, s="xyzw"))
    code += inst(DCL_SAMPLER, reg(SAMPLER, 0, comps=0))
    for t in range(textures):
        code += inst(DCL_RESOURCE, reg(RESOURCE, t, comps=0), [0x5555], ctl=3)  # texture2d, float x4
    code += inst(DCL_INPUT_PS, reg(INPUT, 1, m="xy"), ctl=LINEAR)
    code += inst(DCL_INPUT_PS, reg(INPUT, 2, m="xyzw"), ctl=LINEAR)
    code += inst(DCL_OUTPUT, reg(OUTPUT, 0, m="xyzw"))
    code += inst(DCL_TEMPS, [temps])
    return code


def sample(dst, coord, t):
    return inst(SAMPLE, reg(TEMP, dst, m="xyzw"), coord, reg(RESOURCE, t), reg(SAMPLER, 0, comps=0))


def ps_sprite():
    """texture x vertex color"""
    return container(0, PS_IN, PS_OUT, [
        *ps_common(1, False, 1),
        *sample(0, reg(INPUT, 1, s="xyxx"), 0),
        *inst(MUL, reg(OUTPUT, 0, m="xyzw"), reg(TEMP, 0), reg(INPUT, 2)),
        *inst(RET),
    ])


def ps_fade():
    """lerp(t0, t1, cb0[0].x)"""
    return container(0, PS_IN, PS_OUT, [
        *ps_common(2, True, 2),
        *sample(0, reg(INPUT, 1, s="xyxx"), 0),
        *sample(1, reg(INPUT, 1, s="xyxx"), 1),
        *inst(ADD, reg(TEMP, 1, m="xyzw"), reg(TEMP, 1), reg(TEMP, 0, neg=True)),
        *inst(MAD, reg(OUTPUT, 0, m="xyzw"), reg(TEMP, 1), reg(CB, 0, 0, s="xxxx"), reg(TEMP, 0)),
        *inst(RET),
    ])


def ps_blur():
    """(t0(uv + cb0[0].xy) + t0(uv - cb0[0].xy)) / 2"""
    return container(0, PS_IN, PS_OUT, [
        *ps_common(1, True, 2),
        *inst(ADD, reg(TEMP, 0, m="xy"), reg(INPUT, 1, s="xyxx"), reg(CB, 0, 0, s="xyxx")),
        *inst(ADD, reg(TEMP, 1, m="xy"), reg(INPUT, 1, s="xyxx"), reg(CB, 0, 0, s="xyxx", neg=True)),
        *sample(0, reg(TEMP, 0, s="xyxx"), 0),
        *sample(1, reg(TEMP, 1, s="xyxx"), 0),
        *inst(ADD, reg(TEMP, 0, m="xyzw"), reg(TEMP, 0), reg(TEMP, 1)),
        *inst(MUL, reg(OUTPUT, 0, m="xyzw"), reg(TEMP, 0), imm(0.5, 0.5, 0.5, 0.5)),
        *inst(RET),
    ])


SHADERS = {"vs_quad": vs_quad(), "ps_sprite": ps_sprite(), "ps_fade": ps_fade(), "ps_blur": ps_blur()}

if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    if len(sys.argv) > 2 and sys.argv[1] == "--dump":
        for name, code in SHADERS.items():
            open(os.path.join(sys.argv[2], f"{name}.dxbc"), "wb").write(code)
        sys.exit()
    out = ["/* Generated by tests/bench/dxbc.py: shader model 4 bytecode for d3d11.c. */"]
    for name, code in SHADERS.items():
        out.append(f"static const unsigned char {name}[{len(code)}] = {{")
        for i in range(0, len(code), 16):
            out.append("    " + ", ".join(f"0x{b:02x}" for b in code[i:i + 16]) + ",")
        out.append("};")
    open(os.path.join(here, "src", "d3d11_shaders.h"), "w").write("\n".join(out) + "\n")
