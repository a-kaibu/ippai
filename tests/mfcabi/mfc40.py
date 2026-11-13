#!/usr/bin/env python3
"""MFC 4.0's ordinal -> name table (out/mfc40.tsv) for sake's own MFC40.DLL (crates/sake/src/mfc42.rs).

mfc40.dll (4.1.6140, the one Windows still ships in SysWOW64) has no public PDB, only the
undecorated names the radare2 table gives each ordinal (libr/bin/d/dll/mfc40.sdb.txt). An
overloaded name stands for several exports; MFC 4.2's table (out/mfc42.tsv, from its PDB) has
their decorated names. MFC's .def files number each name's overloads in decorated-name order,
so the n-th ordinal of a name in MFC 4.0 is matched to MFC 4.2's decorated overloads of that
name in order, keeping the matches whose argument bytes agree with the `ret N` that ends the
MFC 4.0 export's code (an order-keeping alignment: overloads MFC 4.2 added are skipped).
An export left unmatched keeps its undecorated name (spaces as _) with the bytes of its ret.

  mfc40.py DIR    DIR: work directory (mfc40.dll and the sdb cached there)

Needs pefile, llvm-objdump and llvm-undname.
"""
import bisect, collections, os, re, subprocess, sys, urllib.request

SYMSRV = 'https://msdl.microsoft.com/download/symbols'
# mfc40.dll 4.1.6140 (x86): TimeDateStamp + SizeOfImage
DLL_KEY = '31E55C32e7000'
SDB = 'https://raw.githubusercontent.com/radareorg/radare2/master/libr/bin/d/dll/mfc40.sdb.txt'
HERE = os.path.dirname(os.path.abspath(__file__))
# MFC 4.0's own overloads (MFC 4.2 changed them), decorated as Visual C++ 4 does
OWN = {
    1006: ('?AfxEndThread@@YGXI@Z', 'stdcall', 4),  # AfxEndThread(UINT nExitCode)
    1884: ('?CreateIndirect@CDialog@@QAEHPBUDLGTEMPLATE@@PAVCWnd@@@Z', 'thiscall', 12),  # (LPCDLGTEMPLATE, CWnd*)
}


def fetch(url, path):
    if not os.path.exists(path):
        with urllib.request.urlopen(url) as r, open(path + '.part', 'wb') as f:
            f.write(r.read())
        os.rename(path + '.part', path)
    return path


def key(s):
    """A qualified name compared across the two tables: no spaces, class/struct keywords or
    arguments"""
    # (a data export's: "protected: static struct AFX_MSGMAP const CWnd::messageMap")
    m = re.match(r'(public|protected|private): static .* (\S+::\S+)$', s)
    if m:
        s = m.group(2)
    s = re.sub(r'\b(class|struct|union|enum) ', '', s)
    return re.sub(r'\s+', '', s)


def stack_bytes(conv, nb):
    """What the callee pops: thiscall's `this` is in ECX, cdecl pops nothing"""
    if conv in ('cdecl', 'varargs'):
        return 0
    return nb - 4 if conv == 'thiscall' else nb


