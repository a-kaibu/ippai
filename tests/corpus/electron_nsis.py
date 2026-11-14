#!/usr/bin/env python3
"""The app of an electron-builder NSIS installer, unpacked as its installer would put it in the
game's folder (for tests/corpus-prep.sh): such an installer carries the whole app as one 7z,
$PLUGINSDIR/app-64.7z (app-32.7z, app-arm64.7z), that its nsis7z.dll unpacks. With it unpacked
the page runs resources/app.asar as a web game (ippai pack unpacks it: crates/ippai/src/asar.rs)
instead of running the installer and then Electron's Chromium in sake (#234).

  electron_nsis.py INSTALLER OUT   exit 0: unpacked into OUT (prints the 7z's name);
                                   exit 1: not one (OUT untouched)
"""
import os, shutil, subprocess, sys, tempfile

# the app 7z's names, the one to take first first (the page runs it whatever its arch)
APPS = ['app-64.7z', 'app-32.7z', 'app-arm64.7z']


def listing(path):
    """`7z l -slt`'s records of PATH as dicts (None when 7z can't read it)."""
    env = dict(os.environ, LC_ALL='C.UTF-8')
    r = subprocess.run(['7z', 'l', '-slt', path], capture_output=True, text=True, errors='replace', env=env)
    if r.returncode != 0: return None
    head, _, body = r.stdout.partition('\n----------\n')
    recs, cur = [], {}
    for line in body.split('\n'):
        if not line.strip():
            if cur: recs.append(cur)
            cur = {}
            continue
        k, sep, v = line.partition(' = ')
        if sep: cur[k] = v
    if cur: recs.append(cur)
    return head, recs


def app_7z(path):
    """The app 7z's path inside the installer at PATH, or None when it is no electron-builder one."""
    got = listing(path)
    if not got: return None
    head, recs = got
    if 'Type = Nsis' not in head: return None
    paths = {r.get('Path', '').replace('\\', '/'): r for r in recs}
    for name in APPS:
        p = f'$PLUGINSDIR/{name}'
        if p in paths and paths[p].get('Folder') != '+': return p
    return None


def unpack(path, out):
    inner = app_7z(path)
    if not inner: return None
    env = dict(os.environ, LC_ALL='C.UTF-8')
    tmp = tempfile.mkdtemp(prefix='electron-nsis-')
    try:
        subprocess.run(['7z', 'x', '-y', f'-o{tmp}', path, inner], check=True, capture_output=True, env=env)
        app = os.path.join(tmp, *inner.split('/'))
        part = out + '.part'
        shutil.rmtree(part, ignore_errors=True)
        os.makedirs(part)
        # (7z keeps what it unpacks inside -o: names with .. are refused)
        subprocess.run(['7z', 'x', '-y', f'-o{part}', app], check=True, capture_output=True, env=env)
        if not os.listdir(part):
            shutil.rmtree(part)
            return None
        shutil.rmtree(out, ignore_errors=True)
        os.rename(part, out)
        return os.path.basename(inner)
    except subprocess.CalledProcessError:
        shutil.rmtree(out + '.part', ignore_errors=True)
        return None
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == '__main__':
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    name = unpack(sys.argv[1], sys.argv[2])
    if not name: sys.exit(1)
    print(name)
