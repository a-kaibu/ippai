#!/usr/bin/env python3
"""The C layouts the apitrace crate packs replay arguments with, from mingw-w64's own headers
(clang's record layout dump, for i686 and x86_64): crates/apitrace/src/layouts.txt.

    tests/replay/layouts.py > crates/apitrace/src/layouts.txt     (the check: --check)

Every struct and union d3d8.h, d3d9.h, d3d11_4.h, dxgi1_6.h, ddraw.h and d3d.h (Direct3D 7) declare, every COM interface's
vtable (the method index is its slot), and the D3D entry points:

    R <name> <size32> <size64> <struct|union> <header set>   a record, then its members:
      <off32> <off64> <name> <type>        (name "-": an anonymous member)
    V <interface> <base>                 an interface ("-": no base), then the methods it adds to
                                         its base's, in vtable order:
      <index> <method> <return> <param>,<param>,...   (the object itself left out)
    F <function> <return> <param>,...    an entry point

Types: hr (a returned HRESULT), i1 i2 i4 i8 (integers and enums), f4 f8, v (void), p (a function pointer), S:<record>,
O:<interface>, H (HWND), *<type> (a pointer), [<n>]<type> (an array), b<width> (a bit field).
"""
import re
import subprocess
import sys

TUS = {
    "d3d9": ["d3d9.h"],
    "d3d8": ["d3d8.h"],
    "d3d11": ["d3d11_4.h", "dxgi1_6.h"],
    "ddraw": ["ddraw.h", "d3d.h"],
}
FUNCTIONS = {
    "d3d9": ["Direct3DCreate9", "Direct3DCreate9Ex"],
    "d3d8": ["Direct3DCreate8"],
    "d3d11": ["D3D11CreateDevice", "D3D11CreateDeviceAndSwapChain", "CreateDXGIFactory", "CreateDXGIFactory1", "CreateDXGIFactory2"],
    "ddraw": ["DirectDrawCreate", "DirectDrawCreateEx", "DirectDrawCreateClipper"],
}
INTS = {
    "char": "i1", "signed char": "i1", "unsigned char": "i1", "_Bool": "i1",
    "short": "i2", "unsigned short": "i2", "wchar_t": "i2",
    "int": "i4", "unsigned int": "i4", "long": "i4", "unsigned long": "i4",
    "long long": "i8", "unsigned long long": "i8",
    "float": "f4", "double": "f8", "long double": "f10", "void": "v",
}


def dump(tu, target):
    src = "#define COBJMACROS\n#define CINTERFACE\n" + "".join(f"#include <{h}>\n" for h in TUS[tu])
    for f in FUNCTIONS[tu]:
        src += f"struct FN_{f} {{ __typeof__(&{f}) {f}; }}; struct FN_{f} fn_{f};\n"
    r = subprocess.run(
        ["clang", "-target", target, "-fsyntax-only", "-x", "c", "-", "-Xclang", "-fdump-record-layouts-complete", "-Xclang", "-fdump-record-layouts-canonical"],
        input=src, capture_output=True, text=True)
    if r.returncode:
        sys.exit(r.stderr)
    return r.stdout


ANON = re.compile(r"\((?:anonymous|unnamed) at [^)]*?:(\d+):(\d+)\)")


def record_name(s):
    # "union D3D11_DEPTH_STENCIL_VIEW_DESC::(anonymous at .../d3d11.h:1824:20)" -> "...::@1824_20"
    return ANON.sub(lambda m: f"@{m.group(1)}_{m.group(2)}", s.strip())


def split_params(s):
    out, depth, cur = [], 0, ""
    for c in s:
        if c == "," and depth == 0:
            out.append(cur)
            cur = ""
            continue
        depth += c in "(["
        depth -= c in ")]"
        cur += c
    if cur.strip():
        out.append(cur)
    return [p.strip() for p in out]


