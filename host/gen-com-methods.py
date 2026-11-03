#!/usr/bin/env python3
"""Writes crates/sake/src/com_methods.rs from Wine's d2d1, dwrite and wincodec IDL files: every
interface's whole vtable (inherited methods first) as (method name, stack argument dwords with
`this` and a hidden result pointer, result kind). sake's com.rs builds vtables from it.

usage: gen-com-methods.py WINE_INCLUDE_DIR"""
import os, re, sys

inc = sys.argv[1]
FILES = ['dcommon.idl', 'd2d1effects.idl', 'd2d1effects_1.idl', 'd2d1effects_2.idl', 'd2d1effectauthor.idl', 'd2d1.idl', 'd2d1_1.idl', 'd2d1_2.idl', 'd2d1_3.idl', 'dwrite.idl', 'dwrite_1.idl', 'dwrite_2.idl', 'dwrite_3.idl', 'wincodec.idl']
src = ''
for f in FILES:
    s = open(f'{inc}/{f}').read()
    s = re.sub(r'/\*.*?\*/', '', s, flags=re.S)
    s = re.sub(r'//[^\n]*', '', s)
    src += s + '\n'

# sizes of the types passed by value (bytes)
size = {'float': 4, 'FLOAT': 4, 'UINT32': 4, 'INT32': 4, 'UINT': 4, 'INT': 4, 'BOOL': 4, 'DWORD': 4, 'UINT16': 2,
        'INT16': 2, 'WCHAR': 2, 'BYTE': 1, 'UINT8': 1, 'UINT64': 8, 'INT64': 8, 'double': 8, 'HRESULT': 4, 'HWND': 4,
        'HDC': 4, 'HMONITOR': 4, 'D2D1_TAG': 8, 'LONG': 4, 'ULONG': 4, 'COLORREF': 4, 'FILETIME': 8, 'DXGI_FORMAT': 4,
        'D3DCOLORVALUE': 16, 'D2D_POINT_2F': 8, 'D2D_POINT_2U': 8, 'D2D_SIZE_F': 8, 'D2D_SIZE_U': 8, 'D2D_RECT_F': 16,
        'D2D_RECT_U': 16, 'D2D_MATRIX_3X2_F': 24, 'D2D_VECTOR_2F': 8, 'D2D_VECTOR_3F': 12, 'D2D_VECTOR_4F': 16,
        'D2D_MATRIX_4X4_F': 64, 'D2D_MATRIX_5X4_F': 80, 'D2D_MATRIX_4X3_F': 48, 'DXGI_ALPHA_MODE': 4,
        'D2D_RECT_L': 16, 'D2D_POINT_2L': 8, 'REFIID': 4, 'REFCLSID': 4, 'REFGUID': 4, 'HANDLE': 4, 'HMODULE': 4, 'LPCWSTR': 4, 'SIZE_T': 4, 'UINT_PTR': 4, 'D2D1_COLOR_F': 16, 'DXGI_COLOR_SPACE_TYPE': 4, 'D3D_FEATURE_LEVEL': 4, 'DXGI_RATIONAL': 8, 'POINT': 8, 'RECT': 16, 'LUID': 8, 'GUID': 16, 'ULARGE_INTEGER': 8, 'LARGE_INTEGER': 8, 'UINT64': 8, 'ULONGLONG': 8, 'LONGLONG': 8, 'INT64': 8, 'DOUBLE': 8, 'double': 8, 'ULONG_PTR': 4, 'HBITMAP': 4, 'HPALETTE': 4, 'HICON': 4}
aliases = {}
def names(s):
    return [n.strip() for n in s.split(',') if n.strip()]
for m in re.finditer(r'typedef\s+enum\s*(\w*)\s*\{.*?\}\s*([\w\s,]+);', src, re.S):
    for n in names(m.group(2)) + ([m.group(1)] if m.group(1) else []):
        size[n] = 4
for m in re.finditer(r'typedef\s+(\w+)\s+([\w\s,]+);', src):
    if m.group(1) in ('struct', 'enum', 'union'):
        continue
    for n in names(m.group(2)):
        aliases[n] = m.group(1)
structs = {}
for m in re.finditer(r'typedef\s+struct\s*(\w*)\s*\{(.*?)\}\s*([\w\s,]+);', src, re.S):
    for n in names(m.group(3)) + ([m.group(1)] if m.group(1) else []):
        structs[n] = m.group(2)


def sizeof(t):
    t = t.strip()
    while t in aliases and t not in size:
        t = aliases[t]
    if t in size:
        return size[t]
    if t in structs:
        n = 0
        for f in structs[t].split(';'):
            f = f.strip()
            if not f:
                continue
            m = re.match(r'(?:const\s+)?(\w+)\s*(\*?)\s*(\w+)\s*(?:\[(\w+)\])?', f)
            if not m:
                raise SystemExit(f'struct {t}: field {f!r}')
            ft, ptr, _, arr = m.groups()
            fs = 4 if ptr else sizeof(ft)
            n += fs * (int(arr) if arr and arr.isdigit() else 1)
        size[t] = n
        return n
    if re.match(r'L?P[A-Z0-9_]+$', t):
        return 4  # (pointer typedefs such as PD2D1_EFFECT_FACTORY)
    raise SystemExit(f'unknown type {t}')