def main():
    d = sys.argv[1]
    import pefile
    dll = fetch(f'{SYMSRV}/mfc40.dll/{DLL_KEY}/mfc40.dll', os.path.join(d, 'mfc40.dll'))
    sdb = fetch(SDB, os.path.join(d, 'mfc40.sdb.txt'))
    pe = pefile.PE(dll)
    base = pe.OPTIONAL_HEADER.ImageBase
    names = {}
    for l in open(sdb):
        m = re.match(r'(\d+)=(.*)', l.rstrip('\n'))
        if m:
            names[int(m.group(1))] = m.group(2)
    # MFC 4.2's decorated names by qualified name, in decorated-name order
    m42 = [l.rstrip('\n').split('\t') for l in open(os.path.join(HERE, 'out', 'mfc42.tsv'))]
    dec = sorted({r[1] for r in m42})
    und = subprocess.run(['llvm-undname', '-no-access-specifier', '-no-calling-convention', '-no-return-type', '-no-member-type', '-no-variable-type'],
                         input='\n'.join(dec) + '\n', capture_output=True, text=True).stdout.split('\n')
    qual = {}
    i = 0
    while i + 1 < len(und):
        if und[i] in qual or not und[i].startswith('?'):
            i += 1
            continue
        qual[und[i]] = key(und[i + 1].split('(')[0]) if '(' in und[i + 1] else key(und[i + 1])
        i += 2
    info = {r[1]: (r[2], int(r[3])) for r in m42}
    by_key = collections.defaultdict(list)
    for n in dec:
        by_key[qual.get(n, n)].append(n)
    # the ret N of each export (up to the next export)
    exps = {e.ordinal: e.address for e in pe.DIRECTORY_ENTRY_EXPORT.symbols}
    starts = sorted(set(exps.values()))
    out = subprocess.run(['llvm-objdump', '-d', '--no-show-raw-insn', '-M', 'intel', dll], capture_output=True, text=True).stdout
    ins = []
    for l in out.split('\n'):
        m = re.match(r'\s*([0-9a-f]+):\s+(.*)', l)
        if m:
            ins.append((int(m.group(1), 16) - base, m.group(2).strip()))
    addrs = [a for a, _ in ins]
    code = [s for s in pe.sections if s.Characteristics & 0x20000000]

    def rets(rva):
        """{N} of the first `ret N` from the export on (MFC's functions end in one epilogue;
        another export can start inside one), set() unknown (a jump first: a forwarder),
        None for data"""
        if not any(s.VirtualAddress <= rva < s.VirtualAddress + s.Misc_VirtualSize for s in code):
            return None
        k = bisect.bisect_left(addrs, rva)
        if k < len(ins) and re.match(r'jmp\s', ins[k][1]):
            return set()
        while k < len(ins) and ins[k][0] < rva + 0x4000:
            m = re.match(r'ret\s*(0x[0-9a-f]+|\d+)?$', ins[k][1])
            if m:
                return {int(m.group(1), 0) if m.group(1) else 0}
            k += 1
        return set()
    # (the one name the radare2 table cuts short: WNDPROC* CWnd::GetSuperWndProcAddr())
    for o, n in names.items():
        if n == 'protected: virtual long':
            names[o] = 'CWnd::GetSuperWndProcAddr'
    groups = collections.defaultdict(list)
    for o in sorted(exps):
        groups[key(names.get(o, f'#{o}'))].append(o)
    rows = {}
    matched = 0
    for k, ords in groups.items():
        cands = by_key.get(k, [])
        # order-keeping alignment: score 2 for a ret that agrees, 1 for an unknown ret
        def score(o, n):
            r = rets(exps[o])
            conv, nb = info[n]
            if conv == 'data':
                return 2 if r is None else 0
            if r is None:
                return 0
            if not r:
                return 1
            return 2 if r == {stack_bytes(conv, nb)} else 0
        A, B = len(ords), len(cands)
        best = [[0] * (B + 1) for _ in range(A + 1)]
        for a in range(A - 1, -1, -1):
            for b in range(B - 1, -1, -1):
                s = score(ords[a], cands[b])
                best[a][b] = max(best[a + 1][b], best[a][b + 1], (best[a + 1][b + 1] + s) if s else 0)
        a = b = 0
        while a < A and b < B:
            s = score(ords[a], cands[b])
            if s and best[a][b] == best[a + 1][b + 1] + s:
                conv, nb = info[cands[b]]
                rows[ords[a]] = (cands[b], conv, nb)
                matched += 1
                a += 1
                b += 1
            elif best[a][b] == best[a + 1][b]:
                a += 1
            else:
                b += 1
    for o, r in OWN.items():
        assert o not in rows, o
        rows[o] = r
    used = collections.Counter()
    for o in sorted(exps):
        if o in rows:
            continue
        n = names.get(o, f'#{o}')
        r = rets(exps[o])
        member = '::' in n
        if r is None:
            conv, nb = 'data', 0
        elif r and next(iter(r)) > 0:
            conv, nb = ('thiscall', next(iter(r)) + 4) if member else ('stdcall', next(iter(r)))
        else:
            conv, nb = ('thiscall', 4) if member else ('cdecl', 0)
        name = re.sub(r'\s+', '_', n)
        used[name] += 1
        if used[name] > 1:
            name += f'#{o}'
        rows[o] = (name, conv, nb)
    seen = collections.Counter(r[0] for r in rows.values())
    with open(os.path.join(HERE, 'out', 'mfc40.tsv'), 'w') as f:
        for o in sorted(rows):
            n, conv, nb = rows[o]
            if seen[n] > 1:
                n += f'#{o}'
            f.write(f'{o}\t{n}\t{conv}\t{nb}\n')
    print(f'{len(rows)} exports, {matched} decorated -> out/mfc40.tsv', file=sys.stderr)


if __name__ == '__main__':
    main()
