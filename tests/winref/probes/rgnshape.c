/* The shapes of GDI's elliptic and round-rectangle regions (issue #55): the rectangles
 * GetRegionData gives for CreateEllipticRgn and CreateRoundRectRgn of many sizes, corner
 * sizes and coordinate orders, one line per region ("<call> <args>: <type> l,t,r,b ..."),
 * and CreatePolygonRgn of a few polygons in both fill modes. libs: -lgdi32 */
#include "../probe.h"

static void dump(const char *what, HRGN r) {
    char buf[65536];
    DWORD n = GetRegionData(r, 0, NULL);
    RGNDATA *d = (RGNDATA *)buf;
    RECT box;
    int type = GetRgnBox(r, &box);
    printf("%s: %d", what, type);
    if (n > sizeof buf || !GetRegionData(r, n, d)) {
        printf(" data %lu\n", n);
        DeleteObject(r);
        return;
    }
    RECT *rc = (RECT *)d->Buffer;
    for (DWORD i = 0; i < d->rdh.nCount; i++) printf(" %ld,%ld,%ld,%ld", rc[i].left, rc[i].top, rc[i].right, rc[i].bottom);
    printf("\n");
    DeleteObject(r);
}

int main(void) {
    char what[128];
    static const int sizes[] = { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17, 20, 23, 24, 31, 32, 33, 40, 47, 48, 63, 64, 100, 101 };
    int ns = sizeof sizes / sizeof *sizes;
    /* circles at an even and an odd origin */
    for (int i = 0; i < ns; i++) {
        int s = sizes[i];
        sprintf(what, "ellipse 0 0 %d %d", s, s);
        dump(what, CreateEllipticRgn(0, 0, s, s));
        sprintf(what, "ellipse 3 5 %d %d", 3 + s, 5 + s);
        dump(what, CreateEllipticRgn(3, 5, 3 + s, 5 + s));
    }
    /* ellipses: wide, tall */
    static const int wh[][2] = { { 20, 10 }, { 10, 20 }, { 33, 7 }, { 7, 33 }, { 64, 31 }, { 31, 64 }, { 2, 9 }, { 9, 2 }, { 100, 40 }, { 41, 100 } };
    for (int i = 0; i < (int)(sizeof wh / sizeof *wh); i++) {
        sprintf(what, "ellipse 0 0 %d %d", wh[i][0], wh[i][1]);
        dump(what, CreateEllipticRgn(0, 0, wh[i][0], wh[i][1]));
    }
    /* swapped corners, negative coordinates */
    dump("ellipse 40 30 0 0", CreateEllipticRgn(40, 30, 0, 0));
    dump("ellipse -20 -10 20 10", CreateEllipticRgn(-20, -10, 20, 10));
    /* round rectangles: box sizes, corner sizes (0, 1, odd, even, over the box) */
    static const int boxes[][2] = { { 30, 30 }, { 40, 20 }, { 17, 33 }, { 64, 48 } };
    static const int corners[][2] = { { 0, 0 }, { 1, 1 }, { 2, 2 }, { 3, 3 }, { 5, 5 }, { 6, 6 }, { 12, 12 }, { 13, 7 }, { 7, 13 }, { 20, 4 }, { 31, 31 }, { 100, 100 }, { -6, -6 } };
    for (int i = 0; i < (int)(sizeof boxes / sizeof *boxes); i++)
        for (int k = 0; k < (int)(sizeof corners / sizeof *corners); k++) {
            sprintf(what, "roundrect 2 2 %d %d %d %d", 2 + boxes[i][0], 2 + boxes[i][1], corners[k][0], corners[k][1]);
            dump(what, CreateRoundRectRgn(2, 2, 2 + boxes[i][0], 2 + boxes[i][1], corners[k][0], corners[k][1]));
        }
    dump("roundrect 30 30 2 2 12 12", CreateRoundRectRgn(30, 30, 2, 2, 12, 12));
    /* polygons: a triangle, a star (both fill modes), a diamond, a thin sliver */
    POINT tri[3] = { { 40, 2 }, { 62, 30 }, { 34, 20 } };
    POINT star[5] = { { 20, 0 }, { 32, 38 }, { 0, 14 }, { 40, 14 }, { 8, 38 } };
    POINT dia[4] = { { 16, 0 }, { 32, 16 }, { 16, 32 }, { 0, 16 } };
    POINT sliver[3] = { { 0, 0 }, { 50, 3 }, { 0, 5 } };
    dump("polygon triangle alternate", CreatePolygonRgn(tri, 3, ALTERNATE));
    dump("polygon star alternate", CreatePolygonRgn(star, 5, ALTERNATE));
    dump("polygon star winding", CreatePolygonRgn(star, 5, WINDING));
    dump("polygon diamond", CreatePolygonRgn(dia, 4, ALTERNATE));
    dump("polygon sliver", CreatePolygonRgn(sliver, 3, ALTERNATE));
    return 0;
}
