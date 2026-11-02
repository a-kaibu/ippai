#!/usr/bin/env python3
"""msiunpack.py PACKAGE.msi OUTDIR: unpacks a Windows Installer package's files when msiextract
can't (packages split over several embedded cabinets give it "Invalid folder index"): every
embedded .cab stream goes through cabextract, then each file (named by its File table key)
moves to its install path from the Directory/Component tables."""
import os, shutil, subprocess, sys, tempfile


def table(msi, name):
    r = subprocess.run(['msiinfo', 'export', msi, name], capture_output=True, env={'LC_ALL': 'C.UTF-8'})
    if r.returncode:
        return []
    lines = r.stdout.decode('utf-8', 'replace').replace('\r\n', '\n').split('\n')
    return [l.split('\t') for l in lines[3:] if l]


def long_name(n):
    # "short|long" names; "." means the parent itself
    return n.split('|')[-1]


def main(msi, out):
    dirs = {r[0]: (r[1], r[2]) for r in table(msi, 'Directory') if len(r) >= 3}
    comps = {r[0]: r[2] for r in table(msi, 'Component') if len(r) >= 3}
    files = {r[0]: (r[1], long_name(r[2])) for r in table(msi, 'File') if len(r) >= 3}

    def path(d, seen=()):
        if d not in dirs or d in seen:
            return ''
        parent, default = dirs[d]
        # DefaultDir: "target:source" -> target
        name = long_name(default.split(':')[0])
        if d == 'TARGETDIR' or not parent or parent == d:
            return ''
        p = path(parent, seen + (d,))
        return p if name in ('.', 'SourceDir') else os.path.join(p, name)

    streams = subprocess.run(['msiinfo', 'streams', msi], capture_output=True, text=True).stdout.split()
    tmp = tempfile.mkdtemp(dir=out)
    # (a cabinet set spans several streams: keep their names so cabextract can follow the set,
    # and let the ones that continue a set fail on their own)
    cabs = [s for s in streams if s.lower().endswith('.cab')]
    for s in cabs:
        with open(os.path.join(tmp, s), 'wb') as f:
            subprocess.run(['msiinfo', 'extract', msi, s], stdout=f, check=True)
    for s in cabs:
        subprocess.run(['cabextract', '-q', '-d', os.path.join(tmp, 'files'), os.path.join(tmp, s)],
                       stderr=subprocess.DEVNULL)
    n = 0
    for key in os.listdir(os.path.join(tmp, 'files')):
        if key not in files:
            continue
        comp, name = files[key]
        dst = os.path.join(out, path(comps.get(comp, '')), name)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.move(os.path.join(tmp, 'files', key), dst)
        n += 1
    shutil.rmtree(tmp)
    if not n:
        sys.exit('no files')


main(*sys.argv[1:3])
