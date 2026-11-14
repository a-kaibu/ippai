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
Ties go to the exe with its KiriKiri X.cf beside it, then the shorter, then the smaller path, so
the answer never depends on listing order.
"""
import os, re, shutil, subprocess, sys

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


def biggest(cands, prefer=lambda p: False):
    """The biggest; of the same size, the one `prefer` names, then the shorter, the smaller path."""
    return max(cands, key=lambda c: (c[0], prefer(c[1]), -len(c[1]), [-ord(ch) for ch in c[1]]))[1] if cands else None


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
    # (copies of one exe under other names: the one whose KiriKiri settings X.cf is beside it,
    # as X.exe reads X.cf: せきさば！'s Files/ has せきさば！体験版.exe with its .cf and an
    # identical せきさば体験版.exe, #222)
    cf = lambda p: beside(p, stem(p) + '.cf')
    return biggest(games, cf) or biggest(own) or biggest(all_)


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
# programs and readmes only, all of it smaller than this: no game data anywhere, a replacement exe
# (#198 シールプリンセス sp_trial_exe_for_S3.zip: one 632 KiB exe for S3 cards, the trial's data is
# another download) or a joiner (ほしフル Hoshiful_Preview.exe, 9 KB). Every mirror trial under
# 3 MB on 2026-11-14 was one of these, a patch or a song.
NO_DATA = 2 << 20


def is_pe(path):
    """Whether the file is a Windows program image (MZ, and "PE\\0\\0" at e_lfanew)."""
    try:
        with open(path, 'rb') as f:
            h = f.read(64)
            if len(h) < 64 or h[:2] != b'MZ': return False
            f.seek(int.from_bytes(h[60:64], 'little'))
            return f.read(4) == b'PE\0\0'
    except OSError:
        return False


def cut_archive(path):
    """Whether the program carries an archive that ends early (7-Zip: "Unexpected end of
    archive"): the first piece of a split download, the rest of it elsewhere (#198 SinsAbell
    sinsabell_bm.exe, FDmerge with an LZH of bm/ cut at 11 MB; .002 to .008 are other files)."""
    if not shutil.which('7z'): return False
    try:
        r = subprocess.run(['7z', 'l', path], capture_output=True, text=True, timeout=60,
                           env=dict(os.environ, LC_ALL='C.UTF-8'))
    except (OSError, subprocess.TimeoutExpired):
        return False
    return 'Unexpected end of archive' in r.stdout + r.stderr


def not_game(root, name=''):
    """Why a tree is not a game (None: it may be one): 'movie' when it holds
    movies and nothing else but readmes and pictures, or its download is named as a sample movie;
    'patch' when the download is named as a patch, or the tree is programs only (with readmes)
    and one of them is a patch's updater or all of it is under NO_DATA (with a real program image,
    is_pe); 'split' when the tree is the downloaded program alone and its archive is cut short
    (cut_archive)."""
    fs = [os.path.join(d, f) for d, _, n in os.walk(root) for f in n]
    if any(UPDATER.search(os.path.basename(f)) for f in fs) and \
            all(PROGRAM.search(f) or EXTRA.search(f) for f in fs):
        return 'patch'
    if fs and all(PROGRAM.search(f) or EXTRA.search(f) for f in fs):
        progs = [f for f in fs if PROGRAM.search(f)]
        if sum(os.path.getsize(f) for f in fs) < NO_DATA and any(map(is_pe, progs)): return 'patch'
        # (only the download itself, kept whole because nothing could unpack it: an archive cut
        # inside a program that came in a zip may be a damaged copy, and a game's own exe can
        # carry something 7-Zip takes for a broken archive)
        if len(fs) == 1 and os.path.basename(fs[0]).lower() == os.path.basename(name).lower() \
                and cut_archive(fs[0]): return 'split'
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
