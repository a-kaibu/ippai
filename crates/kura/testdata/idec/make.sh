#!/bin/bash
# planes.bin: Chrome's ImageDecoder planes of JPEG files (as web/src/idec.ts copies them) with the
# RGB they decode to: Windows' (tests/winref/golden/imaging-32) for tests/winref's jpeg_*, and
# libjpeg-turbo's (Pillow) for random pictures of sizes from 1 x 1 up in 4:4:4, 4:2:2, 4:2:0.
# Records: u32 name length, name, u32 w, h, layout (kura::idec::Layout), bytes; planes; w*h*3 RGB.
#
#   crates/kura/testdata/idec/make.sh     (python3 with Pillow and numpy, playwright's Chromium)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
W=$(mktemp -d)
cd "$W"
python3 - "$ROOT" <<'PY'
import sys, re
from PIL import Image
import numpy as np
root = sys.argv[1]
h = open(f"{root}/tests/winref/probes/imaging_data.h").read()
for name, body in re.findall(r"static const unsigned char (jpeg_\w+)\[\d*\][^{]*\{([^}]*)\}", h):
    if name != "jpeg_cmyk":
        open(f"{name}.jpg", "wb").write(bytes(int(x, 16) for x in body.replace(",", " ").split()))
rng = np.random.default_rng(1)
for w, hh in [(1,1),(2,3),(3,2),(4,4),(5,7),(6,1),(1,6),(9,5),(17,9),(33,20),(64,33)]:
    for sub, n in [(0, "444"), (1, "422"), (2, "420")]:
        Image.fromarray((rng.random((hh, w, 3)) * 255).astype(np.uint8)).save(f"r{w}x{hh}_{n}.jpg", quality=85, subsampling=sub)
PY
# (ImageDecoder wants a secure context: a page of localhost)
PORT=${PORT:-8766}
python3 -m http.server "$PORT" >/dev/null 2>&1 &
trap "kill $! 2>/dev/null || true; rm -rf $W" EXIT
sleep 1
URL=http://localhost:$PORT/ node "$HERE/cap.mjs" planes.json *.jpg
python3 - "$ROOT" "$HERE/planes.bin" <<'PY'
import sys, json, base64, struct
import numpy as np
from PIL import Image
root, out = sys.argv[1], sys.argv[2]
d = json.load(open("planes.json"))
codes = {"I420": 1, "I422": 2, "I444": 3, "BGRX": 4, "RGBX": 5}
b = b""
for k in sorted(d):
    v = d[k]
    src = f"{root}/tests/winref/golden/imaging-32/{k[:-4]}.png" if k.startswith("jpeg_") else k
    rgb = np.asarray(Image.open(src).convert("RGB"))
    pl = base64.b64decode(v["b64"])
    name = k[:-4].encode()
    b += struct.pack("<I", len(name)) + name + struct.pack("<IIII", v["w"], v["h"], codes[v["fmt"]], len(pl)) + pl + rgb.tobytes()
open(out, "wb").write(b)
print(out, len(d), "cases")
PY
