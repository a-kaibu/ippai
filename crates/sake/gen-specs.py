#!/usr/bin/env python3
"""Argument sizes of Win32 exports from Wine's .spec files -> specs.txt (dll name conv bytes).
Unimplemented imports still get the right stack cleanup (crates/sake/src/api.rs).
usage: gen-specs.py <wine-src> > specs.txt"""
import re, sys, os
DLLS = """kernel32 kernelbase ntdll user32 gdi32 advapi32 ole32 oleaut32 comctl32 comdlg32 shell32 shlwapi version
winmm imm32 dsound ddraw msvcrt msvcp60 ws2_32 wsock32 d3d8 d3d9 dinput dinput8 msacm32 msvfw32 avifil32
quartz uxtheme msimg32 winspool.drv rpcrt4 setupapi cfgmgr32 mpr wininet urlmon dwmapi psapi powrprof crypt32 secur32 hid
iphlpapi userenv winhttp gdiplus usp10 dbghelp xinput1_3 vorbisfile
oleacc imagehlp shfolder compstui uiautomationcore atl cabinet propsys mfreadwrite msi
xaudio2_7 xaudio2_8 xaudio2_9 x3daudio1_7 olepro32 windowscodecs crtdll glu32 dhcpcsvc dnsapi ncrypt tbs dxva2
ksproxy.ax msdmo hhctrl.ocx
d3d10 d3d10_1 msvcr70 msvcr71 msvcr80 msvcr90 msvcr100 msvcr110 msvcr120 vcruntime140 vcomp vcomp90 vcomp100 vcomp110 vcomp120 vcomp140 msvcp120 msvcp140""".split() + [f'd3dx9_{v}' for v in range(24, 44)] + [f'd3dcompiler_{v}' for v in range(33, 48)]
SIZES = {'word': 4, 'long': 4, 'ptr': 4, 'str': 4, 'wstr': 4, 'segptr': 4, 'segstr': 4, 'int64': 8,
         'double': 8, 'float': 4, 'int128': 16, 'long64': 8}
src = sys.argv[1]
for d in DLLS:
    p = os.path.join(src, 'dlls', d, d + '.spec')
    if not os.path.exists(p):
        p = os.path.join(src, 'dlls', d, d.split('.')[0] + '.spec')
    if not os.path.exists(p):
        continue
    for line in open(p, encoding='utf-8', errors='replace'):
        line = line.split('#')[0].strip()
        m = re.match(r'(@|\d+)\s+(stdcall|cdecl|varargs|thiscall|fastcall)\s+((?:-\S+\s+)*)(\S+?)\s*\(([^)]*)\)', line)
        if not m:
            continue
        ordinal, conv, flags, name, args = m.groups()
        if '-arch=' in flags and not re.search(r'-arch=\S*(win32|i386)', flags):
            continue
        if '-arch=win64' in flags:
            continue
        # (Wine's own exports, wine_get_version, __wine_dbg_*...: not Windows', so not sake's)
        if re.match(r'_*wine_', name):
            continue
        n = sum(SIZES.get(a, 4) for a in args.split())
        # (i386 fastcall: the first two 4-byte arguments come in ECX and EDX)
        if conv == 'fastcall' or '-fastcall' in flags:
            n -= sum(4 for a in args.split()[:2] if SIZES.get(a, 4) == 4)
            conv = 'stdcall'
        dll = d.split('.')[0]
        print(dll, name, conv, n, ordinal if ordinal != '@' else '-')