def ty(s, interfaces, pre=""):
    s = s.replace("__attribute__((stdcall))", "").replace("volatile", "")
    s = re.sub(r"\bconst\b", "", s)
    s = " ".join(s.split())
    if "(*)" in s:
        return "p"
    m = re.fullmatch(r"(.*)\[(\d+)\]", s)
    if m:
        return f"[{m.group(2)}]" + ty(m.group(1), interfaces, pre)
    if s.endswith("*"):
        t = ty(s[:-1], interfaces, pre)
        return "H" if t == "S:HWND__" else "*" + t
    m = re.fullmatch(r"(struct|union|enum) (.*)", s)
    if m:
        kind, name = m.group(1), record_name(m.group(2))
        if kind == "enum":
            return "i4"
        return "O:" + name if name in interfaces else "S:" + (pre if name != "HWND__" else "") + name
    if s in INTS:
        return INTS[s]
    # (a struct or an enum declared "typedef struct { ... } NAME": clang names it by the
    # typedef, and a method's parameter says NAME alone)
    if re.fullmatch(r"[A-Z_][A-Z_0-9]*", s):
        return "O:" + s if s in interfaces else "S:" + pre + s if s in NAMED else "i4"
    raise SystemExit(f"layouts.py: type {s!r}")


# the records of the headers (by their names in the dump)
NAMED = set()

LINE = re.compile(r"^\s*(\d+)(?::(\d+)-(\d+))? \| (\s*)(.*)$")
SIZE = re.compile(r"\[sizeof=(\d+), (?:dsize=\d+, )?align=(\d+)")


def records(text):
    """{name: (size, [(offset, member type text, member name, bits)], "struct" / "union")}"""
    out = {}
    for block in text.split("*** Dumping AST Record Layout")[1:]:
        lines = [l for l in block.split("\n") if l.strip()]
        head = LINE.match(lines[0])
        kind = head.group(5).split()[0]
        name = record_name(re.sub(r"^(struct|union) ", "", head.group(5)))
        members = []
        size = None
        for l in lines[1:]:
            m = SIZE.search(l)
            if m:
                size = int(m.group(1))
                break
            m = LINE.match(l)
            if not m:
                continue
            depth = len(m.group(4)) // 2
            if depth != 1:
                continue
            t = m.group(5)
            bits = None
            if m.group(2) is not None:
                bits = int(m.group(3)) - int(m.group(2)) + 1
            # "type name" (an anonymous member: the type alone, ending in its record's name)
            mm = re.fullmatch(r"(.*?)\s*([A-Za-z_][A-Za-z_0-9]*)", t)
            if t.rstrip().endswith(")") or (mm and mm.group(1) in ("struct", "union", "")):
                members.append((int(m.group(1)), t.strip(), "-", bits))
            else:
                members.append((int(m.group(1)), mm.group(1), mm.group(2), bits))
        out.setdefault(name, (size, members, kind))
    return out


def headers(tu):
    src = "".join(f"#include <{h}>\n" for h in TUS[tu])
    r = subprocess.run(["clang", "-target", "i686-w64-mingw32", "-M", "-x", "c", "-"], input=src, capture_output=True, text=True)
    return [f for f in r.stdout.replace("\\\n", " ").split()[1:] if f.endswith(".h")]


