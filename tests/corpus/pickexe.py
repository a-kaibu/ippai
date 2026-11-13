#!/usr/bin/env python3
"""The game's exe in an unpacked trial (tests/corpus-prep.sh, when titles.tsv names none).

  pickexe.py DIR      prints the exe's path relative to DIR; exit 3 when there is no .exe

In order:
  0. Triangle's chapter trials: DEMO.EXE beside DTri*.INI (妄想ぷろとこる！'s DLPC02/DLPC04: the
     bigger DLPC0n.EXE run alone refuses, "起動用のファイルが読み込めません" or reads the INI
     DEMO.EXE has not written yet, "初期化ファイルが破壊されています"; #172).
  1. Unity: X.exe beside its X_Data folder (the biggest such). Its own helpers (ffmpeg.exe in
     StreamingAssets, 80 MB in SuccubusTemptationW's trial, bigger than the game) never count.
  2. Ren'Py: X.exe beside X.py and renpy/ (lib/windows-i686/zsync.exe, python.exe are tools).
  3. The biggest .exe that is not a tool (installers, configuration, redistributables, ffmpeg,
     ...; a launcher, whose start button may want an install first: ensemble's launcher.exe
     beside AdvHD.exe), matched without the folders the unpacking made (setup.exe.inno/app/game.exe is the
     game Inno Setup installs, not a setup program; inst.exe is AGS's installer beside ags.exe), nor inside a Unity *_Data folder or a
     Ren'Py lib/ folder.
  4. Nothing but tools: the biggest .exe (an installer sake runs itself), helpers last.
Ties go to the shorter, then the smaller path, so the answer never depends on listing order.
"""
import os, re, sys

TOOL = re.compile(r'unins|setup|install|config|launcher|WindowsInstaller|InstMsi|KB[0-9]{6}|vcredist|vc_redist|redist|directx|dxsetup|tool|設定|update|DXWeb|dotnet|CrashHandler|ffmpeg|zsync|(^|/)inst\.exe$', re.I)
UNPACKED = re.compile(r'[^/]*\.(exe|msi|zip|lzh|rar|7z)\.(is|inno|zip|sfx|x)/', re.I)
# (a Unity game's data folder, a Ren'Py game's helper programs)
HELPER_DIR = re.compile(r'(^|/)[^/]*_Data/|(^|/)lib/(windows|py[0-9][^/]*-windows)[^/]*/', re.I)


def exes(root):
    """[(size, relative path)] of every .exe under root ('/'-separated)."""
    out = []
    for d, _, fs in os.walk(root):
        for f in fs:
            if f.lower().endswith('.exe'):
                p = os.path.join(d, f)
                if os.path.isfile(p):
                    out.append((os.path.getsize(p), os.path.relpath(p, root).replace(os.sep, '/')))
    return out


def biggest(cands):
    return max(cands, key=lambda c: (c[0], -len(c[1]), [-ord(ch) for ch in c[1]]))[1] if cands else None


def pick(root):
    all_ = exes(root)
    def beside(p, name):
        return os.path.exists(os.path.join(root, os.path.dirname(p), name))
    stem = lambda p: os.path.basename(p)[:-4]
    triangle = [c for c in all_ if os.path.basename(c[1]).lower() == 'demo.exe'
                and any(re.fullmatch(r'dtri.*\.ini', f, re.I) for f in os.listdir(os.path.join(root, os.path.dirname(c[1]))))]
    if triangle: return biggest(triangle)
    unity = [c for c in all_ if not HELPER_DIR.search(c[1]) and os.path.isdir(os.path.join(root, os.path.dirname(c[1]), stem(c[1]) + '_Data'))]
    if unity: return biggest(unity)
    renpy = [c for c in all_ if not HELPER_DIR.search(c[1]) and beside(c[1], stem(c[1]) + '.py') and beside(c[1], 'renpy')]
    if renpy: return biggest(renpy)
    own = [c for c in all_ if not HELPER_DIR.search(c[1])]
    games = [c for c in own if not TOOL.search(UNPACKED.sub('', c[1]))]
    return biggest(games) or biggest(own) or biggest(all_)


if __name__ == '__main__':
    e = pick(sys.argv[1])
    if not e: sys.exit(3)
    print(e)
