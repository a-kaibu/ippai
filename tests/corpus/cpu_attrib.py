#!/usr/bin/env python3
"""Where a game's CPU goes on sake: waiting, polling, presenting, or its own code (the review
of 2026-11-14: titles where Windows idles but masu uses about a core, docs/winspeed.md).

Run the title with SAKE_API_STATS=<s> (crates/sake/src/api.rs: every s seconds the calls of
each API and the time spent in them, waits included), e.g. corpus-run.py with
SAKE_ENV=SAKE_API_STATS=5, then

  python3 tests/corpus/cpu_attrib.py console.log [--cores C] [--from S]

C: the cores the run used meanwhile (its result.json storyCores or idleCores); S: skip the
windows before S seconds (the start). It prints per bucket the calls a second and the ms a
second inside sake's APIs, and, with C, what is left for the guest's own code (the translated
x86, and sake's work outside API calls: the JIT, the renderer's thread):

  wait     calls that block (WaitFor*, Sleep, GetMessage, ...): time, not CPU. Many short
           ones (under WAIT_SHORT_MS on average, over POLL_RATE a second) are a polling loop
           made of waits: Sleep(0), a wait with no timeout
  poll     calls that only look (PeekMessage, timeGetTime, QueryPerformanceCounter,
           GetAsyncKeyState, ...): a loop that spins on them is busy on sake as on Windows,
           but each call costs more here
  present  putting a picture on the screen (Present, Flip, BitBlt, StretchDIBits, SwapBuffers, ...)
  sound, file, sync, other
Only an API among a window's top 40 is named: the rest of the window's total is "unlisted"."""
import re, sys

WAIT = {'WaitForSingleObject', 'WaitForSingleObjectEx', 'WaitForMultipleObjects', 'WaitForMultipleObjectsEx',
        'MsgWaitForMultipleObjects', 'MsgWaitForMultipleObjectsEx', 'SignalObjectAndWait', 'Sleep', 'SleepEx',
        'GetMessage', 'WaitMessage', 'NtWaitForSingleObject', 'NtWaitForMultipleObjects', 'NtDelayExecution',
        'GetQueuedCompletionStatus', 'SleepConditionVariableCS', 'SleepConditionVariableSRW', 'WaitOnAddress'}
POLL = {'PeekMessage', 'GetTickCount', 'GetTickCount64', 'timeGetTime', 'QueryPerformanceCounter',
        'QueryPerformanceFrequency', 'GetAsyncKeyState', 'GetKeyState', 'GetKeyboardState', 'GetCursorPos',
        'SwitchToThread', 'GetSystemTimeAsFileTime', 'GetSystemTimePreciseAsFileTime', 'GetLocalTime',
        'GetSystemTime', 'GetForegroundWindow', 'GetActiveWindow', 'GetQueueStatus', 'GetInputState',
        'GetMessageTime', 'timeGetSystemTime', 'joyGetPosEx', 'joyGetPos', 'XInputGetState'}
PRESENT = {'BitBlt', 'StretchBlt', 'StretchDIBits', 'SetDIBitsToDevice', 'UpdateWindow', 'InvalidateRect',
           'BeginPaint', 'EndPaint', 'SwapBuffers', 'wglSwapBuffers', 'UpdateLayeredWindow', 'DwmFlush',
           'AlphaBlend', 'TransparentBlt', 'GdiFlush', 'ValidateRect', 'RedrawWindow'}
SOUND_PRE = ('waveOut', 'DirectSound', 'mmio', 'midiOut', 'mci', 'acm')
FILE = {'ReadFile', 'WriteFile', 'SetFilePointer', 'SetFilePointerEx', 'CreateFile', 'CloseHandle', 'GetFileSize',
        'GetFileSizeEx', 'FindFirstFile', 'FindFirstFileEx', 'FindNextFile', 'GetFileAttributes',
        'GetFileAttributesEx', 'MapViewOfFile', 'UnmapViewOfFile', 'CreateFileMapping', 'DeviceIoControl'}
SYNC = {'EnterCriticalSection', 'LeaveCriticalSection', 'TryEnterCriticalSection', 'TlsGetValue', 'TlsSetValue',
        'SetEvent', 'ResetEvent', 'PulseEvent', 'ReleaseMutex', 'ReleaseSemaphore', 'InterlockedIncrement',
        'InterlockedDecrement', 'InterlockedExchange', 'InterlockedCompareExchange', 'AcquireSRWLockExclusive',
        'ReleaseSRWLockExclusive', 'AcquireSRWLockShared', 'ReleaseSRWLockShared', 'WakeConditionVariable',
        'WakeAllConditionVariable', 'GetLastError', 'SetLastError', 'GetCurrentThreadId', 'GetCurrentThread'}
