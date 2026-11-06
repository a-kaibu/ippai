/* gdi32 drawing into a 32bpp DIB section, one picture per operation: lines (all octants, pens,
 * ROP2), shapes, polygons in both fill modes, regions and clipping, PatBlt/BitBlt ROPs,
 * StretchBlt modes, StretchDIBits from 8/16/24bpp, AlphaBlend, GradientFill, TransparentBlt,
 * GetDIBits conversions. libs: -lgdi32 -lmsimg32 */
#include "../probe.h"

#define W 64
#define H 64
static HDC dc;
static unsigned *px;
static HBITMAP bmp;

static void clear(void) {
    for (int i = 0; i < W * H; i++) px[i] = 0x404040;
    SelectObject(dc, GetStockObject(BLACK_PEN));
    SelectObject(dc, GetStockObject(WHITE_BRUSH));
    SetROP2(dc, R2_COPYPEN);
    SetPolyFillMode(dc, ALTERNATE);
    SetBkMode(dc, OPAQUE);
    SetBkColor(dc, RGB(255, 255, 255));
    SetTextColor(dc, RGB(0, 0, 0));
    SelectClipRgn(dc, NULL);
    SetStretchBltMode(dc, COLORONCOLOR);
    SetBrushOrgEx(dc, 0, 0, NULL);
}

static void shot(const char *name, int tol) {
    GdiFlush();
    probe_image(name, px, W, H, W * 4, tol);
}

/* a 16x16 source picture with distinct pixels */
static HDC src;
static unsigned *spx;