def split_params(p):
    out, depth, cur = [], 0, ''
    for ch in p:
        if ch in '([':
            depth += 1
        elif ch in ')]':
            depth -= 1
        if ch == ',' and depth == 0:
            out.append(cur)
            cur = ''
        else:
            cur += ch
    if cur.strip():
        out.append(cur)
    return out


ifaces = {}
order = []
iids = {}
for m in re.finditer(r'uuid\(([0-9a-fA-F-]{36})\)[^\]]*\]\s*interface\s+(\w+)', src):
    iids[m.group(2)] = m.group(1).lower()
for m in re.finditer(r'interface\s+(\w+)\s*:\s*(\w+)\s*\{(.*?)\n\}', src, re.S):
    name, base, body = m.groups()
    meths = []
    for mm in re.finditer(r'([\w\s\*]+?)\s+(\w+)\s*\(([^;]*?)\)\s*;', body, re.S):
        ret, mname, params = mm.group(1).strip(), mm.group(2), mm.group(3)
        ret = re.sub(r'\[.*?\]', '', ret).strip()
        dwords = 1
        kind = 0
        if ret in ('float', 'FLOAT'):
            kind = 1
        elif ret not in ('HRESULT', 'void', 'BOOL', 'UINT32', 'UINT', 'INT32', 'ULONG') and '*' not in ret:
            r = ret
            while r in aliases:
                r = aliases[r]
            if r in size and r not in structs and size[r] == 4 and not r.startswith('D2D_') and r not in ('D3DCOLORVALUE',):
                kind = 0  # an enum
            else:
                kind = 2
                dwords += 1
        for p in split_params(params):
            p = re.sub(r'\[.*?\]', '', p).strip()
            if not p or p == 'void':
                continue
            p = p.replace('const ', '')
            if '*' in p or re.search(r'\[\s*\w*\s*\]$', p):
                dwords += 1
                continue
            t = p.rsplit(None, 1)[0]
            dwords += (sizeof(t) + 3) // 4
        meths.append((mname, dwords, kind))
    ifaces[name] = (base, meths)
    order.append(name)


# bases from objidl.idl / ocidl.idl
UNK = [('QueryInterface', 3, 0), ('AddRef', 1, 0), ('Release', 1, 0)]
SEQ = UNK + [('Read', 4, 0), ('Write', 4, 0)]
KNOWN = {
    'ISequentialStream': SEQ,
    'IStream': SEQ + [('Seek', 5, 0), ('SetSize', 3, 0), ('CopyTo', 6, 0), ('Commit', 2, 0), ('Revert', 1, 0),
                      ('LockRegion', 6, 0), ('UnlockRegion', 6, 0), ('Stat', 3, 0), ('Clone', 2, 0)],
    'IPersist': UNK + [('GetClassID', 2, 0)],
    'IPersistStream': UNK + [('GetClassID', 2, 0), ('IsDirty', 1, 0), ('Load', 2, 0), ('Save', 3, 0), ('GetSizeMax', 2, 0)],
}


def chain(name):
    if name in ifaces:
        return [name] + chain(ifaces[name][0])
    return [name]


IIDS = {'IUnknown': '00000000-0000-0000-c000-000000000046', 'IStream': '0000000c-0000-0000-c000-000000000046',
        'ISequentialStream': '0c733a30-2a1c-11ce-ade5-00aa0044773d', 'IPersist': '0000010c-0000-0000-c000-000000000046',
        'IPersistStream': '00000109-0000-0000-c000-000000000046'}


def guid(g):
    a, b, c, d, e = g.split('-')
    by = bytes.fromhex(d + e)
    return f'(0x{a}, 0x{b}, 0x{c}, [' + ', '.join(f'0x{x:02x}' for x in by) + '])'


def vtable(name):
    if name == 'IUnknown':
        return [('QueryInterface', 3, 0), ('AddRef', 1, 0), ('Release', 1, 0)]
    if name in KNOWN:
        return KNOWN[name]
    base, meths = ifaces[name]
    return vtable(base) + meths


out = ['//! generated by host/gen-com-methods.py from Wine\'s d2d1/dwrite/wincodec IDL: do not edit.',
       '//! Per interface, its whole vtable: (method, stack argument dwords with `this` and a hidden',
       '//! result pointer, result: 0 integer, 1 float, 2 a structure through that pointer), and the',
       '//! interface names it derives from (itself first), for QueryInterface.', '',
       'pub type Guid = (u32, u16, u16, [u8; 8]);',
       'pub const IFACES: &[(&str, &[(&str, u8, u8)], &[&str])] = &[']
for name in order:
    v = vtable(name)
    out.append(f'    ("{name}", &[' + ', '.join(f'("{n}", {a}, {k})' for n, a, k in v) + '], &['
               + ', '.join(f'"{b}"' for b in chain(name)) + ']),')
out.append('];')
IIDS.update(iids)
out.append('pub const IIDS: &[(&str, Guid)] = &[')
for n in sorted(IIDS):
    out.append(f'    ("{n}", {guid(IIDS[n])}),')
out.append('];')
here = os.path.dirname(os.path.abspath(__file__))
open(f'{here}/../crates/sake/src/com_methods.rs', 'w').write('\n'.join(out) + '\n')
print(len(order), 'interfaces')
