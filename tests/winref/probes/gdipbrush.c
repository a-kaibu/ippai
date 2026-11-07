/* GDI+ brushes (gdiplus_brush.rs) as games and their UI frameworks fill with them: every hatch
 * style's 8x8 pattern and how it lines up with the rendering origin, hatch colors with alpha,
 * linear gradients (the rectangle and transform GDI+ makes from two points, a rectangle and a
 * mode, a rectangle and an angle; blends, preset blends, wrap modes, the pixels they give), a
 * rotated texture, path gradients (center point, rectangle, surround colors) and matrices.
 * libs: -lgdiplus -lole32 -lgdi32 */
#include "../probe.h"

typedef int GpStatus;
typedef void GpImage, GpBitmap, GpGraphics, GpBrush, GpMatrix, GpPath;
typedef struct { UINT32 GdiplusVersion; void *DebugEventCallback; BOOL SuppressBackgroundThread, SuppressExternalCodecs; } StartupInput;
typedef struct { float X, Y; } PointF;
typedef struct { float X, Y, Width, Height; } RectF;
GpStatus WINAPI GdiplusStartup(ULONG_PTR *, const StartupInput *, void *);
GpStatus WINAPI GdipCreateBitmapFromScan0(INT, INT, INT, INT, BYTE *, GpBitmap **);
GpStatus WINAPI GdipGetImageGraphicsContext(GpImage *, GpGraphics **);
GpStatus WINAPI GdipDeleteGraphics(GpGraphics *);
GpStatus WINAPI GdipDisposeImage(GpImage *);
GpStatus WINAPI GdipBitmapGetPixel(GpBitmap *, INT, INT, DWORD *);
GpStatus WINAPI GdipBitmapSetPixel(GpBitmap *, INT, INT, DWORD);
GpStatus WINAPI GdipFillRectangleI(GpGraphics *, GpBrush *, INT, INT, INT, INT);
GpStatus WINAPI GdipSetRenderingOrigin(GpGraphics *, INT, INT);
GpStatus WINAPI GdipDeleteBrush(GpBrush *);
GpStatus WINAPI GdipGetBrushType(GpBrush *, INT *);
GpStatus WINAPI GdipCreateHatchBrush(INT, DWORD, DWORD, GpBrush **);
GpStatus WINAPI GdipGetHatchForegroundColor(GpBrush *, DWORD *);
GpStatus WINAPI GdipGetHatchBackgroundColor(GpBrush *, DWORD *);
GpStatus WINAPI GdipCreateLineBrush(const PointF *, const PointF *, DWORD, DWORD, INT, GpBrush **);
GpStatus WINAPI GdipCreateLineBrushFromRect(const RectF *, DWORD, DWORD, INT, INT, GpBrush **);
GpStatus WINAPI GdipCreateLineBrushFromRectWithAngle(const RectF *, DWORD, DWORD, float, BOOL, INT, GpBrush **);
GpStatus WINAPI GdipGetLineRect(GpBrush *, RectF *);
GpStatus WINAPI GdipGetLineTransform(GpBrush *, GpMatrix *);
GpStatus WINAPI GdipGetLineWrapMode(GpBrush *, INT *);
GpStatus WINAPI GdipSetLineWrapMode(GpBrush *, INT);
GpStatus WINAPI GdipSetLineBlend(GpBrush *, const float *, const float *, INT);
GpStatus WINAPI GdipSetLinePresetBlend(GpBrush *, const DWORD *, const float *, INT);
GpStatus WINAPI GdipSetLineSigmaBlend(GpBrush *, float, float);
GpStatus WINAPI GdipSetLineLinearBlend(GpBrush *, float, float);
GpStatus WINAPI GdipGetLineBlendCount(GpBrush *, INT *);
GpStatus WINAPI GdipGetLineBlend(GpBrush *, float *, float *, INT);
GpStatus WINAPI GdipCreateTexture(GpImage *, INT, GpBrush **);
GpStatus WINAPI GdipRotateTextureTransform(GpBrush *, float, INT);
GpStatus WINAPI GdipScaleTextureTransform(GpBrush *, float, float, INT);
GpStatus WINAPI GdipCreatePath(INT, GpPath **);
GpStatus WINAPI GdipDeletePath(GpPath *);
GpStatus WINAPI GdipAddPathEllipse(GpPath *, float, float, float, float);
GpStatus WINAPI GdipGetPointCount(GpPath *, INT *);
GpStatus WINAPI GdipCreatePathGradient(const PointF *, INT, INT, GpBrush **);
GpStatus WINAPI GdipCreatePathGradientFromPath(GpPath *, GpBrush **);
GpStatus WINAPI GdipGetPathGradientCenterPoint(GpBrush *, PointF *);
GpStatus WINAPI GdipGetPathGradientCenterColor(GpBrush *, DWORD *);
GpStatus WINAPI GdipGetPathGradientRect(GpBrush *, RectF *);
GpStatus WINAPI GdipGetPathGradientSurroundColorCount(GpBrush *, INT *);
GpStatus WINAPI GdipGetPathGradientSurroundColorsWithCount(GpBrush *, DWORD *, INT *);
GpStatus WINAPI GdipGetPathGradientWrapMode(GpBrush *, INT *);
GpStatus WINAPI GdipCreateMatrix(GpMatrix **);
GpStatus WINAPI GdipCreateMatrix2(float, float, float, float, float, float, GpMatrix **);
GpStatus WINAPI GdipDeleteMatrix(GpMatrix *);
GpStatus WINAPI GdipGetMatrixElements(GpMatrix *, float *);
GpStatus WINAPI GdipRotateMatrix(GpMatrix *, float, INT);
GpStatus WINAPI GdipScaleMatrix(GpMatrix *, float, float, INT);
GpStatus WINAPI GdipTranslateMatrix(GpMatrix *, float, float, INT);
GpStatus WINAPI GdipShearMatrix(GpMatrix *, float, float, INT);
GpStatus WINAPI GdipMultiplyMatrix(GpMatrix *, GpMatrix *, INT);
GpStatus WINAPI GdipInvertMatrix(GpMatrix *);
GpStatus WINAPI GdipIsMatrixInvertible(GpMatrix *, BOOL *);
GpStatus WINAPI GdipTransformMatrixPoints(GpMatrix *, PointF *, INT);

