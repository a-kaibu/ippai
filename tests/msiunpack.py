#!/usr/bin/env python3
"""msiunpack.py PACKAGE.msi OUTDIR: unpacks a Windows Installer package's files when msiextract
can't (cabinets split into a set give it "Invalid folder index"): the cabinets the Media table
names, embedded ("#name": a stream of the package) or beside the package (InstallShield's
Data1.cab + Data11.cab of 三極姫４ 体験版), go through 7-Zip, which follows a cabinet set from
volume to volume; then each file (named by its File table key) moves to its install path from
the Directory/Component tables."""
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


SYSTEM = {'ProgramFilesFolder': 'Program Files', 'LocalAppDataFolder': 'LocalAppData',
          'AppDataFolder': 'AppData', 'CommonAppDataFolder': 'CommonAppData'}


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
        # the system folders, named as msiextract names them
        if d in SYSTEM:
            return SYSTEM[d]
        p = path(parent, seen + (d,))
        return p if name in ('.', 'SourceDir') else os.path.join(p, name)

    os.makedirs(out, exist_ok=True)
    tmp = tempfile.mkdtemp(dir=out)
    here = os.path.dirname(os.path.abspath(msi))
    beside = {f.lower(): f for f in os.listdir(here)}
    streams = subprocess.run(['msiinfo', 'streams', msi], capture_output=True, text=True).stdout.split()
    cabs = []
    # (the Media table's cabinets in disk order; streams named *.cab that it does not name too)
    named = [r[3] for r in sorted(table(msi, 'Media'), key=lambda r: int(r[0]) if r[0].isdigit() else 0)
             if len(r) >= 4 and r[3]]
    for c in named + [s for s in streams if s.lower().endswith('.cab') and '#' + s not in named]:
        if c.startswith('#') or c in streams:
            name = c.lstrip('#')
            if name not in streams or os.path.exists(os.path.join(tmp, name)):
                continue
            # (a set spanning several streams: their names kept so 7-Zip follows the set)
            with open(os.path.join(tmp, name), 'wb') as f:
                subprocess.run(['msiinfo', 'extract', msi, name], stdout=f, check=True)
            cabs.append(os.path.join(tmp, name))
        elif c.lower() in beside:
            cabs.append(os.path.join(here, beside[c.lower()]))
    os.makedirs(os.path.join(tmp, 'files'))
    for c in cabs:
        # (-aos: a file a set's first volume already gave is not unpacked again from a later one,
        # which may hold only its tail)
        subprocess.run(['7z', 'x', '-y', '-aos', '-o' + os.path.join(tmp, 'files'), c],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=dict(os.environ, LC_ALL='C.UTF-8'))
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
