#!/usr/bin/env python3
"""MFC 4.2's ordinal -> name table (out/mfc42.tsv) for sake's own MFC42.DLL (crates/sake/src/mfc42.rs).

MFC42.DLL exports by ordinal only, and the VC6 import library that names them is long gone. The
DLL Windows still ships (6.06.8063.0, System32 / SysWOW64) and its public PDB are on Microsoft's
symbol server: the PDB's public symbols at each export's address name it. Overloads the linker
folded into one body (identical code) share an address; the radare2 table (undecorated names by
ordinal, libr/bin/d/dll/mfc42.sdb.txt) picks among them.

  mfc42.py DIR    DIR: work directory (mfc42.dll, mfc42.pdb, publics.txt, sdb cached there)
                  writes out/mfc42.tsv: ordinal, decorated name, conv, argument bytes (with this)

Needs pefile (pip), llvm-pdbutil and llvm-undname. The argument bytes come from the decorated
names (sizes of the by-value classes below) and are checked against the `ret N` that ends each
export's code where that is found.
"""
import collections, os, re, subprocess, sys, urllib.request

SYMSRV = 'https://msdl.microsoft.com/download/symbols'
# mfc42.dll 6.06.8063.0 (x86): TimeDateStamp + SizeOfImage, the symbol server's key
DLL_KEY = '5BD1349712a000'
SDB = 'https://raw.githubusercontent.com/radareorg/radare2/master/libr/bin/d/dll/mfc42.sdb.txt'
# by-value argument sizes (x86; MFC's headers pack to 4: COleDateTime is 12 bytes)
BYVAL = {'CPoint': 8, 'CSize': 8, 'CRect': 16, 'tagPOINT': 8, 'tagSIZE': 8, 'tagRECT': 16, 'CString': 4,
         'CTime': 4, 'CTimeSpan': 4, 'COleDateTime': 12, 'COleDateTimeSpan': 12, 'COleCurrency': 12,
         'COleVariant': 16, 'tagVARIANT': 16, '_GUID': 16, 'tagCY': 8, '_FILETIME': 8, '_SYSTEMTIME': 16,
         'tagDEC': 16, '_ULARGE_INTEGER': 8, '_LARGE_INTEGER': 8, 'CComBSTR': 4, 'tagSAFEARRAY': 24, '_POINTL': 8, '_charformat': 60, 'tagFONTOBJECT': 32}


UNKNOWN = set()


def fetch(url, path):
    if not os.path.exists(path):
        with urllib.request.urlopen(url) as r, open(path + '.part', 'wb') as f:
            f.write(r.read())
        os.rename(path + '.part', path)
    return path


def split_args(s):
    out, depth, cur = [], 0, ''
    for ch in s:
        if ch in '(<': depth += 1
        if ch in ')>': depth -= 1
        if ch == ',' and depth == 0:
            out.append(cur.strip()); cur = ''
        else:
            cur += ch
    if cur.strip(): out.append(cur.strip())
    return out


def size_of(t):
    t = t.strip()
    if t in ('void', '...', ''): return 0
    if t.endswith('*') or t.endswith('&') or '(' in t or t.endswith('const') and ('*' in t or '&' in t): return 4
    t = re.sub(r'\b(const|volatile)\b', '', t).strip()
    if t in ('double', '__int64', 'unsigned __int64'): return 8
    m = re.match(r'(?:class|struct|union) (\w+)$', t)
    if m:
        if m.group(1) not in BYVAL: UNKNOWN.add(m.group(1)); return 4
        return BYVAL[m.group(1)]
    return 4


