#!/usr/bin/env python3
"""Setup programs of unpacked trials run in sake (node) as a person going through them with the
defaults (SAKE_AUTO: crates/sake/src/autopilot.rs), for tests/corpus-prep.sh: what nothing here
unpacks (Gentee CreateInstall, DGCA and YU-RIS self-extractors), setup programs that run a
Windows Installer package (InstallShield's Setup Launcher, Visual Studio's MSI loader: their
custom actions and external cabinets are the installer's, not msiextract's), and install media
whose game only runs installed (an autorun.inf beside a setup or install program: BLUEGALE's setup.exe,
CatSystem2's INSTALL.exe).

  install.py choose ROOT PICKED      prints the setup program to run (relative to ROOT), exit 1: none
  install.py keep PATH               exit 0: PATH is kept for sake (a setup program that runs an
                                       .msi, or an .msi beside one), not unpacked
  install.py run ROOT SETUP CDIR     runs it (C:\\ = CDIR) and prints the game it made, exit 1: none:
                                       "Z<TAB>path in ROOT" (written beside the setup: a self-extractor)
                                       "C<TAB>path in CDIR" (installed on C:\\)

env: SAKE_NODE (node's sake.js, tools/ci/build.sh's sake-node/), NODE (default node),
SAKE_FONTS (Inno Setup and others lay their dialogs out in font units), INSTALL_TIMEOUT
(seconds, default 900), INSTALL_IDLE (seconds without a new file once one came, default 90).
"""
import os, re, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import pickexe

# setup programs and self-extractors, by what their exe carries
MARKS = [
    (b'DGCA SelfExtractor', 'dgca'),
    # (a DGCA self-extractor whose stub is packed, so its version text is not there to read:
    # the archive's own first chunk, "DGCA" and its 32-byte header size. #198 巣作りドラゴン DragonT.exe)
    (b'DGCA\x20\x00\x00\x00', 'dgca'),
    (b'Gentee Launcher', 'gentee'),
    (b'YU-RIS Self-EXE Extracter', 'yuris-sfx'),
    (b'EXEpress CX', 'exepress'),
    (b"LHA's SFX", 'lha-sfx'),
    # (setup programs that run msiexec on their package: InstallShield 12's Setup Launcher, in one
    # exe (マスクド上海) or beside the .msi and its cabinets (天極姫); Visual Studio's MSI
    # loader, setup.exe with Setup.Ini's [MSILoader] (揺れるバスガイド))
    (b'InstallShieldMSIDelete', 'installshield-msi'),
    (b'MSILoader', 'msiloader'),
]
# the kinds that install a Windows Installer package
MSI_LAUNCHERS = ('installshield-msi', 'msiloader')
SETUP = re.compile(r'(setup|instal|インストール)[^/]*\.exe$', re.I)
NOT_SETUP = re.compile(r'unins|uinst|アンインストール', re.I)


def kind(path):
    """The kind of setup program `path` is by its marks (None: none known)."""
    try:
        with open(path, 'rb') as f: head = f.read(4 << 20)
    except OSError:
        return None
    for mark, k in MARKS:
        if mark in head: return k
    return None


def is_setup_name(rel):
    return bool(SETUP.search(rel)) and not NOT_SETUP.search(rel)


def choose(root, picked):
    """The setup program to run in ROOT, or None: the chosen exe when it is one (by its marks, or
    by name when the tree has nothing else), else the setup program of install media (a folder
    with autorun.inf and a setup/install exe in it or below it, the shallowest)."""
    if picked and (kind(os.path.join(root, picked)) or is_setup_name(picked)):
        return picked
    # (a setup program that runs a package: the game is in the package, the shallowest one)
    launchers = [(rel.count('/'), -size, rel) for size, rel in pickexe.exes(root)
                 if kind(os.path.join(root, rel)) in MSI_LAUNCHERS]
    if launchers:
        return min(launchers)[2]
    media = []
    for d, _, fs in os.walk(root):
        if any(f.lower() == 'autorun.inf' for f in fs):
            media.append(os.path.relpath(d, root).replace(os.sep, '/'))
    setups = []
    for size, rel in pickexe.exes(root):
        if not is_setup_name(rel): continue
        for m in media:
            if m == '.' or rel.startswith(m + '/'):
                setups.append((rel.count('/'), -size, rel))
    return min(setups)[2] if setups else None