#define PF_32RGB 0x00022009
#define PF_ARGB 0x0026200a

static GpMatrix *mat;

/* elements rounded to 3 decimals (-0 printed as 0) */
static void elems(const char *what, GpMatrix *m) {
    float e[6];
    int s = GdipGetMatrixElements(m, e);
    printf("%s: %d", what, s);
    for (int i = 0; i < 6; i++) {
        double v = e[i];
        if (v > -0.0005 && v < 0.0005) v = 0;
        printf(" %.3f", v);
    }
    printf("\n");
}

/* (-0 as 0: the 64-bit GDI+ gets tiny negatives where the 32-bit one has 0) */
static double z(double v) { return v > -0.0005 && v < 0.0005 ? 0 : v; }

static void line(const char *what, GpBrush *b) {
    RectF r = {0};
    GdipGetLineRect(b, &r);
    printf("%s rect %.3f %.3f %.3f %.3f\n", what, r.X, r.Y, r.Width, r.Height);
    GdipGetLineTransform(b, mat);
    elems(what, mat);
}

/* fills a w x h bitmap of format fmt with the brush and prints its pixels */
static void fill(const char *name, GpBrush *b, int w, int h, int fmt, int tol, int ox, int oy) {
    GpBitmap *bmp;
    GpGraphics *g;
    GdipCreateBitmapFromScan0(w, h, 0, fmt, NULL, &bmp);
    GdipGetImageGraphicsContext(bmp, &g);
    if (ox || oy) GdipSetRenderingOrigin(g, ox, oy);
    int s = GdipFillRectangleI(g, b, 0, 0, w, h);
    GdipDeleteGraphics(g);
    static DWORD px[64 * 64];
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) GdipBitmapGetPixel(bmp, x, y, &px[y * w + x]);
    printf("%s fill %d\n", name, s);
    probe_image(name, px, w, h, w * 4, tol);
    GdipDisposeImage(bmp);
}

