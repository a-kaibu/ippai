#!/usr/bin/env python3
"""Argument sizes of Win32 exports from Wine's .spec files -> specs.txt (dll name conv bytes).
Unimplemented imports still get the right stack cleanup (crates/sake/src/api.rs).
usage: gen-specs.py <wine-src> > specs.txt"""
import re, sys, os
DLLS = """kernel32 kernelbase ntdll user32 gdi32 advapi32 ole32 oleaut32 comctl32 comdlg32 shell32 shlwapi version
winmm imm32 dsound ddraw msvcrt msvcp60 ws2_32 wsock32 d3d8 d3d9 dinput dinput8 msacm32 msvfw32 avifil32
quartz uxtheme winspool.drv rpcrt4 setupapi mpr wininet urlmon dwmapi psapi powrprof crypt32 secur32 hid
iphlpapi userenv winhttp gdiplus usp10 dbghelp xinput1_3 vorbisfile
oleacc imagehlp shfolder compstui uiautomationcore atl cabinet propsys mfreadwrite msi""".split()
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
        n = sum(SIZES.get(a, 4) for a in args.split())
        dll = d.split('.')[0]
        print(dll, name, conv, n, ordinal if ordinal != '@' else '-')
