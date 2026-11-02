#!/usr/bin/env python3
"""msireg.py PACKAGE.msi INSTALLDIR: prints a Windows Installer package's registry entries
(its Registry table) as root<TAB>key<TAB>name<TAB>value lines for the corpus launcher helper
(host/tests/autoclick.c writes them before starting the game). INSTALLDIR is the Windows
path the package's files ended up in (corpus-prep.sh unpacks them with msiextract). Games read
their install path back from there (Eushully: HKCU\\Software\\Eushully\\<title>\\ExePath)."""
import re, subprocess, sys

ROOTS = {'-1': 'HKCU', '0': 'HKCR', '1': 'HKCU', '2': 'HKLM', '3': 'HKU'}


def table(msi, name):
    r = subprocess.run(['msiinfo', 'export', msi, name], capture_output=True, env={'LC_ALL': 'C.UTF-8'})
    if r.returncode:
        return []
    lines = r.stdout.decode('utf-8', 'replace').split('\r\n' if b'\r\n' in r.stdout else '\n')
    return [l.split('\t') for l in lines[3:] if l]


def main(msi, instdir):
    instdir = instdir.rstrip('\\') + '\\'
    props = {r[0]: r[1] for r in table(msi, 'Property') if len(r) >= 2}
    files = {r[0]: r[2].split('|')[-1] for r in table(msi, 'File') if len(r) >= 3}

    def subst(v):
        def one(m):
            k = m.group(1)
            if k in ('INSTALLDIR', 'TARGETDIR'):
                return instdir
            if k.startswith('#') or k.startswith('!'):
                return instdir + files.get(k[1:], '')
            if k == '~':
                return ''
            return props.get(k, m.group(0))
        return re.sub(r'\[([^\]]+)\]', one, v)

    for r in table(msi, 'Registry'):
        if len(r) < 5 or r[1] not in ROOTS:
            continue
        name, value = r[3], r[4]
        if name in ('+', '-', '*') and not value:
            continue  # key creation/removal markers
        print('\t'.join([ROOTS[r[1]], subst(r[2]), subst(name), subst(value)]))


main(*sys.argv[1:3])
