/* LoadImage(IMAGE_BITMAP, LR_LOADFROMFILE) of .bmp files: what object comes back (depth, DIB
 * section or not, bits) and its pixels. Key's anemoi StartMenu.exe calls
 * LoadImageW(its module, L"Start.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE)
 * on a 400x400 24-bit file whose biSizeImage is 2 bytes too big, and sake looked for a resource
 * (the module handle is not looked at for a file). Also: no LR_CREATEDIBSECTION, 8-, 1- and
 * 32-bit (top-down) files, a file whose bits do not start right after the header, the A
 * version, missing and broken files, an icon from a missing file.
 * libs: -lgdi32 */
#include "../probe.h"

static void put16(unsigned char *p, int v) { p[0] = v; p[1] = v >> 8; }
static void put32(unsigned char *p, int v) { put16(p, v); put16(p + 2, v >> 16); }

/* a .bmp: w x h (negative h: top-down), bpp, colours, `gap` bytes between table and bits,
 * `extra` added to biSizeImage */
static void write_bmp(const char *name, int w, int h, int bpp, int ncol, int gap, int extra) {
    int ah = h < 0 ? -h : h, stride = ((w * bpp + 31) / 32) * 4, off = 14 + 40 + ncol * 4 + gap;
    int size = off + stride * ah;
    unsigned char *b = calloc(1, size);
    b[0] = 'B'; b[1] = 'M';
    put32(b + 2, size);
    put32(b + 10, off);
    put32(b + 14, 40);
    put32(b + 18, w);
    put32(b + 22, h);
    put16(b + 26, 1);
    put16(b + 28, bpp);
    put32(b + 34, stride * ah + extra);
    put32(b + 38, 2834);
    put32(b + 42, 2834);
    put32(b + 46, ncol);
    for (int i = 0; i < ncol; i++) {
        unsigned char *q = b + 54 + i * 4;
        q[0] = i * 40; q[1] = 255 - i * 30; q[2] = i * 70 & 255;
    }
    for (int y = 0; y < ah; y++)
        for (int x = 0; x < w; x++) {
            unsigned char *row = b + off + y * stride;
            int v = x * 7 + y * 13;
            if (bpp == 24) { row[x * 3] = v; row[x * 3 + 1] = v * 3; row[x * 3 + 2] = 255 - v; }
            if (bpp == 32) { row[x * 4] = v; row[x * 4 + 1] = v * 5; row[x * 4 + 2] = 200 - v; }
            if (bpp == 8) row[x] = (x + y) % ncol;
            if (bpp == 1 && (x + y) % 2) row[x / 8] |= 0x80 >> (x % 8);
        }
    FILE *f = fopen(name, "wb");
    fwrite(b, 1, size, f);
    fclose(f);
    free(b);
}

static void show(const char *what, HBITMAP h) {
    if (!h) {
        printf("%s: NULL\n", what);
        return;
    }
    BITMAP bm;
    DIBSECTION ds;
    int n = GetObjectA(h, sizeof bm, &bm);
    int dn = GetObjectA(h, sizeof ds, &ds);
    printf("%s: %d bytes %dx%d bpp %d planes %d widthbytes %d bits %s section %s", what, n, (int)bm.bmWidth, (int)bm.bmHeight,
           bm.bmBitsPixel, bm.bmPlanes, (int)bm.bmWidthBytes, bm.bmBits ? "yes" : "no", dn == sizeof ds ? "yes" : "no");
    if (dn == sizeof ds) printf(" height %d compression %d", (int)ds.dsBmih.biHeight, (int)ds.dsBmih.biCompression);
    printf("\n");
    HDC dc = CreateCompatibleDC(NULL);
    HGDIOBJ old = SelectObject(dc, h);
    printf("%s pixels:", what);
    for (int y = 0; y < bm.bmHeight && y < 3; y++)
        for (int x = 0; x < bm.bmWidth && x < 4; x++) printf(" %06lx", (unsigned long)GetPixel(dc, x, y));
    printf("\n");
    SelectObject(dc, old);
    DeleteDC(dc);
    DeleteObject(h);
}

int main(void) {
    char dir[MAX_PATH];
    GetTempPathA(MAX_PATH, dir);
    strcat(dir, "masu-loadimage");
    CreateDirectoryA(dir, NULL);
    SetCurrentDirectoryA(dir);
    HINSTANCE me = GetModuleHandleA(NULL);
    write_bmp("Start.bmp", 5, 3, 24, 0, 0, 2);
    write_bmp("pal8.bmp", 4, 3, 8, 6, 0, 0);
    write_bmp("mono.bmp", 9, 2, 1, 2, 0, 0);
    write_bmp("top32.bmp", 3, -2, 32, 0, 0, 0);
    write_bmp("gap.bmp", 2, 2, 24, 0, 6, 0);
    FILE *f = fopen("broken.bmp", "wb");
    fwrite("BMxx", 1, 4, f);
    fclose(f);

    show("24 section (module handle)", LoadImageW(me, L"Start.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));
    show("24 ddb", LoadImageW(NULL, L"Start.bmp", IMAGE_BITMAP, 0, 0, LR_LOADFROMFILE));
    show("24 section A", LoadImageA(me, "Start.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));
    show("8 section", LoadImageA(NULL, "pal8.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));
    show("8 ddb", LoadImageA(NULL, "pal8.bmp", IMAGE_BITMAP, 0, 0, LR_LOADFROMFILE));
    show("1 section", LoadImageA(NULL, "mono.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));
    show("1 ddb", LoadImageA(NULL, "mono.bmp", IMAGE_BITMAP, 0, 0, LR_LOADFROMFILE));
    show("32 top-down section", LoadImageA(NULL, "top32.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));
    show("gap section", LoadImageA(NULL, "gap.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));
    char full[MAX_PATH];
    GetFullPathNameA("Start.bmp", MAX_PATH, full, NULL);
    show("full path", LoadImageA(NULL, full, IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE));

    SetLastError(0);
    HANDLE h = LoadImageA(me, "nothere.bmp", IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION | LR_LOADFROMFILE);
    printf("missing: %s error %lu\n", h ? "handle" : "NULL", GetLastError());
    h = LoadImageA(NULL, "broken.bmp", IMAGE_BITMAP, 0, 0, LR_LOADFROMFILE);
    printf("broken: %s\n", h ? "handle" : "NULL");
    h = LoadImageA(NULL, "nothere.ico", IMAGE_ICON, 32, 32, LR_LOADFROMFILE);
    printf("missing icon: %s\n", h ? "handle" : "NULL");
    return 0;
}
