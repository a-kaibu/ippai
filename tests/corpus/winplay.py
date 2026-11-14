#!/usr/bin/env python3
"""One corpus title on real Windows, the way tests/corpus.mjs plays it on sake, for Windows'
answers (tests/corpus/golden, .github/workflows/corpusref.yml): starts the game, finds the title
screen (the game's biggest window, at least 600x400, whose middle third has more than 300
colours; before it a launcher or settings dialog gets PRE clicks every 3 s, or Return every 6 s
from 12 s on), waits until it holds still, shoots it (title.png), runs the title's steps, shoots
(steps.png), then clicks through the story every 2 s for STORY seconds (story-20.png, ...).
Writes OUT/result.json with the fields corpus.mjs writes. Standard library only (ctypes).

  python winplay.py setup                         once per runner: display mode, time zone
  python winplay.py accept-driver-prompt SECONDS  answers the unsigned-driver prompt meanwhile
  python winplay.py meta TITLES.tsv ID EXE META.json   (any OS) what `play` needs of titles.tsv
  python winplay.py play GAME OUT                 GAME/meta.json + GAME/x: run with its settings
  python winplay.py run EXE OUT [STEPS]           env: ARGS, PRE=x,y, STORY_AT=x,y, TITLE_STILL (2), STORY (30),
                                                  TITLE_MAX (120)

Japanese games on an English Windows: `run` gives the game (and every exe beside it) an external
manifest with activeCodePage ja-JP (code page 932), the embedded manifest's other settings kept
(PreferExternalManifest, set by `setup`), so the game sees code page 932 as on a Japanese PC.

STEPS as corpus.mjs: "click:x,y wait:3 key:Return move:x,y shot:name" in client pixels of the
game window (below 1: a fraction of the client size).
"""
import ctypes, json, os, re, subprocess, sys, time
from ctypes import wintypes as W

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shots

STEP_RE = re.compile(r'^(click|move|wait|key|shot):(.*)$')


def parse_steps(s):
    """STEPS -> [(op, arg)]: click/move (x, y) floats, wait seconds, key name, shot name"""
    out = []
    for t in re.split(r'[ ;]+', s.strip()):
        if not t: continue
        m = STEP_RE.match(t)
        if not m: raise ValueError(f'bad step {t!r}')
        op, a = m.groups()
        if op in ('click', 'move'):
            x, y = (float(v) for v in a.split(','))
            out.append((op, (x, y)))
        elif op == 'wait': out.append((op, float(a)))
        else: out.append((op, a))
    return out


def client_xy(x, y, cw, ch):
    """a step's x, y in client pixels (below 1: a fraction of the client size), as corpus.mjs"""
    if x < 1: x = round(x * cw)
    if y < 1: y = round(y * ch)
    return int(x), int(y)


def settled(hashes, need):
    """whether the title holds still: the frames of the last `need` seconds (one a second) are all
    the same screen (titles.tsv's title_still, 2 by default), as tests/corpus/story.mjs"""
    return len(hashes) > need and all(h == hashes[-1] for h in hashes[-need - 1:])


def still_max(need):
    """how long the wait for a still title lasts at most, as tests/corpus/story.mjs's stillMax"""
    return 25 + max(0, need - 2) * 5


def story_shot(s, story):
    """whether the story's click at second s (every 2 s) is followed by a shot: every 20 s and at
    the end (two screens for check.py's story), as tests/corpus/story.mjs"""
    return s % 20 == 18 or s + 2 >= story


