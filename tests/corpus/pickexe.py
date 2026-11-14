#!/usr/bin/env python3
"""The game's exe in an unpacked trial (tests/corpus-prep.sh, when titles.tsv names none).

  pickexe.py DIR [NAME]  prints the exe's path relative to DIR; exit 3 when there is no .exe,
                         exit 5 with "not a game: ..." on stderr when DIR (unpacked from the
                         download NAME) is not a game at all: only movies, or a patch (not_game())

In order:
  0. Triangle's chapter trials: DEMO.EXE beside DTri*.INI (妄想ぷろとこる！'s DLPC02/DLPC04: the
     bigger DLPC0n.EXE run alone refuses, "起動用のファイルが読み込めません" or reads the INI
     DEMO.EXE has not written yet, "初期化ファイルが破壊されています"; #172).
  1. Unity: X.exe beside its X_Data folder (the biggest such). Its own helpers (ffmpeg.exe in
     StreamingAssets, 80 MB in SuccubusTemptationW's trial, bigger than the game) never count.
  1b. BISHOP (BSXX): startup.exe beside bsxx.dat. It writes bsxx.ini, without which the game
     exe (demo.exe, bigger) stops with 「起動設定ファイルの読み込みに失敗しました」.
  2. Ren'Py: X.exe beside X.py and renpy/ (lib/windows-i686/zsync.exe, python.exe are tools).
  3. The biggest .exe that is not a tool (installers, configuration, redistributables, ffmpeg,
     ...; a launcher, whose start button may want an install first: ensemble's launcher.exe
     beside AdvHD.exe; Silky's start.exe beside the game), matched without the folders the unpacking made (setup.exe.inno/app/game.exe is the
     game Inno Setup installs, not a setup program; inst.exe is AGS's installer beside ags.exe), nor inside a Unity *_Data folder or a
     Ren'Py lib/ folder.
  4. Nothing but tools: the biggest .exe (an installer sake runs itself), helpers last.
Ties go to the shorter, then the smaller path, so the answer never depends on listing order.
"""
import os, re, sys

TOOL = re.compile(r'unins|setup|install|config|launcher|WindowsInstaller|InstMsi|KB[0-9]{6}|vcredist|vc_redist|redist|directx|dxsetup|tool|設定|update|DXWeb|dotnet|CrashHandler|ffmpeg|zsync|(^|/)inst\.exe$'
                  # (Silky's start.exe: a launcher beside the game, 堕とされた美姉妹's bishimai.exe, #228; a
                  # copy of the game named for its icon, せきさば！体験版_アイコン２.exe; Japanese
                  # installers and tools: インストール.EXE, ファイル破損チェックツール.exe)
                  r'|(^|/)start\.exe$|アイコン|インストール|ツール', re.I)
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
    return choose(exes(root), lambda rel: os.path.join(root, rel))


def choose(all_, path_of):
    """pick()'s answer among [(size, relative path)], `path_of` giving a path's file (the folder
    beside it is looked at: a Unity *_Data, Ren'Py's renpy/, BISHOP's bsxx.dat)."""
    def beside(p, name):
        return os.path.exists(os.path.join(os.path.dirname(path_of(p)), name))
    stem = lambda p: os.path.basename(p)[:-4]
    triangle = [c for c in all_ if os.path.basename(c[1]).lower() == 'demo.exe'
                and any(re.fullmatch(r'dtri.*\.ini', f, re.I) for f in os.listdir(os.path.dirname(path_of(c[1]))))]
    if triangle: return biggest(triangle)
    unity = [c for c in all_ if not HELPER_DIR.search(c[1]) and os.path.isdir(os.path.join(os.path.dirname(path_of(c[1])), stem(c[1]) + '_Data'))]
    if unity: return biggest(unity)
    bishop = [c for c in all_ if os.path.basename(c[1]).lower() == 'startup.exe' and beside(c[1], 'bsxx.dat')]
    if bishop: return biggest(bishop)
    renpy = [c for c in all_ if not HELPER_DIR.search(c[1]) and beside(c[1], stem(c[1]) + '.py') and beside(c[1], 'renpy')]
    if renpy: return biggest(renpy)
    own = [c for c in all_ if not HELPER_DIR.search(c[1])]
    games = [c for c in own if not TOOL.search(UNPACKED.sub('', c[1]))]
    return biggest(games) or biggest(own) or biggest(all_)


MOVIE = re.compile(r'\.(mpg|mpeg|m1v|wmv|asf|mp4|m4v|avi|mov|flv|webm|mkv|m2ts|ts|ogv)$', re.I)
# (what comes with a movie: its readme, a shortcut, a picture)
EXTRA = re.compile(r'\.(txt|url|html?|jpe?g|png|bmp|gif|ico|pdf|ini|db)$', re.I)
# download names that say what they are: DLsite's sample movies (VJ012345_trial_mov.zip), DMM's
# (xero_0038m.zip; trials are ..._0038t.zip), patches (noble_trial_patch101.zip)
NAME_MOVIE = re.compile(r'(_mov|_movie|^[a-z]+_?[0-9]+m)\.(zip|lzh|rar|7z)$', re.I)
NAME_PATCH = re.compile(r'patch', re.I)
# a patch's own program, which writes the files beside it over an installed game's
# (hatahani_try_v101.zip: update.exe and the new hatahani_try.exe, no data)
UPDATER = re.compile(r'^(update|patch|updater|アップデート|パッチ)[^/]*\.exe$', re.I)
PROGRAM = re.compile(r'\.(exe|dll)$', re.I)


def not_game(root, name=''):
    """Why a tree is not a game (None: it may be one): 'movie' when it holds
    movies and nothing else but readmes and pictures, or its download is named as a sample movie;
    'patch' when the download is named as a patch, or the tree is programs only (with readmes)
    and one of them is a patch's updater."""
    fs = [os.path.join(d, f) for d, _, n in os.walk(root) for f in n]
    if any(UPDATER.search(os.path.basename(f)) for f in fs) and \
            all(PROGRAM.search(f) or EXTRA.search(f) for f in fs):
        return 'patch'
    if exes(root): return None
    movies = [f for f in fs if MOVIE.search(f)]
    if movies and all(MOVIE.search(f) or EXTRA.search(f) for f in fs): return 'movie'
    base = os.path.basename(name)
    if NAME_MOVIE.search(base): return 'movie'
    if NAME_PATCH.search(base): return 'patch'
    return None


if __name__ == '__main__':
    why = not_game(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else '')
    if why:
        print(f'not a game: {why} ({os.path.basename(sys.argv[2]) if len(sys.argv) > 2 else ""})', file=sys.stderr)
        sys.exit(5)
    e = pick(sys.argv[1])
    if not e: sys.exit(3)
    print(e)
