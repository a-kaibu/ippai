#!/usr/bin/env python3
"""Makes sake's C:\\windows\\Fonts from the IPA fonts (Debian/Ubuntu fonts-ipafont-gothic,
fonts-ipafont-mincho): IPA Gothic / Mincho renamed to the Windows fonts games ask for
(MS Gothic, MS PGothic, MS UI Gothic, MS Mincho, MS PMincho), with the IPA license.
No Wine and nothing downloaded from GitHub: CI makes them on the runner.

  tools/ci/sake-fonts.py OUT_DIR [IPA_DIR...]     (needs fontTools: pip install fonttools)
"""
import glob, os, shutil, sys
from fontTools.ttLib import TTFont

out = sys.argv[1]
dirs = sys.argv[2:] or glob.glob('/usr/share/fonts/opentype/ipafont-*') + glob.glob('/usr/share/fonts/truetype/ipafont-*')

def find(name):
    for d in dirs:
        p = os.path.join(d, name)
        if os.path.exists(p): return os.path.realpath(p)
    sys.exit(f'{name} not found in {dirs} (apt install fonts-ipafont-gothic fonts-ipafont-mincho)')

# file, source, English family, Japanese family, PostScript name
FONTS = [
    ('msgothic.ttf', 'ipag.ttf', 'MS Gothic', 'ＭＳ ゴシック', 'MSGothic'),
    ('mspgothic.ttf', 'ipagp.ttf', 'MS PGothic', 'ＭＳ Ｐゴシック', 'MSPGothic'),
    ('msuigothic.ttf', 'ipagp.ttf', 'MS UI Gothic', 'MS UI Gothic', 'MSUIGothic'),
    ('msmincho.ttf', 'ipam.ttf', 'MS Mincho', 'ＭＳ 明朝', 'MSMincho'),
    ('mspmincho.ttf', 'ipamp.ttf', 'MS PMincho', 'ＭＳ Ｐ明朝', 'MSPMincho'),
]
os.makedirs(out, exist_ok=True)
for file, src, en, ja, ps in FONTS:
    f = TTFont(find(src))
    name = f['name']
    name.names = [n for n in name.names if n.nameID not in (1, 2, 3, 4, 6, 16, 17)]
    for lang, fam in ((0x409, en), (0x411, ja)):
        for nid, s in ((1, fam), (2, 'Regular'), (4, fam)):
            name.setName(s, nid, 3, 1, lang)
    name.setName(ps, 3, 3, 1, 0x409)
    name.setName(ps, 6, 3, 1, 0x409)
    f.save(os.path.join(out, file))
lic = glob.glob('/usr/share/doc/fonts-ipafont-gothic/copyright')
if lic: shutil.copy(lic[0], os.path.join(out, 'IPA_Font_License.txt'))
open(os.path.join(out, 'README.txt'), 'w').write(
    'msgothic, mspgothic, msuigothic, msmincho, mspmincho .ttf: IPA fonts (IPAGothic, IPAPGothic, IPAMincho,\n'
    'IPAPMincho) under the names of the Windows fonts games ask for. License: IPA_Font_License.txt.\n')
print(f'{len(FONTS)} fonts in {out}')
