# wingfx case language and output

A **case file** (`cases/<suite>.txt`) is plain text, one call per line, tokens separated by
spaces. `# ` starts a comment (and a line starting with `#` is one); `#RRGGBB` is a color.
Numbers are decimal (`-3`), or hex with `0x`; never write a leading zero (C's `strtoul` reads
it as octal). A case runs from `case <name>` to `end`; everything it made is released at `end`,
so cases are independent and their order does not matter.

The executor prints `wingfx <suite>`, then for each case `case <name>` and the lines its calls
print, then `done`. A line it cannot run prints `bad line <n>` (n counts every line of the
file from 1). Output is flushed at the end of each case: a crash keeps the finished cases and
has no `done`.

## GDI (DIB sections)

Eight slots, 0..7; each holds a DIB section selected into a memory DC of its own, with that
DC's state (a memory DC starts with black text color, white background color, OPAQUE, the
white stock brush, brush origin 0,0, no clip region).

| line | calls | prints |
|---|---|---|
| `dib <s> <w> <h> <bpp> [td\|bu] [565] [#c0 #c1 ...]` | CreateDIBSection (bpp 1, 4, 8, 16, 24, 32; top-down by default; `565` = BI_BITFIELDS 5-6-5 for 16 bpp, else 5-5-5) with the color table given or the default one, and the pixels zeroed | `dib failed <error>` on failure |
| `fill <s> <seed>` | every byte of the pixel memory, in memory order, from xorshift32 (below) | |
| `poke <s> <x> <y> <hex>` | raw bytes of one pixel (bpp >= 8) | |
| `dump <s>` | GdiFlush, then the picture | `px <s> <w> <h> <bpp>`, then each row top to bottom as hex bytes, only the bytes that hold the row's w pixels |
| `brush <s> solid #c` / `hatch <style> #c` / `dib <slot>` / `null` | CreateSolidBrush / CreateHatchBrush / CreateDIBPatternBrushPt of a packed BI_RGB copy of a slot's picture (DIB_RGB_COLORS) / the NULL_BRUSH stock object, selected | `brush failed` |
| `text <s> #c`, `bk <s> #c`, `bkmode <s> <1\|2>`, `org <s> <x> <y>` | SetTextColor, SetBkColor, SetBkMode, SetBrushOrgEx | |
| `clip <s> <x0> <y0> <x1> <y1>`, `noclip <s>` | IntersectClipRect, SelectClipRgn(NULL) | `clip <region type>` |
| `patblt <s> <x> <y> <w> <h> <rop>` | PatBlt | `ret <0\|1>` |
| `bitblt <d> <x> <y> <w> <h> <s> <sx> <sy> <rop>` | BitBlt | `ret <0\|1>` |
| `stretchblt <d> <x> <y> <w> <h> <s> <sx> <sy> <sw> <sh> <rop>`, `mode <s> <m>` | StretchBlt, SetStretchBltMode (a memory DC starts with BLACKONWHITE) | `ret <0\|1>` |
| `getpixel <s> <x> <y>`, `setpixel <s> <x> <y> #c` | GetPixel, SetPixel | `pixel RRGGBB` or `pixel invalid` (CLR_INVALID) |
| `getdibits <s> <bpp> [565]` | GetDIBits of the slot's bitmap (deselected meanwhile) into a top-down DIB of that format with the default color table | `dibits <lines>`, for bpp <= 8 `pal` and the color table GetDIBits wrote, then the rows as `dump` prints them |
| `setdibits <s> <bpp> [565] <seed>` | SetDIBits from a top-down DIB of that format (default color table), its bytes from xorshift32 | `setdibits <lines>` |

Default color tables: 1 bpp black, white; 4 bpp the 16 VGA colors (000000 800000 008000
808000 000080 800080 008080 c0c0c0 808080 ff0000 00ff00 ffff00 0000ff ff00ff 00ffff ffffff);
8 bpp entry i = (r, g, b) = ((i>>5&7)*255/7, (i>>2&7)*255/7, (i&3)*255/3), integer division.

xorshift32: state = seed (0 becomes 0x9e3779b9); per byte `x ^= x << 13; x ^= x >> 17;
x ^= x << 5`, the byte is the low 8 bits of x.