def main():
    recs, vtbls, fns, bases = {}, {}, {}, {"IUnknown": None}
    for tu in TUS:
        for h in headers(tu):
            t = open(h, encoding="latin-1").read()
            for i, b in re.findall(r"DECLARE_INTERFACE_\(\s*(\w+)\s*,\s*(\w+)\s*\)", t) + re.findall(r"^\s*(?:interface\s+)?(\w+)\s*:\s*public\s+(\w+)", t, re.M):
                bases.setdefault(i, b)
        r32, r64 = records(dump(tu, "i686-w64-mingw32")), records(dump(tu, "x86_64-w64-mingw32"))
        NAMED.update(r32)
        interfaces = {n[:-4] for n in r32 if n.endswith("Vtbl")}
        pre = {"d3d8": "8.", "ddraw": "7."}.get(tu, "")
        for name, (size, members, kind) in r32.items():
            if name.startswith("FN_") or name.endswith("Vtbl") or name in interfaces or pre + name in recs:
                continue
            size64, members64, _ = r64.get(name, (None, None, None))
            if members64 is None or [m[2] for m in members] != [m[2] for m in members64]:
                recs[pre + name] = None  # (members differ between the two: x86/x64-only structs of winnt.h)
                continue
            # (types made when the record is wanted: winnt.h's have some this does not know)
            ms = [(o32, o64, n, f"b{bits}" if bits else (t, interfaces, pre)) for (o32, t, n, bits), (o64, _, _, _) in zip(members, members64)]
            recs[pre + name] = (size, size64, ms, kind, tu)
        for name, (size, members, _) in r32.items():
            if not name.endswith("Vtbl") or name[:-4] in vtbls or not re.match(r"(IDirect3D|IDirectDraw|ID3D11|IDXGI|IUnknownVtbl)", name):
                continue
            ms = []
            for i, (o32, t, n, _) in enumerate(members):
                assert o32 == 4 * i, (name, n)
                m = re.fullmatch(r"(.*?)\(\*\)\((.*)\)\s*", t.replace("__attribute__((stdcall))", "").strip())
                ret = "hr" if m.group(1).strip() == "long" else ty(m.group(1), interfaces, pre)
                ms.append((i, n, ret, [ty(p, interfaces, pre) for p in split_params(m.group(2))[1:] if p != "void"]))
            vtbls[name[:-4]] = ms
        for f in FUNCTIONS[tu]:
            t = r32[f"FN_{f}"][1][0][1]
            m = re.fullmatch(r"(.*?)\(\*\)\((.*)\)\s*", t.replace("__attribute__((stdcall))", "").strip())
            fns[f] = ("hr" if m.group(1).strip() == "long" else ty(m.group(1), interfaces, pre), [ty(p, interfaces, pre) for p in split_params(m.group(2)) if p != "void"])
    # only the records the methods and functions reach
    want, todo = set(), []
    def reach(t):
        for r in re.findall(r"S:([^ ,]+)", t):
            if r not in want:
                want.add(r)
                todo.append(r)
    for ms in vtbls.values():
        for _, _, ret, ps in ms:
            reach(" ".join([ret] + ps))
    for ret, ps in fns.values():
        reach(" ".join([ret] + ps))
    while todo:
        r = todo.pop()
        if not recs.get(r):
            sys.exit(f"layouts.py: no layout of {r}")
        size, size64, ms, kind, tu = recs[r]
        ms = [(o32, o64, n, t if isinstance(t, str) else ty(*t)) for o32, o64, n, t in ms]
        recs[r] = (size, size64, ms, kind, tu)
        for m in ms:
            reach(m[3])
    lines = []
    for name in sorted(want):
        size, size64, ms, kind, tu = recs[name]
        lines.append(f"R {name} {size} {size64} {kind} {tu}")
        lines += [f" {o32} {o64} {n} {t}" for o32, o64, n, t in ms]
    # (an interface lists its own methods after its base's, the base the headers name)
    sig = {n: [m for _, m, _, _ in ms] for n, ms in vtbls.items()}
    for name in sorted(vtbls):
        base = bases.get(name)
        if name != "IUnknown" and (base not in sig or sig[name][:len(sig[base])] != sig[base]):
            sys.exit(f"layouts.py: {name}'s methods do not begin with its base's ({base})")
        lines.append(f"V {name} {base or '-'}")
        lines += [f" {i} {n} {ret} " + ",".join(ps) for i, n, ret, ps in vtbls[name][len(sig[base]) if base else 0:]]
    for f in sorted(fns):
        lines.append(f"F {f} {fns[f][0]} " + ",".join(fns[f][1]))
    text = "\n".join(lines) + "\n"
    if sys.argv[1:] == ["--check"]:
        path = __file__.rsplit("/", 3)[0] + "/crates/apitrace/src/layouts.txt"
        if open(path).read() != text:
            sys.exit("crates/apitrace/src/layouts.txt is not what tests/replay/layouts.py makes: run it again")
        print("layouts.txt: current")
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