def keep(path):
    """Whether corpus-prep.sh leaves `path` for sake rather than unpacking it: a setup program
    that runs a Windows Installer package, or an .msi in the folder of one."""
    if path.lower().endswith('.msi'):
        d = os.path.dirname(path) or '.'
        try: names = os.listdir(d)
        except OSError: return False
        return any(n.lower().endswith('.exe') and kind(os.path.join(d, n)) in MSI_LAUNCHERS for n in names)
    return kind(path) in MSI_LAUNCHERS


def files(top):
    out = {}
    for d, _, fs in os.walk(top):
        for f in fs:
            p = os.path.join(d, f)
            try: st = os.stat(p)
            except OSError: continue
            out[p] = (st.st_size, st.st_mtime_ns)
    return out


# what sake writes on C:\ itself whatever runs (msiexec::install)
OWN = re.compile(r'^windows/system32/msiexec\.exe$', re.I)


def new_files(before, top, base):
    """Files under `top` not in `before` or changed, relative to `base`."""
    now = files(top)
    return {os.path.relpath(p, base).replace(os.sep, '/'): v for p, v in now.items() if before.get(p) != v}


def game_of(made, root_of):
    """The game among the new files `made` ({rel: (size, mtime)}): pickexe's rules over them
    (setup and uninstall programs, tools and the setup itself never), None when they hold none."""
    cands = [(size, rel) for rel, (size, _) in made.items()
             if rel.lower().endswith('.exe') and not OWN.match(rel)]
    # (by the file's own name: the folder a setup made may say install, C:\\autoinstall)
    base = lambda rel: rel.rsplit('/', 1)[-1]
    games = [c for c in cands if not pickexe.TOOL.search(base(c[1])) and not is_setup_name(base(c[1]))
             and not NOT_SETUP.search(base(c[1])) and not kind(root_of(c[1]))]
    return pickexe.choose(games, root_of)


def run(root, setup, cdir):
    """Runs `setup` in sake with C:\\ = cdir; (where, path) of the game it made, or None."""
    sake = os.environ.get('SAKE_NODE')
    if not sake or not os.path.exists(sake):
        print('install: no SAKE_NODE (sake.js for node)', file=sys.stderr)
        return None
    os.makedirs(cdir, exist_ok=True)
    before_x, before_c = files(root), files(cdir)
    env = dict(os.environ, SAKE_C=os.path.abspath(cdir), SAKE_AUTO='1', SAKE_AUTO_LOG='1')
    timeout = int(os.environ.get('INSTALL_TIMEOUT', '900'))
    idle = int(os.environ.get('INSTALL_IDLE', '90'))
    exe = os.path.abspath(os.path.join(root, setup))
    log = open(os.path.join(os.path.dirname(os.path.abspath(cdir)), 'install.log'), 'w')
    p = subprocess.Popen([os.environ.get('NODE', 'node'), os.path.basename(sake), exe], cwd=os.path.dirname(sake),
                         env=env, stdout=log, stderr=subprocess.STDOUT)
    start = last = time.time()
    seen = None
    # (an installer that starts the game when it ends, or waits on a question nobody answers,
    # never exits: it ends once nothing new was written for INSTALL_IDLE seconds)
    while p.poll() is None:
        time.sleep(2)
        n = (len(files(root)), len(files(cdir)), sum(v[0] for v in files(cdir).values()))
        if n != seen:
            seen, last = n, time.time()
        if time.time() - start > timeout or (n != (len(before_x), len(before_c), sum(v[0] for v in before_c.values()))
                                             and time.time() - last > idle):
            p.kill()
            break
    p.wait()
    log.close()
    made_x = new_files(before_x, root, root)
    made_c = {k: v for k, v in new_files(before_c, cdir, cdir).items() if not OWN.match(k)}
    print(f'install: {setup}: {len(made_x)} files beside it, {len(made_c)} on C:, exit {p.returncode}', file=sys.stderr)
    g = game_of(made_c, lambda r: os.path.join(cdir, r))
    if g: return 'C', g
    g = game_of(made_x, lambda r: os.path.join(root, r))
    if g: return 'Z', g
    return None


def main(argv):
    if len(argv) == 3 and argv[0] == 'choose':
        s = choose(argv[1], argv[2])
        if not s: return 1
        print(s)
    elif len(argv) == 2 and argv[0] == 'keep':
        return 0 if keep(argv[1]) else 1
    elif len(argv) == 4 and argv[0] == 'run':
        r = run(argv[1], argv[2], argv[3])
        if not r: return 1
        print(f'{r[0]}\t{r[1]}')
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
