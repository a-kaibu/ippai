/* LoadBitmap from the program's NE resources (gen-bmp.py's bitmaps, loadbmp.rc): by number and
 * by name, 8, 4, 1 and 24 bits and an OS/2 core header; the size and colours of what it made,
 * pixel by pixel; a missing one; a system bitmap; and LoadString, which found
 * no NE string table before either. sake had no LoadBitmap for NE resources (#37). */
#include <windows.h>
#include <stdio.h>

static HINSTANCE inst;

static void show(const char *what, LPCSTR name, int w, int h) {
    HBITMAP b = LoadBitmap(inst, name);
    BITMAP bm;
    HDC dc, mem;
    HBITMAP old;
    int x, y;
    printf("%s: %s", what, b ? "loaded" : "none");
    if (!b) {
        printf("\n");
        return;
    }
    GetObject(b, sizeof bm, &bm);
    printf(" %dx%d planes %d bits %s\n", bm.bmWidth, bm.bmHeight, bm.bmPlanes, bm.bmBitsPixel == 1 ? "1" : "colour");
    dc = GetDC(0);
    mem = CreateCompatibleDC(dc);
    old = SelectObject(mem, b);
    for (y = 0; y < h; y++) {
        printf("  ");
        for (x = 0; x < w; x++) printf(" %06lx", GetPixel(mem, x, y));
        printf("\n");
    }
    SelectObject(mem, old);
    DeleteDC(mem);
    ReleaseDC(0, dc);
    DeleteObject(b);
}

int PASCAL WinMain(HINSTANCE hi, HINSTANCE hp, LPSTR cmd, int sh) {
    inst = hi;
    show("1 (8 bits)", MAKEINTRESOURCE(1), 4, 2);
    show("LOGO (4 bits)", "LOGO", 3, 3);
    show("logo, any case", "logo", 1, 1);
    show("MONO (1 bit)", "MONO", 9, 2);
    show("RGB (24 bits)", "RGB", 2, 2);
    show("7 (core header)", MAKEINTRESOURCE(7), 5, 1);
    show("missing", "NONE", 0, 0);
    show("missing number", MAKEINTRESOURCE(99), 0, 0);
    {
        char b[32];
        int n = LoadString(inst, 17, b, sizeof b);
        printf("LoadString 17: %d \"%s\"\n", n, b);
        n = LoadString(inst, 1, b, 3);
        printf("LoadString 1 into 3: %d \"%s\"\n", n, b);
        printf("LoadString 2 (none): %d\n", LoadString(inst, 2, b, sizeof b));
    }
    printf("OBM_CHECK %s\n", LoadBitmap(0, MAKEINTRESOURCE(32760)) ? "loaded" : "none");
    return 0;
}