## Direct3D 9

| line | calls | prints |
|---|---|---|
| `device [hwvp\|swvp\|mixed]` | Direct3DCreate9 (d3d9.dll loaded when first needed), CreateDevice HAL, windowed, 64x64 X8R8G8B8, auto depth stencil D24S8 (HAL software vertex processing if the one asked is refused) | `device ok`, `device none` (no Direct3D 9), `device failed <hr>` |
| `rs get <state>`, `tss get <stage> <type>`, `ss get <sampler> <type>` | Get{Render,TextureStage,Sampler}State | `rs <state> = <value>` (8 hex digits), or `... hr <hr>` |
| `rs set <state> <v>`, `tss set <stage> <type> <v>`, `ss set <sampler> <type> <v>` | Set...State | `... set hr <hr>` only on failure |
| `sb begin`, `sb end <i>`, `sb create <i> <type>`, `sb capture <i>`, `sb apply <i>`, `sb release <i>` | BeginStateBlock, EndStateBlock into block i (0..7), CreateStateBlock, Capture, Apply, Release | `sb <verb> hr <hr>` on failure; `sb <i> none` when block i does not exist |

`device8 [hwvp|swvp|mixed]` makes a Direct3D 8 device the same way (`device8 ok` / `none` / `failed <hr>`); while it is the current device, `rs` and `tss` (stage state types 0..29: Direct3D 8's sampler states are stage states) and `sb` act on it (state blocks are DWORD tokens: Create / End / Capture / Apply / DeleteStateBlock), and `ss` is a bad line.

Without a device the state lines print `nodev`. (Not asked by the generators: stages past 7,
samplers past 15 other than 256..260, sampler state types past 13: Windows reads past its arrays.)

## Direct3D 11

| line | calls | prints |
|---|---|---|
| `device11` | D3D11CreateDevice (d3d11.dll loaded when first needed), hardware (WARP if refused), feature level 11_0 only | `device11 ok`, `device11 none`, `device11 failed <hr>` |
| `blend\|depth\|raster\|sampler [check\|nodesc] <words>` | Create{Blend,DepthStencil,Rasterizer,Sampler}State of the description given as its 32-bit words (66, 13, 10, 13; missing trailing words 0; UINT8 fields with their padding in one word), then GetDesc; `check`: ppState NULL; `nodesc`: pDesc NULL and no words | `<kind> hr <hr>` on failure and for `check` / `nodesc`, else `<kind> = <GetDesc's words> same <n\|->` (hex, the UINT8 fields without padding; n: the case's earlier state line, counted from 0, that gave the same object) |

Without a device these lines print `nodev`. A case holds at most 64 state lines.

## WIC

| line | calls | prints |
|---|---|---|
| `convert <from> <w> <h> <seed\|ramp> <to>` | CreateBitmapFromMemory of format `from` (w 1..256, h 1..64, rows of (w * bits + 7) / 8 bytes from xorshift32, or byte i = i mod 256 for `ramp`; indexed formats get the palette below), CreateFormatConverter, CanConvert, Initialize to `to` (WICBitmapDitherTypeNone; the palette for indexed targets), CopyPixels of the whole picture | `conv <w> <h> can <0\|1>` and each row's bytes as hex (the bits after the row's last pixel 0); `conv can <0\|1> init hr <hr>` / `copy hr <hr>`, `conv bitmap hr <hr>`, `conv none` (no WIC) |

Formats: `bw gray2 gray4 gray8 gray16 bgr555 bgr565 bgr24 rgb24 bgr32 bgra pbgra rgb32 rgba prgba rgb48
rgba64 cmyk alpha8 idx1 idx2 idx4 idx8` (GUID_WICPixelFormat...). The palette: 16 colors, entry i =
ARGB (i == 5 ? 0x40 : 0xff, i * 17, 255 - i * 13, i * 71 mod 256).

## Models

`wingfx model <suite>` prints what Windows prints, case by case. A case whose behavior the
documentation does not settle and Windows' answers have not shown yet prints
`unmodeled <why>` instead; it is then checked against Windows' recorded answers only.