def conv_bytes(dec, und):
    """(conv, argument bytes with `this`) from the undecorated name"""
    if '(' not in und:
        return 'data', 0
    # (the function's own convention: the one before its name, not a function pointer's "(__cdecl *)")
    m = re.search(r'__(cdecl|stdcall|thiscall|fastcall) (?![*&)])', und)
    conv = m.group(1) if m else 'cdecl'
    # the outermost parameter list: the last top-level (...) of the function
    depth, start, end = 0, None, None
    name_end = und.find(m.group(0)) + len(m.group(0)) if m else 0
    for i in range(name_end, len(und)):
        if und[i] == '(':
            if depth == 0 and start is None: start = i
            depth += 1
        elif und[i] == ')':
            depth -= 1
            if depth == 0 and start is not None:
                end = i; break
    args = split_args(und[start + 1:end])
    n = sum(size_of(a) for a in args if a != 'void')
    if '...' in args: conv = 'varargs'
    ret = und[:und.find(m.group(0))] if m else ''
    ret = re.sub(r'^(public|protected|private): |^(static|virtual) ', '', ret)
    ret = re.sub(r'^(static|virtual) ', '', ret).strip()
    if re.match(r'(class|struct|union) \w+$', re.sub(r'\bconst\b', '', ret).strip()):
        n += 4  # the hidden result pointer
    member = re.match(r'(public|protected|private): (?!static )', und) is not None
    if conv == 'thiscall' or (conv == 'stdcall' and member):
        n += 4  # `this` (on the stack for a __stdcall member: COM interfaces' methods)
    if conv == 'fastcall':
        # (the first two 4-byte arguments in ECX and EDX: what is left on the stack, as stdcall)
        n -= 4 * min(2, len([a for a in args if size_of(a) == 4]))
        conv = 'stdcall'
    return conv, n


def main():
    d = sys.argv[1]
    os.makedirs(d, exist_ok=True)
    import pefile
    dll = fetch(f'{SYMSRV}/mfc42.dll/{DLL_KEY}/mfc42.dll', os.path.join(d, 'mfc42.dll'))
    pe = pefile.PE(dll)
    cv = [e.entry for e in pe.DIRECTORY_ENTRY_DEBUG if hasattr(e.entry, 'PdbFileName')][0]
    g = cv.Signature_String
    pdb = fetch(f'{SYMSRV}/mfc42.pdb/{g}{cv.Age:x}/mfc42.pdb', os.path.join(d, 'mfc42.pdb'))
    pubs = os.path.join(d, 'publics.txt')
    if not os.path.exists(pubs):
        open(pubs, 'w').write(subprocess.run(['llvm-pdbutil', 'dump', '-publics', pdb], capture_output=True, text=True, check=True).stdout)
    sdb = fetch(SDB, os.path.join(d, 'mfc42.sdb.txt'))
    secs = [s.VirtualAddress for s in pe.sections]
    byrva = collections.defaultdict(set)
    name = None
    for l in open(pubs):
        m = re.search(r'S_PUB32 \[size = \d+\] `(.*)`', l)
        if m: name = m.group(1); continue
        m = re.search(r'addr = (\d+):(\d+)', l)
        if m and name:
            seg, off = int(m.group(1)), int(m.group(2))
            if seg <= len(secs): byrva[secs[seg - 1] + off].add(name)
            name = None
    r2 = dict(l.rstrip('\n').split('=', 1) for l in open(sdb) if '=' in l)
    names = sorted({n for s in byrva.values() for n in s})
    und_out = subprocess.run(['llvm-undname'], input='\n'.join(names) + '\n', capture_output=True, text=True).stdout.split('\n')
    und, i = {}, 0
    want = set(names)
    while i < len(und_out) - 1:
        if und_out[i] in want and und_out[i] not in und:
            und[und_out[i]] = und_out[i + 1]; i += 2
        else:
            i += 1
    sq = lambda s: re.sub(r'[^A-Za-z0-9_]', '', s)
    img = pe.get_memory_mapped_image()
    rows = []
    for e in pe.DIRECTORY_ENTRY_EXPORT.symbols:
        cands = sorted(byrva.get(e.address, ()))
        want_r2 = r2.get(str(e.ordinal), '')
        if len(cands) > 1:
            meth = sq(want_r2.split('::')[-1])
            hit = [n for n in cands if meth and meth in sq(und.get(n, n)) and sq(want_r2) in sq(und.get(n, n)).replace('public', '').replace('protected', '').replace('private', '').replace('virtual', '').replace('thiscall', '').replace('stdcall', '').replace('cdecl', '')]
            cands = hit or [n for n in cands if meth and meth in sq(und.get(n, n))] or cands
        n = cands[0] if cands else f'#{e.ordinal}'
        conv, nb = conv_bytes(n, und.get(n, n))
        rows.append((e.ordinal, n, conv, nb))
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'out', 'mfc42.tsv')
    with open(out, 'w') as f:
        for r in rows:
            f.write('%d\t%s\t%s\t%d\n' % r)
    print(f'{len(rows)} exports -> {out}', file=sys.stderr)
    if UNKNOWN: sys.exit(f'sizes unknown: {sorted(UNKNOWN)}')


if __name__ == '__main__':
    main()