BUCKETS = ('wait', 'poll', 'present', 'sound', 'file', 'sync', 'other', 'unlisted')
WAIT_SHORT_MS, POLL_RATE = 2.0, 200

COUNT = re.compile(r'sake api (\d+) calls/([\d.]+)s: ?(.*)$')
TIME = re.compile(r'sake api time (\d+)ms/([\d.]+)s: ?(.*)$')


def bucket(api):
    """an API's bucket by its name (dll!Name, its A/W ending and COM method prefixes aside)"""
    n = api.split('!')[-1].split('::')[-1]
    base = n[:-1] if n[-1:] in ('A', 'W') and (n[:-1] in WAIT | POLL | PRESENT | FILE | SYNC) else n
    if base in WAIT: return 'wait'
    if base in POLL: return 'poll'
    if base in PRESENT or 'Present' in n or n.endswith('Flip'): return 'present'
    if n.startswith(SOUND_PRE) or 'DirectSound' in api or 'dsound' in api.lower(): return 'sound'
    if base in FILE: return 'file'
    if base in SYNC: return 'sync'
    return 'other'


def pairs(text):
    out = {}
    for t in text.split():
        k, _, v = t.rpartition('=')
        if not k: continue
        v = v[:-2] if v.endswith('ms') else v
        try: out[k] = out.get(k, 0) + int(v)
        except ValueError: pass
    return out


def windows(lines):
    """the log's windows in order: dicts(secs, total_calls, calls{api: n}, total_ms, ms{api: ms})"""
    out, cur = [], None
    for l in lines:
        m = COUNT.search(l)
        if m:
            cur = {'secs': float(m[2]), 'total_calls': int(m[1]), 'calls': pairs(m[3]), 'total_ms': None, 'ms': {}}
            out.append(cur)
            continue
        m = TIME.search(l)
        if m and cur is not None and cur['total_ms'] is None:
            cur['total_ms'], cur['ms'] = int(m[1]), pairs(m[3])
    return out


def attribute(ws, cores=None):
    """the buckets over windows `ws`, per second: {bucket: [calls/s, ms/s]}, the polling waits
    (APIs), and with `cores`, the ms/s left for the guest's own code ('guest')"""
    secs = sum(w['secs'] for w in ws) or 1.0
    b = {k: [0.0, 0.0] for k in BUCKETS}
    per_api = {}
    for w in ws:
        for api, n in w['calls'].items():
            per_api.setdefault(api, [0, 0])[0] += n
        for api, ms in w['ms'].items():
            per_api.setdefault(api, [0, 0])[1] += ms
        b['unlisted'][0] += w['total_calls'] - sum(w['calls'].values())
        if w['total_ms'] is not None:
            b['unlisted'][1] += w['total_ms'] - sum(w['ms'].values())
    for api, (n, ms) in per_api.items():
        k = bucket(api)
        b[k][0] += n
        b[k][1] += ms
    res = {k: [round(v[0] / secs, 1), round(v[1] / secs, 1)] for k, v in b.items()}
    spin = sorted(api for api, (n, ms) in per_api.items()
                  if bucket(api) == 'wait' and n / secs > POLL_RATE and ms / max(n, 1) < WAIT_SHORT_MS)
    out = {'secs': secs, 'buckets': res, 'polling_waits': spin}
    if cores is not None:
        busy = sum(v[1] for k, v in res.items() if k != 'wait')
        out['guest'] = round(max(0.0, cores * 1000 - busy), 1)
    return out


def main(a):
    cores = float(a[a.index('--cores') + 1]) if '--cores' in a else None
    start = float(a[a.index('--from') + 1]) if '--from' in a else 0.0
    ws = windows(open(a[0], encoding='utf-8', errors='replace'))
    t, keep = 0.0, []
    for w in ws:
        if t >= start: keep.append(w)
        t += w['secs']
    if not keep:
        print('no SAKE_API_STATS lines (run with SAKE_API_STATS=5)')
        return 1
    r = attribute(keep, cores)
    print(f"{len(keep)} windows, {r['secs']:.0f} s")
    print('| bucket | calls/s | ms/s in sake |')
    print('|---|---|---|')
    for k in BUCKETS:
        print(f"| {k} | {r['buckets'][k][0]} | {r['buckets'][k][1]} |")
    if r['polling_waits']:
        print(f"waits that poll (over {POLL_RATE}/s, under {WAIT_SHORT_MS} ms each): {' '.join(r['polling_waits'])}")
    if 'guest' in r:
        print(f"guest code and sake outside APIs: {r['guest']} ms/s of {cores * 1000:.0f}")
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]) if len(sys.argv) > 1 else __doc__)
