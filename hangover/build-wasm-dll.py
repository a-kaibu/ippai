#!/usr/bin/env python3
"""Builds Wine PE DLLs straight to wasm (see docs/wasm-dlls.md).

    hangover/build-wasm-dll.py [--out DIR] user32 gdi32 ...

Each DLL's sources are compiled with the flags of vnbox's i386 build (its compile_commands.json)
for wasm32 with the i386 ABI (-D__i386__, 16-bit wchar_t), linked with `wasm-ld -shared`
together with the static libraries the x86 DLL links (libpng, ...) and hangover/wasmdll/crt.c,
and post-processed by crates/vnwasm against the x86 DLL and its .spec file. The result,
DIR/<dll>.wasm, goes next to the x86 DLLs as lib/wine/i386-wasm/<dll>.wasm.

env: WINE_BUILD (vnbox's Wine build tree), WINEROOT (the Wine root with the x86 DLLs),
     EMSDK4, VNWASM (the vnwasm binary)
"""
import json, os, re, shlex, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
B = os.environ.get('WINE_BUILD', '/tmp/vnbox-build/wine-build-src/build')
WINEROOT = os.environ.get('WINEROOT', '/home/user/work/wineroot')
EMSDK = os.environ.get('EMSDK4', '/home/user/emsdk4')
CLANG = EMSDK + '/upstream/bin/clang'
WASMLD = EMSDK + '/upstream/bin/wasm-ld'
NM = EMSDK + '/upstream/bin/llvm-nm'
VNWASM = os.environ.get('VNWASM', ROOT + '/target/release/vnwasm')

# gcc/x86-only flags of the PE build
DROP = {'-mpreferred-stack-boundary=2', '-mlong-double-64', '-msse2', '-fno-omit-frame-pointer'}
WASM_FLAGS = ['--target=wasm32-unknown-unknown', '-D__i386__', '-D__vnwasm_pe__', '-DDECLSPEC_IMPORT=', '-D_WIN32', '-DWIN32',
              '-fshort-wchar', '-fPIC', '-matomics', '-mbulk-memory', '-mmutable-globals', '-msign-ext',
              '-mnontrapping-fptoint', '-fms-extensions', '-fdeclspec', '-nostdlibinc', '-fno-builtin-printf', '-D__builtin_return_address(x)=((void *)0)', '-DSYMCRYPT_IGNORE_PLATFORM',
              '-w']

# sources left out (x86 through and through: exception dispatch, contexts, x87 ABI helpers);
# their exports stay x86
EXCLUDE = {}

def link_rule(dll):
    """(spec, objects, static libraries) of dlls/.../i386-windows/<dll>.dll in the Makefile"""
    mk = open(B + '/Makefile').read()
    m = re.search(r'^(dlls/[^\s:]+/i386-windows/%s(?:\.dll|\.drv|\.sys)?):((?:.*\\\n)*.*)\n' % re.escape(dll), mk, re.M)
    if not m:
        sys.exit('no link rule for %s' % dll)
    deps = m.group(2).replace('\\\n', ' ').split()
    spec = next(d for d in deps if d.endswith('.spec'))
    objs = [d for d in deps if d.endswith('.o') and os.path.basename(d) not in EXCLUDE.get(dll, ())]
    libs = [d for d in deps if d.endswith('.a') and d.startswith('libs/')
            and not any(x in d for x in ('winecrt0', 'compiler-rt'))]
    return m.group(1), spec, objs, libs

def commands():
    cc = json.load(open(B + '/compile_commands.json'))
    out = {}
    for e in cc:
        a = shlex.split(e['command'])
        if '-o' in a:
            out[a[a.index('-o') + 1]] = (e['directory'], a, e['file'])
    return out

def compile_one(cmds, obj, outdir):
    d, a, src = cmds[obj]
    dst = os.path.join(outdir, obj.replace('/', '_'))
    if os.path.exists(dst) and os.path.getmtime(dst) > os.path.getmtime(os.path.join(d, src)) \
            and os.path.getmtime(dst) > os.path.getmtime(__file__):
        return dst, None
    args = [x for x in a[1:] if x not in DROP and not x.startswith('-W')]
    args[args.index('-o') + 1] = dst
    r = subprocess.run([CLANG] + WASM_FLAGS + args, cwd=d, capture_output=True, text=True)
    if r.returncode:
        return dst, '%s:\n%s' % (obj, '\n'.join(l for l in r.stderr.split('\n') if 'error' in l and not l.startswith(' #'))[:3000])
    return dst, None

def lib_objects(cmds, lib):
    pre = os.path.dirname(lib) + '/'
    # not the x86 assembly versions (symcrypt's; the C ones are used, SYMCRYPT_IGNORE_PLATFORM)
    return sorted(o for o in cmds if o.startswith(pre) and o.endswith('.o')
                  and not cmds[o][2].endswith(('.S', '.s', '.cppasm')) and '/i386/' not in cmds[o][2])