int main(void) {
    ULONG_PTR tok;
    StartupInput in = {1, NULL, FALSE, FALSE};
    GdiplusStartup(&tok, &in, NULL);
    GdipCreateMatrix(&mat);
    GpBrush *b;
    char name[64];

    /* hatches: black on white, every style; the ones out of range */
    for (int hs = -1; hs <= 53; hs++) {
        b = NULL;
        int s = GdipCreateHatchBrush(hs, 0xff000000, 0xffffffff, &b);
        if (s) {
            printf("hatch %d: %d\n", hs, s);
            continue;
        }
        sprintf(name, "hatch%d", hs);
        fill(name, b, 8, 8, PF_32RGB, 0, 0, 0);
        GdipDeleteBrush(b);
    }
    GdipCreateHatchBrush(4, 0xff000000, 0xffffffff, &b);
    fill("origin_3_6", b, 8, 8, PF_32RGB, 0, 3, 6);
    fill("origin_m7_m4", b, 8, 8, PF_32RGB, 0, -7, -4);
    GdipDeleteBrush(b);
    /* colors with alpha onto a transparent ARGB bitmap, and onto an opaque one */
    GdipCreateHatchBrush(2, 0x80ff0000, 0x400000ff, &b);
    {
        int t = -1;
        DWORD f = 0, k = 0;
        GdipGetBrushType(b, &t);
        GdipGetHatchForegroundColor(b, &f);
        GdipGetHatchBackgroundColor(b, &k);
        printf("hatch type %d fore %08lx back %08lx\n", t, f, k);
        GpBitmap *bmp;
        GpGraphics *g;
        GdipCreateBitmapFromScan0(8, 1, 0, PF_ARGB, NULL, &bmp);
        GdipGetImageGraphicsContext(bmp, &g);
        GdipFillRectangleI(g, b, 0, 0, 8, 1);
        printf("hatch argb:");
        for (int x = 0; x < 8; x++) {
            DWORD p;
            GdipBitmapGetPixel(bmp, x, 0, &p);
            printf(" %08lx", p);
        }
        printf("\n");
        GdipDeleteGraphics(g);
        GdipDisposeImage(bmp);
    }
    GdipDeleteBrush(b);

    /* linear gradients from two points */
    static const PointF pts[][2] = {
        {{1, 1}, {100, 100}}, {{0, 0}, {0, 10}}, {{0, 0}, {10, 0}}, {{0, 0}, {20, -20}}, {{0, 0}, {100, 1}},
        {{10, 10}, {-90, 10}}, {{3, 7}, {40, 20}}, {{-5, 30}, {25, -10}}, {{0, 0}, {-30, -60}},
    };
    for (int i = 0; i < (int)(sizeof pts / sizeof pts[0]); i++) {
        int s = GdipCreateLineBrush(&pts[i][0], &pts[i][1], 0xff000000, 0xffffffff, 0, &b);
        sprintf(name, "line %d: %d", i, s);
        line(name, b);
        GdipDeleteBrush(b);
    }
    PointF same = {5, 5};
    b = NULL;
    printf("line same points %d\n", GdipCreateLineBrush(&same, &same, 0, 0, 0, &b));

    /* from a rectangle and a mode, and from a rectangle and an angle */
    static const RectF rects[] = {{10, -12, 100, 105}, {0, 0, 100, 10}, {10, 10, -100, -50}, {-5, 3, 40, 80}};
    for (int i = 0; i < 4; i++)
        for (int mode = 0; mode < 4; mode++) {
            int s = GdipCreateLineBrushFromRect(&rects[i], 0xff000000, 0xffffffff, mode, 0, &b);
            sprintf(name, "rect %d mode %d: %d", i, mode, s);
            line(name, b);
            GdipDeleteBrush(b);
        }
    b = NULL;
    printf("rect mode 4: %d\n", GdipCreateLineBrushFromRect(&rects[0], 0, 0, 4, 0, &b));
    static const float angles[] = {0, 30, 45, 90, 120, 180, -90, -225, 405, 271.5f};
    for (int i = 0; i < 4; i++)
        for (int a = 0; a < (int)(sizeof angles / sizeof angles[0]); a++)
            for (int sc = 0; sc < 2; sc++) {
                int s = GdipCreateLineBrushFromRectWithAngle(&rects[i], 0xff000000, 0xffffffff, angles[a], sc, 0, &b);
                sprintf(name, "angle %d %g %d: %d", i, angles[a], sc, s);
                line(name, b);
                GdipDeleteBrush(b);
            }

    /* the pixels of gradients: black to white over 16 pixels, each wrap mode over 40 */
    PointF p0 = {0, 0}, p1 = {16, 0};
    for (int wrap = 0; wrap < 4; wrap++) {
        GdipCreateLineBrush(&p0, &p1, 0xff000000, 0xffffffff, wrap, &b);
        int w = -1;
        GdipGetLineWrapMode(b, &w);
        sprintf(name, "grad_wrap%d", wrap);
        printf("wrap %d got %d\n", wrap, w);
        fill(name, b, 40, 1, PF_32RGB, 2, 0, 0);
        GdipDeleteBrush(b);
    }
    b = NULL;
    printf("line wrap clamp %d\n", GdipCreateLineBrush(&p0, &p1, 0, 0, 4, &b));
    PointF p2 = {0, 0}, p3 = {16, 16};
    GdipCreateLineBrush(&p2, &p3, 0xffff0000, 0xff0000ff, 0, &b);
    fill("grad_diag", b, 16, 16, PF_32RGB, 2, 0, 0);
    static const float fac[] = {0, 0.8f, 0.2f, 1}, pos[] = {0, 0.25f, 0.75f, 1};
    GdipSetLineBlend(b, fac, pos, 4);
    fill("grad_blend", b, 16, 16, PF_32RGB, 2, 0, 0);
    static const DWORD cols[] = {0xffff0000, 0xff00ff00, 0xff0000ff};
    static const float cpos[] = {0, 0.3f, 1};
    GdipSetLinePresetBlend(b, cols, cpos, 3);
    fill("grad_preset", b, 16, 16, PF_32RGB, 2, 0, 0);
    GdipDeleteBrush(b);
    GdipCreateLineBrush(&p0, &p1, 0x00000000, 0xffffffff, 0, &b);
    fill("grad_alpha", b, 16, 1, PF_ARGB, 2, 0, 0);
    GdipSetLineSigmaBlend(b, 0.5f, 1.0f);
    {
        int n = -1;
        GdipGetLineBlendCount(b, &n);
        printf("sigma blend count %d\n", n);
        float f[512], q[512];
        if (n > 0 && n <= 512 && !GdipGetLineBlend(b, f, q, n))
            for (int i = 0; i < n; i += n / 8 > 0 ? n / 8 : 1) printf("sigma %d %.3f %.3f\n", i, q[i], f[i]);
    }
    for (int k = 0; k < 2; k++) {
        int n = -1;
        GdipSetLineSigmaBlend(b, (float)k, 0.5f);
        GdipGetLineBlendCount(b, &n);
        printf("sigma focus %d count %d\n", k, n);
        float f[512], q[512];
        if (n > 0 && n <= 512 && !GdipGetLineBlend(b, f, q, n))
            for (int i = 0; i < n; i += n / 4 > 0 ? n / 4 : 1) printf("sigma %d %.3f %.3f\n", i, q[i], f[i]);
    }
    GdipSetLineLinearBlend(b, 0.25f, 0.5f);
    {
        int n = -1;
        GdipGetLineBlendCount(b, &n);
        float f[8], q[8];
        GdipGetLineBlend(b, f, q, n);
        printf("linear blend %d:", n);
        for (int i = 0; i < n && i < 8; i++) printf(" %.3f/%.3f", q[i], f[i]);
        printf("\n");
    }
    GdipDeleteBrush(b);

    /* a texture rotated by 180 degrees, scaled by 2 */
    {
        GpBitmap *t;
        GdipCreateBitmapFromScan0(3, 2, 0, PF_32RGB, NULL, &t);
        for (int y = 0; y < 2; y++)
            for (int x = 0; x < 3; x++) GdipBitmapSetPixel(t, x, y, 0xff000000 | (x * 0x60) << 16 | (y * 0x80) << 8);
        GdipCreateTexture(t, 0, &b);
        fill("texture", b, 7, 5, PF_32RGB, 0, 0, 0);
        GdipRotateTextureTransform(b, 180, 0);
        fill("texture_rot180", b, 7, 5, PF_32RGB, 0, 0, 0);
        GdipDeleteBrush(b);
        GdipCreateTexture(t, 0, &b);
        GdipScaleTextureTransform(b, 2, 2, 0);
        fill("texture_x2", b, 7, 5, PF_32RGB, 0, 0, 0);
        GdipDeleteBrush(b);
        GdipCreateTexture(t, 3, &b);
        fill("texture_flipxy", b, 7, 5, PF_32RGB, 0, 0, 0);
        GdipDeleteBrush(b);
        GdipDisposeImage(t);
    }

    /* path gradients */
    static const PointF tri[] = {{0, 0}, {30, 0}, {0, 40}, {10, 50}};
    for (int n = 1; n <= 4; n++) {
        b = NULL;
        int s = GdipCreatePathGradient(tri, n, 0, &b);
        printf("pathgrad %d: %d", n, s);
        if (!s) {
            PointF c;
            RectF r;
            DWORD cc = 0, sc[4];
            int k = 4, t = -1, w = -1;
            GdipGetPathGradientCenterPoint(b, &c);
            GdipGetPathGradientRect(b, &r);
            GdipGetPathGradientCenterColor(b, &cc);
            GdipGetBrushType(b, &t);
            GdipGetPathGradientWrapMode(b, &w);
            printf(" center %.3f %.3f rect %.1f %.1f %.1f %.1f color %08lx type %d wrap %d", c.X, c.Y, r.X, r.Y, r.Width, r.Height, cc, t, w);
            GdipGetPathGradientSurroundColorCount(b, &k);
            printf(" surround %d", k);
            k = 4;
            s = GdipGetPathGradientSurroundColorsWithCount(b, sc, &k);
            printf(" got %d %d %08lx", s, k, sc[0]);
            GdipDeleteBrush(b);
        }
        printf("\n");
    }
    GpPath *path;
    GdipCreatePath(0, &path);
    GdipAddPathEllipse(path, 0, 0, 100, 50);
    int np = 0;
    GdipGetPointCount(path, &np);
    GdipCreatePathGradientFromPath(path, &b);
    {
        PointF c;
        RectF r;
        GdipGetPathGradientCenterPoint(b, &c);
        GdipGetPathGradientRect(b, &r);
        printf("ellipse points %d center %.3f %.3f rect %.3f %.3f %.3f %.3f\n", np, z(c.X), z(c.Y), z(r.X), z(r.Y), z(r.Width), z(r.Height));
    }
    GdipDeleteBrush(b);
    GdipDeletePath(path);

    /* matrices */
    GpMatrix *m;
    GdipCreateMatrix2(1, 2, 3, 4, 5, 6, &m);
    GdipRotateMatrix(m, 30, 0);
    elems("rotate prepend", m);
    GdipRotateMatrix(m, 30, 1);
    elems("rotate append", m);
    GdipScaleMatrix(m, 2, 3, 0);
    elems("scale prepend", m);
    GdipTranslateMatrix(m, 7, -8, 1);
    elems("translate append", m);
    GdipShearMatrix(m, 0.5f, 0.25f, 0);
    elems("shear prepend", m);
    BOOL inv = 0;
    GdipIsMatrixInvertible(m, &inv);
    printf("invertible %d invert %d\n", inv, GdipInvertMatrix(m));
    elems("inverted", m);
    PointF tp[2] = {{1, 2}, {-3, 4}};
    GdipTransformMatrixPoints(m, tp, 2);
    printf("points %.3f %.3f %.3f %.3f\n", tp[0].X, tp[0].Y, tp[1].X, tp[1].Y);
    GpMatrix *z;
    GdipCreateMatrix2(1, 2, 2, 4, 0, 0, &z);
    GdipIsMatrixInvertible(z, &inv);
    printf("singular invertible %d invert %d\n", inv, GdipInvertMatrix(z));
    GdipMultiplyMatrix(m, z, 1);
    elems("multiply append", m);
    return 0;
}