# msvcp120 / msvcp140 functions Wine only stubs (move constructors, set_rdbuf...): their stack
# bytes from the demangled x86 signature (llvm-undname), for sake's own implementations of them.
# A by-value class argument (size unknown here) leaves the function out.
import subprocess
def param_bytes(sig):
    # the parameter list: the last top-level (...) of the demangled name
    depth, end, start = 0, None, None
    for i in range(len(sig) - 1, -1, -1):
        ch = sig[i]
        if ch == ')':
            if depth == 0:
                end = i
            depth += 1
        elif ch == '(':
            depth -= 1
            if depth == 0:
                start = i
                break
    if start is None:
        return None
    params, depth, cur = [], 0, ''
    for ch in sig[start + 1:end]:
        if ch in '<(':
            depth += 1
        elif ch in '>)':
            depth -= 1
        if ch == ',' and depth == 0:
            params.append(cur.strip()); cur = ''
        else:
            cur += ch
    params.append(cur.strip())
    n = 0
    for p in params:
        if p in ('void', ''):
            continue
        if p == '...':
            return None
        if p.endswith('*') or p.endswith('&') or p.endswith('* const') or '(__' in p:
            n += 4
        elif re.search(r'\b(__int64|double)$', p):
            n += 8
        elif p.startswith(('class ', 'struct ', 'union ')):
            return None
        else:
            n += 4
    return n
def hidden_bytes(sig, conv):
    """`this` (on the stack for a __cdecl member) and the hidden pointer a class is returned by"""
    n = 0
    member = re.match(r'(public|private|protected): ', sig) and ': static ' not in sig and not re.match(r'\w+: static ', sig)
    if member:
        n += 4
    ret = re.sub(r'^(public|private|protected): (virtual |static )?', '', sig).split(' __')[0]
    if re.match(r'(class|struct|union) ', ret) and not ret.rstrip().endswith(('*', '&', '* const')):
        n += 4
    return n
def demangle(names):
    out = subprocess.run(['llvm-undname'], input='\n'.join(names) + '\n', capture_output=True, text=True).stdout
    # (llvm-undname echoes each name, then its demangling)
    sigs = [l for l in out.splitlines() if l and not l.startswith('?')]
    if len(sigs) != len(names):
        sys.exit(f'{len(names)} names, {len(sigs)} demangled')
    return sigs
def stub_bytes(name, sig):
    if sig.startswith('error') or '__thiscall' not in sig and '__cdecl' not in sig and '__stdcall' not in sig:
        return None
    n = param_bytes(sig)
    if n is None:
        return None
    conv = 'thiscall' if '__thiscall' in sig else 'stdcall' if '__stdcall' in sig else 'cdecl'
    n += hidden_bytes(sig, conv)
    # (a constructor of a class with a virtual base, the streams: MSVC's hidden "most derived"
    # int, as Wine's own lines count it)
    if re.match(r'\?\?0\?\$basic_(i|o|io|if|of|f|istring|ostring|string)stream@', name):
        n += 4
    return conv, n
if '--check-stubs' in sys.argv:
    # the rule against Wine's implemented i386 lines (one known Wine miscount: the char32_t
    # codecvt(_Locinfo const&, unsigned long, _Codecvt_mode, size_t) constructor)
    rows = []
    for line in open(os.path.join(src, 'dlls', 'msvcp140', 'msvcp140.spec')):
        m = re.match(r'@\s+(thiscall|cdecl)\s+-arch=(i386|win32)\s+(\?\S+?)\((.*?)\)', line)
        if m:
            rows.append((m.group(3), sum(SIZES.get(a, 4) for a in m.group(4).split())))
    bad = [(n, w, r) for (n, w), s in zip(rows, demangle([r[0] for r in rows])) for r in [stub_bytes(n, s)] if r and r[1] != w]
    for b in bad:
        print('mismatch', *b, file=sys.stderr)
    sys.exit(1 if len(bad) > 1 else 0)
for d in ['msvcp120', 'msvcp140']:
    p = os.path.join(src, 'dlls', d, d + '.spec')
    names = []
    for line in open(p, encoding='utf-8', errors='replace'):
        m = re.match(r'@\s+stub\s+((?:-\S+\s+)*)(\?\S+)', line.split('#')[0].strip())
        if m and (re.search(r'-arch=\S*(win32|i386)', m.group(1)) or '-arch=' not in m.group(1)):
            names.append(m.group(2))
    if not names:
        continue
    for name, sig in zip(names, demangle(names)):
        r = stub_bytes(name, sig)
        if r:
            print(d, name, r[0], r[1], '-')