def libm(out):
    """musl's math functions (from emscripten's copy) for what the x86 build got from gcc's
    builtins or x87 instructions (exp2, ...); only pulled in where a DLL needs them"""
    musl = EMSDK + '/upstream/emscripten/system/lib/libc/musl'
    rt = EMSDK + '/upstream/emscripten/system/lib/compiler-rt/lib/builtins'
    srcs = sorted(musl + '/src/math/' + f for f in os.listdir(musl + '/src/math') if f.endswith('.c'))
    # and compiler-rt's helpers (__multi3, ...)
    srcs += sorted(rt + '/' + f for f in os.listdir(rt) if f.endswith('.c') and f.endswith(('ti3.c', 'ti4.c', 'di3.c', 'di4.c')))
    objdir = os.path.join(out, 'obj', 'libm')
    os.makedirs(objdir, exist_ok=True)
    inc = ['-I' + musl + d for d in ('/src/include', '/src/internal', '/arch/emscripten', '/arch/generic', '/include')] + \
          ['-I' + EMSDK + '/upstream/emscripten/system/lib/libc', '-I' + EMSDK + '/upstream/emscripten/cache/sysroot/include']
    def one(f):
        dst = os.path.join(objdir, os.path.basename(f)[:-2] + ('.rt.o' if '/builtins/' in f else '.o'))
        if not os.path.exists(dst):
            r = subprocess.run([CLANG, '--target=wasm32-unknown-unknown', '-fPIC', '-matomics', '-mbulk-memory',
                                '-O2', '-w', '-nostdlibinc', '-c', '-o', dst, f] + inc,
                               capture_output=True, text=True)
            if r.returncode:
                return None
        return dst
    with ThreadPoolExecutor(os.cpu_count()) as ex:
        return [o for o in ex.map(one, srcs) if o]

def build(dll, out, cmds):
    target, spec, objs, libs = link_rule(dll)
    name = os.path.basename(target)
    objdir = os.path.join(out, 'obj', dll)
    os.makedirs(objdir, exist_ok=True)
    # winecrt0's portable parts (debug channels, unix calls); its entry point and the x86
    # helpers are replaced by hangover/wasmdll/crt.c
    # (not for ntdll: it has its own, and the x86 image's unix call dispatcher is the one set up)
    crt0 = [] if dll == 'ntdll' else ['libs/winecrt0/i386-windows/%s.o' % n for n in ('debug', 'unix_lib')]
    units = objs + [o for l in libs for o in lib_objects(cmds, l)] + crt0
    with ThreadPoolExecutor(os.cpu_count()) as ex:
        res = list(ex.map(lambda o: compile_one(cmds, o, objdir), units))
    errs = [e for _, e in res if e]
    if errs:
        sys.exit('\n'.join(errs))
    ofiles = [f for f, _ in res]
    dllmain = subprocess.run([NM, '--defined-only', '-j'] + [f for f, _ in res[:len(objs)]],
                             capture_output=True, text=True).stdout.split()
    crt = os.path.join(objdir, 'vnwasm_crt.o')
    inc = ['-I' + B + '/include', '-I' + os.path.join(os.path.dirname(os.path.dirname(spec)), '..', 'include')]
    src_inc = re.search(r'-I(\S+/wine-11\.20/include)\b', ' '.join(cmds[objs[0]][1])).group(1)
    subprocess.run([CLANG] + WASM_FLAGS + ['-O2', '-c', '-o', crt, HERE + '/wasmdll/crt.c', '-I' + B + '/include',
                    '-I' + src_inc, '-I' + src_inc + '/msvcrt', '-D__WINESRC__', '-D_UCRT'] +
                   (['-DVNWASM_DLLMAIN'] if 'DllMain' in dllmain else []), check=True)
    libc = os.path.join(objdir, 'vnwasm_libc.o')
    subprocess.run([CLANG] + WASM_FLAGS + ['-O2', '-fno-builtin', '-c', '-o', libc, HERE + '/wasmdll/libc.c'], check=True)
    raw = os.path.join(out, 'obj', name + '.raw.wasm')
    # static libraries' objects only where something needs them
    libobjs = ofiles[len(objs):] + [libc] + libm(out)
    r = subprocess.run([WASMLD, '--experimental-pic', '-shared', '--shared-memory', '--import-memory',
                        '--max-memory=4294967296', '-Bsymbolic', '--allow-undefined', '--no-entry',
                        '--export-all', '-o', raw] + ofiles[:len(objs)] + [crt] +
                       (['--start-lib'] + libobjs + ['--end-lib'] if libobjs else []),
                       capture_output=True, text=True)
    if r.returncode:
        sys.exit('%s: wasm-ld failed:\n%s' % (dll, r.stderr))
    x86 = os.path.join(WINEROOT, 'lib/wine/i386-windows', name)
    spec_path = spec if os.path.isabs(spec) else os.path.join(B, spec)
    dst = os.path.join(out, os.path.splitext(name)[0] + '.wasm') if False else os.path.join(out, name + '.wasm')
    r = subprocess.run([VNWASM, raw, x86, spec_path, '-o', dst])
    if r.returncode:
        sys.exit('%s: vnwasm failed' % dll)
    return dst

def main():
    argv = sys.argv[1:]
    out = os.path.join(os.environ.get('HOME', '/tmp'), 'work/wasm-dlls')
    if argv[:1] == ['--out']:
        out, argv = argv[1], argv[2:]
    os.makedirs(out, exist_ok=True)
    cmds = commands()
    for dll in argv:
        print(build(dll, out, cmds))

main()