int main(void) {
    BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), W, -H, 1, 32, BI_RGB } };
    dc = CreateCompatibleDC(NULL);
    bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, (void **)&px, NULL, 0);
    SelectObject(dc, bmp);
    BITMAPINFO sbi = { { sizeof(BITMAPINFOHEADER), 16, -16, 1, 32, BI_RGB } };
    src = CreateCompatibleDC(NULL);
    SelectObject(src, CreateDIBSection(src, &sbi, DIB_RGB_COLORS, (void **)&spx, NULL, 0));
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++)
            spx[y * 16 + x] = (x * 16) << 16 | (y * 16) << 8 | ((x ^ y) & 1 ? 0xff : 0x20) | (unsigned)(x * 16 + y) << 24;

    clear();
    for (int a = 0; a < 16; a++) {
        static const int ex[] = { 63, 63, 63, 50, 32, 13, 0, 0, 0, 0, 0, 13, 32, 50, 63, 63 };
        static const int ey[] = { 32, 45, 63, 63, 63, 63, 63, 45, 32, 18, 0, 0, 0, 0, 0, 18 };
        MoveToEx(dc, 32, 32, NULL);
        LineTo(dc, ex[a], ey[a]);
    }
    shot("lines_octants", 0);
    clear();
    for (int i = 0; i < 8; i++) {
        HPEN p = CreatePen(i & 1 ? PS_DOT : PS_SOLID, i < 4 ? 1 : 3, RGB(255, 32 * i, 0));
        HGDIOBJ o = SelectObject(dc, p);
        MoveToEx(dc, 4 + 7 * i, 4, NULL);
        LineTo(dc, 10 + 6 * i, 60);
        SelectObject(dc, o);
        DeleteObject(p);
    }
    shot("pens", 0);
    {
        static const int rops[] = { R2_BLACK, R2_NOTMERGEPEN, R2_MASKNOTPEN, R2_NOTCOPYPEN, R2_MASKPENNOT, R2_NOT,
                                    R2_XORPEN, R2_NOTMASKPEN, R2_MASKPEN, R2_NOTXORPEN, R2_NOP, R2_MERGENOTPEN,
                                    R2_COPYPEN, R2_MERGEPENNOT, R2_MERGEPEN, R2_WHITE };
        clear();
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) px[y * W + x] = (x * 4) << 16 | (y * 4) << 8 | 0x5a;
        HPEN p = CreatePen(PS_SOLID, 3, RGB(0xc3, 0x3c, 0x99));
        SelectObject(dc, p);
        for (int i = 0; i < 16; i++) {
            SetROP2(dc, rops[i]);
            MoveToEx(dc, 2 + 4 * i, 2, NULL);
            LineTo(dc, 2 + 4 * i, 62);
        }
        SelectObject(dc, GetStockObject(BLACK_PEN));
        DeleteObject(p);
        shot("rop2", 0);
    }
    clear();
    Rectangle(dc, 2, 2, 20, 14);
    Ellipse(dc, 22, 2, 40, 20);
    Ellipse(dc, 42, 2, 61, 13);
    RoundRect(dc, 2, 22, 30, 40, 10, 8);
    Pie(dc, 32, 22, 62, 52, 62, 30, 32, 30);
    Arc(dc, 2, 42, 30, 62, 30, 52, 2, 52);
    Chord(dc, 34, 50, 62, 63, 62, 56, 34, 56);
    shot("shapes", 0);
    clear();
    {
        HPEN np = CreatePen(PS_NULL, 0, 0);
        HBRUSH b = CreateSolidBrush(RGB(200, 30, 60));
        SelectObject(dc, np);
        SelectObject(dc, b);
        Rectangle(dc, 2, 2, 20, 14);
        Ellipse(dc, 22, 2, 40, 20);
        POINT star[5] = { { 16, 24 }, { 26, 60 }, { 0, 36 }, { 32, 36 }, { 6, 60 } };
        Polygon(dc, star, 5);
        SetPolyFillMode(dc, WINDING);
        for (int i = 0; i < 5; i++) star[i].x += 30;
        Polygon(dc, star, 5);
        SelectObject(dc, GetStockObject(BLACK_PEN));
        DeleteObject(np);
        SelectObject(dc, GetStockObject(WHITE_BRUSH));
        DeleteObject(b);
    }
    shot("fills_nullpen", 0);
    clear();
    {
        POINT pl[6] = { { 2, 2 }, { 60, 10 }, { 10, 30 }, { 50, 60 }, { 2, 60 }, { 30, 5 } };
        Polyline(dc, pl, 6);
        POINT bz[4] = { { 2, 62 }, { 20, 0 }, { 44, 64 }, { 62, 2 } };
        PolyBezier(dc, bz, 4);
    }
    shot("polyline_bezier", 0);
    clear();
    {
        static const int hs[] = { HS_HORIZONTAL, HS_VERTICAL, HS_FDIAGONAL, HS_BDIAGONAL, HS_CROSS, HS_DIAGCROSS };
        for (int i = 0; i < 6; i++) {
            HBRUSH b = CreateHatchBrush(hs[i], RGB(0, 0, 255));
            RECT r = { (i % 3) * 21, (i / 3) * 21, (i % 3) * 21 + 20, (i / 3) * 21 + 20 };
            if (i == 3) SetBkMode(dc, TRANSPARENT);
            FillRect(dc, &r, b);
            DeleteObject(b);
        }
        WORD pat[8] = { 0xaa, 0x55, 0xf0, 0x0f, 0xcc, 0x33, 0xff, 0x00 };
        HBITMAP pb = CreateBitmap(8, 8, 1, 1, pat);
        HBRUSH b = CreatePatternBrush(pb);
        SetBkMode(dc, OPAQUE);
        SetTextColor(dc, RGB(255, 0, 0));
        SetBkColor(dc, RGB(0, 255, 0));
        SetBrushOrgEx(dc, 3, 1, NULL);
        RECT r = { 0, 44, 64, 64 };
        FillRect(dc, &r, b);
        DeleteObject(b);
        DeleteObject(pb);
    }
    shot("brushes", 0);
    clear();
    {
        HRGN a = CreateEllipticRgn(4, 4, 44, 44), b = CreateRectRgn(24, 24, 60, 60), c = CreateRectRgn(0, 0, 1, 1);
        static const int modes[] = { RGN_AND, RGN_OR, RGN_XOR, RGN_DIFF };
        HBRUSH br = CreateSolidBrush(RGB(0, 200, 0));
        for (int i = 0; i < 4; i++) {
            int t = CombineRgn(c, a, b, modes[i]);
            RECT bx;
            GetRgnBox(c, &bx);
            DWORD n = GetRegionData(c, 0, NULL);
            printf("CombineRgn %d -> %d box %ld %ld %ld %ld data %lu\n", modes[i], t, bx.left, bx.top, bx.right, bx.bottom, n);
            if (i == 2) FillRgn(dc, c, br);
        }
        printf("PtInRegion %d %d\n", PtInRegion(a, 24, 24), PtInRegion(a, 5, 5));
        HRGN rr = CreateRoundRectRgn(2, 2, 30, 30, 12, 12);
        FrameRgn(dc, rr, GetStockObject(BLACK_BRUSH), 1, 1);
        HRGN poly;
        POINT pts[3] = { { 40, 2 }, { 62, 30 }, { 34, 20 } };
        poly = CreatePolygonRgn(pts, 3, ALTERNATE);
        InvertRgn(dc, poly);
        DeleteObject(a); DeleteObject(b); DeleteObject(c); DeleteObject(rr); DeleteObject(poly); DeleteObject(br);
    }
    shot("regions", 0);
    clear();
    {
        HRGN clip = CreateEllipticRgn(8, 8, 56, 56);
        SelectClipRgn(dc, clip);
        ExcludeClipRect(dc, 28, 0, 36, 64);
        IntersectClipRect(dc, 0, 4, 64, 60);
        RECT box;
        printf("GetClipBox %d", GetClipBox(dc, &box));
        printf(" %ld %ld %ld %ld\n", box.left, box.top, box.right, box.bottom);
        HBRUSH br = CreateSolidBrush(RGB(255, 128, 0));
        RECT all = { 0, 0, 64, 64 };
        FillRect(dc, &all, br);
        DeleteObject(br);
        BitBlt(dc, 0, 0, 16, 16, src, 0, 0, SRCCOPY);
        DeleteObject(clip);
    }
    shot("clip", 0);
    clear();
    {
        static const DWORD rops[] = { SRCCOPY, SRCPAINT, SRCAND, SRCINVERT, SRCERASE, NOTSRCCOPY, NOTSRCERASE,
                                      MERGECOPY, MERGEPAINT, PATCOPY, PATPAINT, PATINVERT, DSTINVERT, BLACKNESS,
                                      WHITENESS, 0x00B8074A };
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) px[y * W + x] = (y * 4) << 16 | (x * 4) << 8 | 0xa5;
        HBRUSH br = CreateSolidBrush(RGB(0x33, 0xcc, 0x0f));
        SelectObject(dc, br);
        for (int i = 0; i < 16; i++) BitBlt(dc, (i % 4) * 16, (i / 4) * 16, 16, 16, src, 0, 0, rops[i]);
        SelectObject(dc, GetStockObject(WHITE_BRUSH));
        DeleteObject(br);
    }
    shot("bitblt_rops", 0);
    clear();
    {
        HBRUSH br = CreateSolidBrush(RGB(10, 100, 200));
        SelectObject(dc, br);
        PatBlt(dc, 4, 4, 30, 20, PATCOPY);
        PatBlt(dc, 20, 10, 30, 30, PATINVERT);
        PatBlt(dc, 40, 40, -20, -10, DSTINVERT);
        SelectObject(dc, GetStockObject(WHITE_BRUSH));
        DeleteObject(br);
        SetPixel(dc, 1, 1, RGB(1, 2, 3));
        SetPixelV(dc, 62, 62, RGB(250, 251, 252));
        printf("GetPixel %06lx %06lx %08lx\n", GetPixel(dc, 1, 1), GetPixel(dc, 30, 15), GetPixel(dc, 100, 100));
    }
    shot("patblt_pixels", 0);
    clear();
    StretchBlt(dc, 0, 0, 40, 40, src, 0, 0, 16, 16, SRCCOPY);
    StretchBlt(dc, 40, 0, 24, 12, src, 0, 0, 16, 16, SRCCOPY);
    StretchBlt(dc, 63, 63, -20, -20, src, 0, 0, 16, 16, SRCCOPY);
    StretchBlt(dc, 40, 14, 10, 10, src, 15, 0, -16, 16, SRCCOPY);
    shot("stretch_coloroncolor", 0);
    clear();
    SetStretchBltMode(dc, HALFTONE);
    StretchBlt(dc, 0, 0, 40, 40, src, 0, 0, 16, 16, SRCCOPY);
    StretchBlt(dc, 40, 0, 7, 9, src, 0, 0, 16, 16, SRCCOPY);
    shot("stretch_halftone", 48);
    clear();
    {
        /* 8bpp bottom-up with a palette, 16bpp 565, 24bpp with row padding */
        struct { BITMAPINFOHEADER h; RGBQUAD pal[256]; } b8 = { { sizeof(BITMAPINFOHEADER), 6, 5, 1, 8, BI_RGB } };
        for (int i = 0; i < 256; i++) b8.pal[i] = (RGBQUAD){ (BYTE)i, (BYTE)(255 - i), (BYTE)(i * 7), 0 };
        unsigned char d8[8 * 5];
        for (int i = 0; i < 40; i++) d8[i] = (unsigned char)(i * 9);
        StretchDIBits(dc, 0, 0, 24, 20, 0, 0, 6, 5, d8, (BITMAPINFO *)&b8, DIB_RGB_COLORS, SRCCOPY);
        struct { BITMAPINFOHEADER h; DWORD m[3]; } b16 = { { sizeof(BITMAPINFOHEADER), 5, -4, 1, 16, BI_BITFIELDS }, { 0xf800, 0x07e0, 0x001f } };
        unsigned short d16[6 * 4];
        for (int i = 0; i < 24; i++) d16[i] = (unsigned short)(i * 2741);
        StretchDIBits(dc, 30, 0, 20, 16, 0, 0, 5, 4, d16, (BITMAPINFO *)&b16, DIB_RGB_COLORS, SRCCOPY);
        BITMAPINFO b24 = { { sizeof(BITMAPINFOHEADER), 5, 3, 1, 24, BI_RGB } };
        unsigned char d24[16 * 3];
        for (int i = 0; i < 48; i++) d24[i] = (unsigned char)(i * 37);
        SetDIBitsToDevice(dc, 2, 30, 5, 3, 0, 0, 0, 3, d24, &b24, DIB_RGB_COLORS);
        StretchDIBits(dc, 10, 30, 25, 15, 0, 0, 5, 3, d24, &b24, DIB_RGB_COLORS, SRCCOPY);
        BITMAPINFO b1 = { { sizeof(BITMAPINFOHEADER), 16, 2, 1, 1, BI_RGB } };
        struct { BITMAPINFOHEADER h; RGBQUAD pal[2]; } m1 = { b1.bmiHeader, { { 0, 0, 255, 0 }, { 0, 255, 255, 0 } } };
        unsigned char d1[8] = { 0xa5, 0x0f, 0, 0, 0x3c, 0xf0, 0, 0 };
        StretchDIBits(dc, 40, 30, 16, 8, 0, 0, 16, 2, d1, (BITMAPINFO *)&m1, DIB_RGB_COLORS, SRCCOPY);
    }
    shot("stretchdibits", 0);
    clear();
    {
        BLENDFUNCTION pp = { AC_SRC_OVER, 0, 255, AC_SRC_ALPHA }, ca = { AC_SRC_OVER, 0, 100, 0 }, both = { AC_SRC_OVER, 0, 128, AC_SRC_ALPHA };
        /* per-pixel alpha wants premultiplied sources */
        unsigned pm[256];
        memcpy(pm, spx, sizeof pm);
        for (int i = 0; i < 256; i++) {
            unsigned a = pm[i] >> 24, r = (pm[i] >> 16 & 255) * a / 255, g = (pm[i] >> 8 & 255) * a / 255, b = (pm[i] & 255) * a / 255;
            spx[i] = a << 24 | r << 16 | g << 8 | b;
        }
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) px[y * W + x] = (x * 4) << 8 | (y * 4);
        AlphaBlend(dc, 0, 0, 16, 16, src, 0, 0, 16, 16, pp);
        AlphaBlend(dc, 20, 0, 16, 16, src, 0, 0, 16, 16, ca);
        AlphaBlend(dc, 40, 0, 16, 16, src, 0, 0, 16, 16, both);
        AlphaBlend(dc, 0, 20, 40, 40, src, 0, 0, 16, 16, pp);
        memcpy(spx, pm, sizeof pm);
    }
    shot("alphablend", 2);
    clear();
    {
        TRIVERTEX v[4] = { { 0, 0, 0xff00, 0, 0, 0 }, { 64, 20, 0, 0, 0xff00, 0 }, { 0, 24, 0, 0xff00, 0, 0 }, { 64, 64, 0xff00, 0xff00, 0xff00, 0 } };
        GRADIENT_RECT gr = { 0, 1 };
        GradientFill(dc, v, 2, &gr, 1, GRADIENT_FILL_RECT_H);
        TRIVERTEX t[3] = { { 0, 24, 0xff00, 0, 0, 0 }, { 63, 30, 0, 0xff00, 0, 0 }, { 20, 63, 0, 0, 0xff00, 0 } };
        GRADIENT_TRIANGLE gt = { 0, 1, 2 };
        GradientFill(dc, t, 3, &gt, 1, GRADIENT_FILL_TRIANGLE);
    }
    shot("gradientfill", 2);
    clear();
    TransparentBlt(dc, 0, 0, 16, 16, src, 0, 0, 16, 16, RGB(0, 0, 0xff) | 0);
    TransparentBlt(dc, 20, 0, 32, 32, src, 0, 0, 16, 16, spx[17] & 0xffffff ? RGB(spx[17] >> 16 & 255, spx[17] >> 8 & 255, spx[17] & 255) : 0);
    shot("transparentblt", 0);
    /* GetDIBits of the 32bpp picture into 24, 16 (555) and 8bpp */
    {
        clear();
        BitBlt(dc, 0, 0, 16, 16, src, 0, 0, SRCCOPY);
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        SelectObject(dc, bmp);
        HDC sdc = CreateCompatibleDC(NULL);
        HGDIOBJ old = SelectObject(dc, CreateCompatibleBitmap(dc, 1, 1));
        static const int bpp[] = { 24, 16, 8, 1 };
        for (int k = 0; k < 4; k++) {
            struct { BITMAPINFOHEADER h; RGBQUAD pal[256]; } o = { { sizeof(BITMAPINFOHEADER), 16, 4, 1, (WORD)bpp[k], BI_RGB } };
            unsigned char out[16 * 4 * 4];
            memset(out, 0xcc, sizeof out);
            int n = GetDIBits(sdc, bmp, 0, 4, out, (BITMAPINFO *)&o, DIB_RGB_COLORS);
            int stride = ((16 * bpp[k] + 31) / 32) * 4;
            printf("GetDIBits %d -> %d size %lu clr %lu:", bpp[k], n, o.h.biSizeImage, o.h.biClrUsed);
            for (int i = 0; i < stride * 4 && i < 64; i++) printf(" %02x", out[i]);
            if (bpp[k] <= 8) for (int i = 0; i < 4; i++) printf(" p%06x", *(unsigned *)&o.pal[i] & 0xffffff);
            printf("\n");
        }
        DeleteObject(SelectObject(dc, old));
        DeleteDC(sdc);
    }
    {
        BITMAP b;
        GetObjectA(bmp, sizeof b, &b);
        printf("GetObject %ld %ld %ld %d %d %d\n", b.bmType, b.bmWidth, b.bmHeight, b.bmWidthBytes, b.bmPlanes, b.bmBitsPixel);
        DIBSECTION ds;
        printf("GetObject dibsection %d %ld %ld\n", GetObjectA(bmp, sizeof ds, &ds), ds.dsBmih.biWidth, ds.dsBmih.biHeight);
        printf("GetDeviceCaps %d %d %d\n", GetDeviceCaps(dc, BITSPIXEL), GetDeviceCaps(dc, RASTERCAPS) & RC_STRETCHBLT ? 1 : 0, GetDeviceCaps(dc, NUMCOLORS));
    }
    return 0;
}
