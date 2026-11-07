/* user32's drawing helpers that dialogs and common controls are made of: the system colours
 * (GetSysColor, what GetSysColorBrush fills with), DrawEdge (every BDR_* combination with the
 * BF_SOFT / BF_FLAT / BF_MONO / BF_MIDDLE variants, single sides, BF_ADJUST's rectangle, return
 * values) as pictures of a 32bpp DIB section, and FillRect with a COLOR_* + 1 brush.
 * libs: -lgdi32 */
#include "../probe.h"

static unsigned *bits;
static HDC dc;
static const int W = 96, H = 64;

static void clear(void) {
    for (int i = 0; i < W * H; i++) bits[i] = 0x123456;
}

int main(void) {
    for (int i = 0; i <= 30; i++) printf("GetSysColor(%d) = %06lx\n", i, (unsigned long)GetSysColor(i));
    for (int i = 0; i <= 30; i++) {
        HBRUSH b = GetSysColorBrush(i);
        LOGBRUSH lb = { 0 };
        GetObjectW(b, sizeof lb, &lb);
        printf("GetSysColorBrush(%d) style %u color %06lx\n", i, lb.lbStyle, (unsigned long)lb.lbColor);
    }

    BITMAPINFO bi = { { sizeof(BITMAPINFOHEADER), W, -H, 1, 32, BI_RGB } };
    dc = CreateCompatibleDC(NULL);
    HBITMAP bm = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, (void **)&bits, NULL, 0);
    SelectObject(dc, bm);

    /* FillRect with COLOR_BTNFACE + 1 and friends */
    clear();
    for (int i = 0; i < 6; i++) {
        RECT r = { i * 16, 0, i * 16 + 16, 8 };
        static const int idx[] = { COLOR_BTNFACE, COLOR_BTNSHADOW, COLOR_3DLIGHT, COLOR_3DDKSHADOW, COLOR_WINDOW, COLOR_HIGHLIGHT };
        FillRect(dc, &r, (HBRUSH)(INT_PTR)(idx[i] + 1));
    }
    GdiFlush();
    probe_image("fillrect_syscolor", bits, W, 8, W * 4, 0);

    /* every edge kind (16 BDR_ combinations) with each flag set: 6 x 3 cells of 16x16 per picture */
    static const struct { const char *name; UINT flags; } kinds[] = {
        { "rect", BF_RECT }, { "soft", BF_RECT | BF_SOFT }, { "flat", BF_RECT | BF_FLAT }, { "mono", BF_RECT | BF_MONO },
        { "middle", BF_RECT | BF_MIDDLE }, { "soft_middle", BF_RECT | BF_SOFT | BF_MIDDLE }, { "flat_middle", BF_RECT | BF_FLAT | BF_MIDDLE },
        { "mono_middle", BF_RECT | BF_MONO | BF_MIDDLE },
    };
    for (unsigned k = 0; k < sizeof kinds / sizeof *kinds; k++) {
        clear();
        char line[256], *o = line;
        o += sprintf(o, "drawedge %s returns", kinds[k].name);
        for (int e = 0; e < 16; e++) {
            RECT r = { (e % 6) * 16, (e / 6) * 16, (e % 6) * 16 + 14, (e / 6) * 16 + 14 };
            o += sprintf(o, " %d", DrawEdge(dc, &r, e, kinds[k].flags));
        }
        puts(line);
        GdiFlush();
        char name[64];
        sprintf(name, "drawedge_%s", kinds[k].name);
        probe_image(name, bits, W, 48, W * 4, 0);
    }

    /* single sides and corner pairs of EDGE_RAISED / EDGE_SUNKEN / EDGE_ETCHED / EDGE_BUMP */
    clear();
    static const UINT edges[] = { EDGE_RAISED, EDGE_SUNKEN, EDGE_ETCHED, EDGE_BUMP };
    static const UINT sides[] = { BF_LEFT, BF_TOP, BF_RIGHT, BF_BOTTOM, BF_TOPLEFT, BF_BOTTOMRIGHT, BF_LEFT | BF_TOP | BF_RIGHT, BF_LEFT | BF_BOTTOM | BF_RIGHT | BF_SOFT };
    for (int e = 0; e < 4; e++)
        for (int s = 0; s < 8; s++) {
            RECT r = { s * 12, e * 12, s * 12 + 10, e * 12 + 10 };
            DrawEdge(dc, &r, edges[e], sides[s]);
        }
    GdiFlush();
    probe_image("drawedge_sides", bits, W, 48, W * 4, 0);

    /* BF_ADJUST: the rectangle left inside the edge */
    static const UINT adj[] = { BF_RECT, BF_LEFT | BF_TOP, BF_RIGHT, BF_RECT | BF_FLAT, BF_RECT | BF_MONO, BF_BOTTOM | BF_MIDDLE };
    for (int e = 0; e < 4; e++)
        for (int a = 0; a < 6; a++) {
            RECT r = { 10, 20, 50, 60 };
            BOOL ok = DrawEdge(dc, &r, edges[e], adj[a] | BF_ADJUST);
            printf("adjust edge %x flags %x -> %d %ld,%ld,%ld,%ld\n", edges[e], adj[a], ok, r.left, r.top, r.right, r.bottom);
        }
    RECT r = { 1, 1, 9, 9 };
    printf("drawedge empty %d\n", DrawEdge(dc, &(RECT){ 5, 5, 5, 5 }, EDGE_RAISED, BF_RECT));
    printf("drawedge inverted %d\n", DrawEdge(dc, &(RECT){ 9, 9, 1, 1 }, EDGE_SUNKEN, BF_RECT | BF_ADJUST));
    (void)r;
    return 0;
}