def stats(w, h, rgb):
    """corpus.mjs's frame(): colours of ~3000 samples of the middle third (a grey picture counted
    by its greys) and a sparse hash"""
    step = max(1, (w * h) // 3 // 3000) + 1  # (pixels; corpus.mjs steps bytes of BGRA by 4*k + 4)
    colors = set()
    for i in range((h // 3) * w, ((2 * h) // 3) * w, step):
        colors.add(rgb[3 * i] | (rgb[3 * i + 1] << 8) | (rgb[3 * i + 2] << 16))
    greys = sum(1 for c in colors if abs((c & 255) - ((c >> 8) & 255)) < 3 and abs((c & 255) - (c >> 16)) < 3)
    n = max(len(colors), 301) if greys > 100 else len(colors)
    hsh = 0
    for i in range(0, len(rgb), 3071):
        hsh = (hsh * 31 + rgb[i]) & 0xffffffff
    return n, hsh


def small(w, h):
    return w < 600 or h < 400


MANIFEST_NS = 'http://schemas.microsoft.com/SMI/2019/WindowsSettings'


def with_codepage(xml):
    """a manifest (text, or None for an exe without one) with activeCodePage ja-JP (code page
    932; a number is not a value Windows takes: tests/winref/ja.manifest is what works)"""
    acp = f'<activeCodePage xmlns="{MANIFEST_NS}">ja-JP</activeCodePage>'
    if not xml or '<assembly' not in xml:
        return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                '<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">\n'
                f'<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>{acp}</windowsSettings></application>\n'
                '</assembly>\n')
    xml = re.sub(r'<(\w+:)?activeCodePage\b[^>]*>[^<]*</(\w+:)?activeCodePage>', '', xml)
    m = re.search(r'<(\w+:)?windowsSettings\b[^>]*>', xml)
    if m: return xml[:m.end()] + acp + xml[m.end():]
    # (no windowsSettings: a block of its own; an <application> may also be compatibility's)
    end = xml.rindex('</')
    return xml[:end] + f'<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>{acp}</windowsSettings></application>\n' + xml[end:]


def meta(titles, tid, exe):
    """the title's row as `play` takes it (the steps default as tests/corpus-run.py's)"""
    import csv
    for r in csv.DictReader(open(titles, encoding='utf-8'), delimiter='\t'):
        if r['id'] == tid:
            return {'id': tid, 'exe': exe, 'args': r.get('args') or '', 'steps': r.get('steps') or 'key:Return wait:3',
                    'pre': r.get('pre') or '', 'story_at': r.get('story_at') or '',
                    'title_still': r.get('title_still') or ''}
    raise SystemExit(f'{tid} is not in {titles}')


# ---- Windows from here on ----

if os.name == 'nt':
    u32, g32, k32 = ctypes.WinDLL('user32', use_last_error=True), ctypes.WinDLL('gdi32'), ctypes.WinDLL('kernel32', use_last_error=True)
    psapi = ctypes.WinDLL('psapi')
    u32.SetProcessDPIAware()
    H = W.HANDLE
    ENUMPROC = ctypes.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
    for f, a, r in [(u32.EnumWindows, [ENUMPROC, W.LPARAM], W.BOOL), (u32.GetWindowThreadProcessId, [W.HWND, ctypes.POINTER(W.DWORD)], W.DWORD),
                    (u32.IsWindowVisible, [W.HWND], W.BOOL), (u32.GetClientRect, [W.HWND, ctypes.POINTER(W.RECT)], W.BOOL), (u32.GetWindowRect, [W.HWND, ctypes.POINTER(W.RECT)], W.BOOL),
                    (u32.ClientToScreen, [W.HWND, ctypes.POINTER(W.POINT)], W.BOOL), (u32.GetDC, [W.HWND], H), (u32.ReleaseDC, [W.HWND, H], ctypes.c_int),
                    (u32.SetForegroundWindow, [W.HWND], W.BOOL), (u32.GetForegroundWindow, [], W.HWND), (u32.SetCursorPos, [ctypes.c_int, ctypes.c_int], W.BOOL),
                    (u32.ShowWindow, [W.HWND, ctypes.c_int], W.BOOL), (u32.IsIconic, [W.HWND], W.BOOL), (u32.MapVirtualKeyW, [W.UINT, W.UINT], W.UINT),
                    (u32.GetWindowLongW, [W.HWND, ctypes.c_int], W.LONG), (u32.GetClassNameW, [W.HWND, W.LPWSTR, ctypes.c_int], ctypes.c_int),
                    (u32.GetWindowTextW, [W.HWND, W.LPWSTR, ctypes.c_int], ctypes.c_int), (u32.EnumChildWindows, [W.HWND, ENUMPROC, W.LPARAM], W.BOOL), (u32.PostMessageW, [W.HWND, W.UINT, W.WPARAM, W.LPARAM], W.BOOL),
                    (g32.CreateCompatibleDC, [H], H), (g32.CreateDIBSection, [H, ctypes.c_void_p, W.UINT, ctypes.POINTER(ctypes.c_void_p), H, W.DWORD], H),
                    (g32.SelectObject, [H, H], H), (g32.BitBlt, [H] + [ctypes.c_int] * 4 + [H, ctypes.c_int, ctypes.c_int, W.DWORD], W.BOOL),
                    (g32.DeleteObject, [H], W.BOOL), (g32.DeleteDC, [H], W.BOOL), (g32.GdiFlush, [], W.BOOL),
                    (k32.OpenProcess, [W.DWORD, W.BOOL, W.DWORD], H), (k32.CloseHandle, [H], W.BOOL),
                    (k32.QueryFullProcessImageNameW, [H, W.DWORD, W.LPWSTR, ctypes.POINTER(W.DWORD)], W.BOOL),
                    (k32.TerminateProcess, [H, W.UINT], W.BOOL),
                    (k32.LoadLibraryExW, [W.LPCWSTR, H, W.DWORD], H), (k32.FreeLibrary, [H], W.BOOL),
                    (k32.FindResourceW, [H, ctypes.c_void_p, ctypes.c_void_p], H), (k32.LoadResource, [H, H], H),
                    (k32.LockResource, [H], ctypes.c_void_p), (k32.SizeofResource, [H, H], W.DWORD),
                    (psapi.EnumProcesses, [ctypes.POINTER(W.DWORD), W.DWORD, ctypes.POINTER(W.DWORD)], W.BOOL)]:
        f.argtypes, f.restype = a, r

    class MOUSEINPUT(ctypes.Structure):
        _fields_ = [('dx', W.LONG), ('dy', W.LONG), ('mouseData', W.DWORD), ('dwFlags', W.DWORD), ('time', W.DWORD), ('dwExtraInfo', ctypes.c_size_t)]

    class KEYBDINPUT(ctypes.Structure):
        _fields_ = [('wVk', W.WORD), ('wScan', W.WORD), ('dwFlags', W.DWORD), ('time', W.DWORD), ('dwExtraInfo', ctypes.c_size_t)]

    class INPUT(ctypes.Structure):
        class _U(ctypes.Union):
            _fields_ = [('mi', MOUSEINPUT), ('ki', KEYBDINPUT), ('pad', ctypes.c_byte * 32)]
        _anonymous_ = ('u',)
        _fields_ = [('type', W.DWORD), ('u', _U)]

    u32.SendInput.argtypes, u32.SendInput.restype = [W.UINT, ctypes.POINTER(INPUT), ctypes.c_int], W.UINT

    class DEVMODEW(ctypes.Structure):
        _fields_ = [('dmDeviceName', W.WCHAR * 32), ('dmSpecVersion', W.WORD), ('dmDriverVersion', W.WORD), ('dmSize', W.WORD),
                    ('dmDriverExtra', W.WORD), ('dmFields', W.DWORD), ('dmPosition', W.POINT), ('dmDisplayOrientation', W.DWORD),
                    ('dmDisplayFixedOutput', W.DWORD), ('dmColor', ctypes.c_short), ('dmDuplex', ctypes.c_short), ('dmYResolution', ctypes.c_short),
                    ('dmTTOption', ctypes.c_short), ('dmCollate', ctypes.c_short), ('dmFormName', W.WCHAR * 32), ('dmLogPixels', W.WORD),
                    ('dmBitsPerPel', W.DWORD), ('dmPelsWidth', W.DWORD), ('dmPelsHeight', W.DWORD), ('dmDisplayFlags', W.DWORD),
                    ('dmDisplayFrequency', W.DWORD), ('dmICMMethod', W.DWORD), ('dmICMIntent', W.DWORD), ('dmMediaType', W.DWORD),
                    ('dmDitherType', W.DWORD), ('dmReserved1', W.DWORD), ('dmReserved2', W.DWORD), ('dmPanningWidth', W.DWORD), ('dmPanningHeight', W.DWORD)]


def setup():
    """the display at its biggest mode (1920x1080 at most: games are 640x480 to 1920x1080), Japan's
    time zone, external manifests preferred; prints what the runner has"""
    import winreg
    dm, best, i = DEVMODEW(), None, 0
    dm.dmSize = ctypes.sizeof(DEVMODEW)
    while u32.EnumDisplaySettingsW(None, i, ctypes.byref(dm)):
        k = (dm.dmPelsWidth * dm.dmPelsHeight, dm.dmBitsPerPel)
        if dm.dmPelsWidth <= 1920 and dm.dmPelsHeight <= 1080 and dm.dmBitsPerPel >= 32 and (best is None or k > best[0]):
            best = (k, dm.dmPelsWidth, dm.dmPelsHeight)
        i += 1
    if best:
        dm2 = DEVMODEW()
        dm2.dmSize = ctypes.sizeof(DEVMODEW)
        u32.EnumDisplaySettingsW(None, -1, ctypes.byref(dm2))
        dm2.dmPelsWidth, dm2.dmPelsHeight, dm2.dmFields = best[1], best[2], 0x80000 | 0x100000
        print('display', best[1], best[2], 'ChangeDisplaySettings', u32.ChangeDisplaySettingsW(ctypes.byref(dm2), 1))
    print('screen', u32.GetSystemMetrics(0), u32.GetSystemMetrics(1))
    print('tzutil', subprocess.run(['tzutil', '/s', 'Tokyo Standard Time']).returncode)
    with winreg.CreateKeyEx(winreg.HKEY_LOCAL_MACHINE, r'SOFTWARE\Microsoft\Windows\CurrentVersion\SideBySide', 0, winreg.KEY_SET_VALUE | winreg.KEY_WOW64_64KEY) as k:
        winreg.SetValueEx(k, 'PreferExternalManifest', 0, winreg.REG_DWORD, 1)
    fonts = os.path.join(os.environ.get('WINDIR', r'C:\Windows'), 'Fonts')
    print('fonts', {f: os.path.exists(os.path.join(fonts, f)) for f in ('msgothic.ttc', 'msmincho.ttc', 'meiryo.ttc', 'YuGothR.ttc')})
    print('ACP', k32.GetACP(), 'OEMCP', k32.GetOEMCP())
    # (no sound device: DirectSound fails and games stop at an error box; corpusref.yml installs one)
    print('waveOut devices', ctypes.WinDLL('winmm').waveOutGetNumDevs())


def embedded_manifest(exe):
    """the exe's RT_MANIFEST (id 1, or 2 for a DLL's) as text, or None"""
    m = k32.LoadLibraryExW(exe, None, 0x2 | 0x20)  # (LOAD_LIBRARY_AS_DATAFILE | AS_IMAGE_RESOURCE)
    if not m: return None
    try:
        for rid in (1, 2, 3):
            r = k32.FindResourceW(m, ctypes.c_void_p(rid), ctypes.c_void_p(24))
            if r:
                n = k32.SizeofResource(m, r)
                p = k32.LockResource(k32.LoadResource(m, r))
                b = ctypes.string_at(p, n)
                for enc in ('utf-8-sig', 'utf-16', 'cp1252'):
                    try: return b.decode(enc)
                    except UnicodeDecodeError: pass
        return None
    finally:
        k32.FreeLibrary(m)


def give_codepage(exe):
    """external manifests with activeCodePage ja-JP for every exe in the game's folder (a launcher
    starts the game beside it); one already there stays (the game's own)"""
    d = os.path.dirname(exe)
    done = []
    for n in os.listdir(d):
        p = os.path.join(d, n)
        if not n.lower().endswith('.exe') or os.path.exists(p + '.manifest'): continue
        try:
            open(p + '.manifest', 'w', encoding='utf-8').write(with_codepage(embedded_manifest(p)))
            done.append(n)
        except OSError as e:
            print('manifest', n, e)
    return done


def pids_under(d):
    """processes whose image is under folder d"""
    a = (W.DWORD * 4096)()
    n = W.DWORD()
    psapi.EnumProcesses(a, ctypes.sizeof(a), ctypes.byref(n))
    d = os.path.normcase(os.path.abspath(d)) + os.sep
    out = set()
    for pid in a[:n.value // 4]:
        h = k32.OpenProcess(0x1000, False, pid)  # (PROCESS_QUERY_LIMITED_INFORMATION)
        if not h: continue
        buf, sz = ctypes.create_unicode_buffer(1024), W.DWORD(1024)
        if k32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(sz)) and os.path.normcase(buf.value).startswith(d):
            out.add(pid)
        k32.CloseHandle(h)
    return out


def game_window(pids):
    """the biggest visible top-level window of those processes: (hwnd, (sx, sy, w, h)) or None"""
    found = []
    def cb(hwnd, _):
        pid = W.DWORD()
        u32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if pid.value in pids and u32.IsWindowVisible(hwnd) and not u32.IsIconic(hwnd):
            r = W.RECT()
            u32.GetClientRect(hwnd, ctypes.byref(r))
            if r.right > 32 and r.bottom > 32:
                p = W.POINT(0, 0)
                u32.ClientToScreen(hwnd, ctypes.byref(p))
                found.append((r.right * r.bottom, hwnd, (p.x, p.y, r.right, r.bottom)))
        return True
    u32.EnumWindows(ENUMPROC(cb), 0)
    if not found: return None
    found.sort(key=lambda f: -f[0])
    return found[0][1], found[0][2]


def dialogs(pids):
    """the visible dialog boxes (#32770: message boxes, error boxes) of those processes, as
    "caption: text" (a shot with one over the game is no answer)"""
    out = []
    def text(h):
        b = ctypes.create_unicode_buffer(512)
        u32.GetWindowTextW(h, b, 512)
        return b.value
    def cb(hwnd, _):
        pid = W.DWORD()
        u32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if pid.value in pids and u32.IsWindowVisible(hwnd):
            c = ctypes.create_unicode_buffer(64)
            u32.GetClassNameW(hwnd, c, 64)
            if c.value == '#32770':
                parts = []
                def child(h, _):
                    t = text(h)
                    if t: parts.append(t)
                    return True
                u32.EnumChildWindows(hwnd, ENUMPROC(child), 0)
                out.append(f"{text(hwnd)}: {' / '.join(parts)}"[:300])
        return True
    u32.EnumWindows(ENUMPROC(cb), 0)
    return out


def accept_driver_prompt(seconds):
    """answers Windows Security's "Windows can't verify the publisher of this driver software"
    with "Install this driver software anyway" (corpusref.yml's virtual sound card: the runner
    is in test mode, the prompt is all that stands in the way) for that many seconds"""
    t = time.time()
    answered = 0
    while time.time() - t < seconds:
        found = []
        def cb(hwnd, _):
            b = ctypes.create_unicode_buffer(256)
            u32.GetWindowTextW(hwnd, b, 256)
            if b.value == 'Windows Security' and u32.IsWindowVisible(hwnd): found.append(hwnd)
            return True
        u32.EnumWindows(ENUMPROC(cb), 0)
        for hwnd in found:
            r = W.RECT()
            u32.GetWindowRect(hwnd, ctypes.byref(r))
            front(hwnd)
            # (the second command link: measured on the runner's 500x298 dialog at 100 %)
            x, y = r.left + round(0.394 * (r.right - r.left)), r.top + round(0.63 * (r.bottom - r.top))
            u32.SetCursorPos(x, y)
            mouse(0x2); time.sleep(0.1); mouse(0x4)
            answered += 1
            print('answered the driver prompt at', x, y, flush=True)
        time.sleep(1)
    return answered


def front(hwnd):
    if u32.GetForegroundWindow() != hwnd:
        # (Windows lets the process that got the last input set the foreground: an Alt press first)
        key_event(0x12, False); key_event(0x12, True)
        u32.SetForegroundWindow(hwnd)


def grab(rect):
    """the screen's pixels in rect (what the game shows, Direct3D included): (w, h, rgb)"""
    sx, sy, w, h = rect
    sw, sh = u32.GetSystemMetrics(0), u32.GetSystemMetrics(1)
    # (a window partly off screen: only what is on it)
    w, h = max(1, min(w, sw - max(sx, 0))), max(1, min(h, sh - max(sy, 0)))
    sx, sy = max(sx, 0), max(sy, 0)
    scr = u32.GetDC(None)
    dc = g32.CreateCompatibleDC(scr)
    bmi = (ctypes.c_byte * 44)()
    ctypes.memmove(bmi, (ctypes.c_int32 * 3)(40, w, -h), 12)
    ctypes.memmove(ctypes.byref(bmi, 12), (ctypes.c_uint16 * 2)(1, 32), 4)
    bits = ctypes.c_void_p()
    bmp = g32.CreateDIBSection(dc, bmi, 0, ctypes.byref(bits), None, 0)
    old = g32.SelectObject(dc, bmp)
    g32.BitBlt(dc, 0, 0, w, h, scr, sx, sy, 0x00CC0020 | 0x40000000)  # (SRCCOPY | CAPTUREBLT)
    g32.GdiFlush()
    bgra = ctypes.string_at(bits, w * h * 4)
    g32.SelectObject(dc, old); g32.DeleteObject(bmp); g32.DeleteDC(dc); u32.ReleaseDC(None, scr)
    rgb = bytearray(w * h * 3)
    rgb[0::3], rgb[1::3], rgb[2::3] = bgra[2::4], bgra[1::4], bgra[0::4]
    return w, h, bytes(rgb)


def send(*inputs):
    a = (INPUT * len(inputs))(*inputs)
    u32.SendInput(len(inputs), a, ctypes.sizeof(INPUT))


def key_event(vk, up):
    i = INPUT(type=1)
    i.ki = KEYBDINPUT(vk, u32.MapVirtualKeyW(vk, 0), 2 if up else 0, 0, 0)
    send(i)


def mouse(flags):
    i = INPUT(type=0)
    i.mi = MOUSEINPUT(0, 0, 0, flags, 0, 0)
    send(i)


VK = {'Return': 0x0d, 'Space': 0x20, 'Escape': 0x1b, 'Down': 0x28, 'Up': 0x26, 'Ctrl': 0x11, 'z': 0x5a}


def end(pids, grace, close, confirm, kill, alive, sleep=time.sleep, clock=time.time):
    """Ends the game: with grace > 0, asks its processes to close (WM_CLOSE: taskkill without /F)
    and waits up to grace seconds, answering a "quit?" box after 3 s with Return (its default
    button), once; then kills what is left. True when it ended before the kill."""
    if grace > 0:
        for pid in pids():
            close(pid)
        t, asked = clock(), False
        while alive() and clock() - t < grace:
            sleep(0.5)
            if not asked and clock() - t >= 3 and alive():
                confirm()
                asked = True
    ended = not alive()
    for pid in pids():
        kill(pid)
    return ended


def run(exe, out, steps):
    exe = os.path.abspath(exe)
    d = os.path.dirname(exe)
    os.makedirs(out, exist_ok=True)
    STORY, TITLE_MAX = float(os.environ.get('STORY', 30)), float(os.environ.get('TITLE_MAX', 120))
    PRE = [float(v) for v in os.environ['PRE'].split(',')] if os.environ.get('PRE') else None
    res = {'exe': exe, 'steps': steps, 'title': None, 'errors': [], 'shots': [], 'windows': True}
    res['manifests'] = give_codepage(exe)
    plan = parse_steps(steps)
    t0 = time.time()
    now = lambda: time.time() - t0
    save = lambda: open(os.path.join(out, 'result.json'), 'w', encoding='utf-8').write(json.dumps(res, ensure_ascii=False, indent=1))
    p = subprocess.Popen(f'"{exe}" {os.environ.get("ARGS", "")}'.strip(), cwd=d)
    win = [None, None]  # hwnd, rect

    def look():
        pids = pids_under(d) | ({p.pid} if p.poll() is None else set())
        g = game_window(pids)
        if not g: return None
        win[0], win[1] = g
        return g

    def frame():
        if not look(): return None
        front(win[0])
        return grab(win[1])

    def shot(name):
        f = frame()
        if not f: return None
        shots.write(os.path.join(out, name + '.png'), *f)
        res['shots'].append(name)
        dl = dialogs(pids_under(d) | ({p.pid} if p.poll() is None else set()))
        if dl: res.setdefault('dialogs', {})[name] = dl
        return f

    def click(x, y):
        if not look(): return
        sx, sy, cw, ch = win[1]
        x, y = client_xy(x, y, cw, ch)
        front(win[0])
        u32.SetCursorPos(sx + x, sy + y)
        mouse(0x2); time.sleep(0.1); mouse(0x4)

    def key(name):
        if look(): front(win[0])
        vk = VK.get(name, ord(name[0].upper()))
        key_event(vk, False); time.sleep(0.05); key_event(vk, True)

    def alive():
        return p.poll() is None or bool(pids_under(d))

    try:
        last, first, last_pre = None, None, 0.0
        while now() < TITLE_MAX and alive():
            time.sleep(1)
            f = frame()
            if not f: continue
            w, h, _ = f
            last = f
            n, _ = stats(*f)
            if first is None and w > 32: first = now()
            if PRE and now() - last_pre >= 3:
                last_pre = now(); click(*PRE)
            elif not PRE and now() > 12 and now() - last_pre >= 6 and (n <= 300 or small(w, h)):
                last_pre = now(); key('Return')
            if n > 300 and not small(w, h):
                res['title'] = now()
                break
        res['firstWindow'] = first
        if last: res['size'] = [last[0], last[1]]
        if res['title'] is None:
            shot('notitle')
            res['crashed'] = not alive()
            save()
            return
        need, hashes, t = int(os.environ.get('TITLE_STILL') or 2), [], now()
        while now() - t < still_max(need) and alive():
            time.sleep(1)
            f = frame()
            if not f: continue
            hashes.append(stats(*f)[1])
            if settled(hashes, need): break
        res['titleStill'] = now()
        f = shot('title')
        res['titleHash'] = stats(*f)[1] if f else None
        save()
        for op, a in plan:
            if op == 'click': click(*a)
            elif op == 'move':
                if look():
                    x, y = client_xy(*a, win[1][2], win[1][3])
                    u32.SetCursorPos(win[1][0] + x, win[1][1] + y)
            elif op == 'wait': time.sleep(a)
            elif op == 'key': key(a)
            elif op == 'shot': shot(a)
        shot('steps')
        at = [float(v) for v in os.environ['STORY_AT'].split(',')] if os.environ.get('STORY_AT') else None
        hashes, s = set(), 0
        while s < STORY and alive():
            cw, ch = (win[1][2], win[1][3]) if win[1] else (640, 480)
            click(*(at or (cw / 2, round(ch * 0.85))))
            time.sleep(2)
            if story_shot(s, STORY):
                f = shot(f'story-{s + 2}')
                if f: hashes.add(stats(*f)[1])
            s += 2
        res['storyFrames'] = len(hashes)
        res['leftTitle'] = any(h != res['titleHash'] for h in hashes)
        res['crashed'] = not alive()
    except Exception as e:
        res['errors'].append(f'winplay: {type(e).__name__}: {e}')
    finally:
        res['end'] = now()
        # (CLOSE_FIRST seconds: asked to close first, so a tracer in it writes out what it holds:
        # tests/replay/record-game.ps1; then killed)
        res['closed'] = end(lambda: pids_under(d) | ({p.pid} if p.poll() is None else set()),
                            float(os.environ.get('CLOSE_FIRST', 0)),
                            lambda pid: subprocess.run(['taskkill', '/T', '/PID', str(pid)], capture_output=True),
                            lambda: key('Return'),
                            lambda pid: subprocess.run(['taskkill', '/F', '/T', '/PID', str(pid)], capture_output=True),
                            alive)
        save()


if __name__ == '__main__':
    a = sys.argv[1:]
    if a[:1] == ['setup']: setup()
    elif a[:1] == ['accept-driver-prompt'] and len(a) == 2: print('answered', accept_driver_prompt(float(a[1])))
    elif a[:1] == ['run'] and len(a) in (3, 4): run(a[1], a[2], a[3] if len(a) == 4 else '')
    elif a[:1] == ['play'] and len(a) == 3:
        # (corpusref.yml: GAME/meta.json from `meta` on the Linux runner, the game in GAME/x)
        m = json.load(open(os.path.join(a[1], 'meta.json'), encoding='utf-8'))
        for k, e in (('args', 'ARGS'), ('pre', 'PRE'), ('story_at', 'STORY_AT'), ('title_still', 'TITLE_STILL')):
            if m.get(k): os.environ[e] = m[k]
        run(os.path.join(a[1], 'x', *m['exe'].split('/')), a[2], m.get('steps') or 'key:Return wait:3')
    elif a[:1] == ['meta'] and len(a) == 5:
        print(json.dumps(meta(a[1], a[2], a[3]), ensure_ascii=False), file=open(a[4], 'w', encoding='utf-8'))
    else: sys.exit(__doc__)
