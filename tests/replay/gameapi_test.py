#!/usr/bin/env python3
"""gameapi.py on made-up exes and DLLs, and on the replay's own exes (mingw's: their imports
are d3d9 / d3d8 / d3d11 by name, replay.c loads them with LoadLibrary)."""
import os
import random
import struct
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import gameapi  # noqa: E402

fails = 0


def expect(what, got, want):
    global fails
    if got != want:
        print(f"gameapi_test: {what}: got {got!r}, want {want!r}")
        fails += 1


def pe(machine, body=b""):
    h = bytearray(0x80)
    h[:2] = b"MZ"
    struct.pack_into("<I", h, 0x3C, 0x40)
    h[0x40:0x44] = b"PE\0\0"
    struct.pack_into("<H", h, 0x44, machine)
    return bytes(h) + body


expect("32", gameapi.bits(pe(0x14C)), 32)
expect("64", gameapi.bits(pe(0x8664)), 64)
expect("arm64", gameapi.bits(pe(0xAA64)), None)
expect("not MZ", gameapi.bits(b"ZM" + pe(0x14C)[2:]), None)
expect("no PE", gameapi.bits(b"MZ" + bytes(0x7e)), None)
expect("short", gameapi.bits(b"MZ"), None)
expect("import", gameapi.apis(b"..KERNEL32.dll\0D3D9.DLL\0.."), {"d3d9"})
expect("utf-16", gameapi.apis("x d3d8.dll y".encode("utf-16-le")), {"d3d8"})
expect("d3d11", gameapi.apis(b"d3d11.dll"), {"dxgi"})
expect("dxgi", gameapi.apis(b"DXGI.dll"), {"dxgi"})
expect("not d3d9x", gameapi.apis(b"d3dx9_43.dll d3d10.dll"), set())
expect("ddraw", gameapi.apis(b"DDRAW.DLL"), {"ddraw"})
expect("d3d9 before ddraw", gameapi.pick({"ddraw", "d3d9"}), "d3d9")
expect("newest", gameapi.pick({"d3d8", "d3d9", "dxgi"}), "dxgi")
expect("d3d9 over 8", gameapi.pick({"d3d8", "d3d9"}), "d3d9")
expect("none", gameapi.pick(set()), "none")
# (random bytes around a name: found; without one: none of them, whatever the noise)
for seed in range(300):
    r = random.Random(seed)
    noise = lambda: bytes(r.choice(b"abcdefghijklmnopqrstuvwxyz0123456789.\0 ") for _ in range(r.randrange(40)))
    name = r.choice([b"d3d9.dll", b"D3D8.dll", b"d3d11.DLL", b"dxgi.dll"])
    want = {b"d3d9.dll": "d3d9", b"d3d8.dll": "d3d8", b"d3d11.dll": "dxgi", b"dxgi.dll": "dxgi"}[name.lower()]
    expect(f"noise {seed}", want in gameapi.apis(noise() + name + noise()), True)
    junk = noise().replace(b"dll", b"dl_")
    expect(f"junk {seed}", gameapi.apis(junk), set())

with tempfile.TemporaryDirectory() as d:
    os.makedirs(os.path.join(d, "g"))
    open(os.path.join(d, "g", "game.exe"), "wb").write(pe(0x14C, b"user32.dll\0"))
    expect("no d3d", gameapi.game(d, "g/game.exe"), ("none", 32))
    open(os.path.join(d, "g", "render.DLL"), "wb").write(pe(0x14C, b"d3d9.dll\0"))
    open(os.path.join(d, "g", "notes.txt"), "wb").write(b"d3d11.dll")
    expect("a DLL beside it", gameapi.game(d, "g/game.exe"), ("d3d9", 32))
    open(os.path.join(d, "g", "game.exe"), "wb").write(pe(0x8664, b"d3d8.dll\0"))
    expect("the exe's own first", gameapi.game(d, "g/game.exe"), ("d3d8", 64))

# next: what run 42380412096's watcher saw (record-game.ps1's lines)
W = "D:\\a\\masu\\masu\\game\\x\\g\\"
S = "C:\\Windows\\SYSTEM32\\"
suzu = [f"s: {S}d3d12.dll", f"s: {S}dxgi.dll", f"s: {S}d3d11.dll", f"s: {S}d3d8.dll", f"s: {S}d3d8thk.dll", f"s: {S}DDRAW.dll", f"s: {S}D3DIM700.DLL"]
expect("next: d3d9 guessed, d3d8 used (suzunone7)", gameapi.next_api(suzu, {"d3d9"}), "d3d8")
expect("next: d3d8 tried too", gameapi.next_api(suzu, {"d3d9", "d3d8"}), "ddraw")
hook = [f"h: {W}d3d8.dll", f"h: {S}ddraw.dll", f"h: {S}dxgi.dll", f"h: {S}d3d10warp.dll"]
expect("next: the wrapper loaded, DirectDraw drew (hook)", gameapi.next_api(hook, {"d3d8"}), "ddraw")
expect("next: only the wrapper (alicesoft)", gameapi.next_api([f"a: {W}d3d9.dll"], {"d3d9"}), "")
expect("next: nothing seen", gameapi.next_api([], {"d3d9"}), "")
expect("next: DirectDraw tried too", gameapi.next_api(hook, {"d3d8", "ddraw"}), "")
expect("next: d3d11 without ddraw", gameapi.next_api([f"x: {S}dxgi.dll", f"x: {S}d3d11.dll"], {"d3d9"}), "dxgi")
expect("next: SysWOW64, any case", gameapi.next_api(["x: c:\\windows\\syswow64\\D3D9.DLL"], {"d3d8"}), "d3d9")
expect("next: a d3d9.dll elsewhere is not the system's", gameapi.next_api([f"x: {W}d3d9.dll", "x: C:\\Program Files\\x\\d3d9.dll"], {"d3d8"}), "")

# (the replay's exes, when built: replay-32.exe names them all)
rp = os.environ.get("REPLAY_EXE")
if rp:
    data = open(rp, "rb").read()
    expect("replay exe", (gameapi.apis(data), gameapi.bits(data)), ({"d3d8", "d3d9", "dxgi"}, 32 if rp.endswith("32.exe") else 64))

if fails:
    sys.exit(1)
print("gameapi_test: ok")
