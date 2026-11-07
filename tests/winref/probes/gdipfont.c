/* GDI+ fonts (gdiplus_font.rs, gdiplus_text.rs) as games make them: families by English and
 * Japanese names (MS Gothic, the families every Windows has, ones it does not), their names
 * and design metrics, the generic families, the installed collection, fonts from a family in
 * each unit and style (size, unit, style, family, LOGFONT, height at a DPI), fonts from a
 * LOGFONT (a positive and a negative height, a face GDI+ lacks, MS Shell Dlg, no weight and
 * charset) and the parameter errors; GDI's side of a family GDI+ has (Tahoma enumerated as a
 * TrueType face, its outline metrics' size and name offsets, as x64 lays them out too).
 * needs: fonts
 * libs: -lgdiplus -lole32 -lgdi32 */
#include "../probe.h"

struct found { int calls, truetype, em; };
static int CALLBACK enum_tt(const LOGFONTW *lf, const TEXTMETRICW *tm, DWORD type, LPARAM lp) {
    struct found *f = (struct found *)lp;
    f->calls++;
    if (type & TRUETYPE_FONTTYPE) {
        f->truetype = 1;
        f->em = ((const NEWTEXTMETRICW *)tm)->ntmSizeEM;
    }
    return 0;
}

typedef int GpStatus;
typedef void GpFontFamily, GpFont, GpGraphics, GpFontCollection;
typedef struct { UINT32 GdiplusVersion; void *DebugEventCallback; BOOL SuppressBackgroundThread, SuppressExternalCodecs; } StartupInput;
GpStatus WINAPI GdiplusStartup(ULONG_PTR *, const StartupInput *, void *);
GpStatus WINAPI GdipCreateFontFamilyFromName(const WCHAR *, GpFontCollection *, GpFontFamily **);
GpStatus WINAPI GdipCloneFontFamily(GpFontFamily *, GpFontFamily **);
GpStatus WINAPI GdipDeleteFontFamily(GpFontFamily *);
GpStatus WINAPI GdipGetFamilyName(GpFontFamily *, WCHAR *, LANGID);
GpStatus WINAPI GdipGetEmHeight(GpFontFamily *, INT, UINT16 *);
GpStatus WINAPI GdipGetCellAscent(GpFontFamily *, INT, UINT16 *);
GpStatus WINAPI GdipGetCellDescent(GpFontFamily *, INT, UINT16 *);
GpStatus WINAPI GdipGetLineSpacing(GpFontFamily *, INT, UINT16 *);
GpStatus WINAPI GdipGetGenericFontFamilySansSerif(GpFontFamily **);
GpStatus WINAPI GdipGetGenericFontFamilySerif(GpFontFamily **);
GpStatus WINAPI GdipGetGenericFontFamilyMonospace(GpFontFamily **);
GpStatus WINAPI GdipNewInstalledFontCollection(GpFontCollection **);
GpStatus WINAPI GdipGetFontCollectionFamilyCount(GpFontCollection *, INT *);
GpStatus WINAPI GdipGetFontCollectionFamilyList(GpFontCollection *, INT, GpFontFamily **, INT *);
GpStatus WINAPI GdipCreateFont(GpFontFamily *, float, INT, INT, GpFont **);
GpStatus WINAPI GdipCreateFontFromLogfontW(HDC, const LOGFONTW *, GpFont **);
GpStatus WINAPI GdipDeleteFont(GpFont *);
GpStatus WINAPI GdipCloneFont(GpFont *, GpFont **);
GpStatus WINAPI GdipGetFontSize(GpFont *, float *);
GpStatus WINAPI GdipGetFontUnit(GpFont *, INT *);
GpStatus WINAPI GdipGetFontStyle(GpFont *, INT *);
GpStatus WINAPI GdipGetFamily(GpFont *, GpFontFamily **);
GpStatus WINAPI GdipGetLogFontW(GpFont *, GpGraphics *, LOGFONTW *);
GpStatus WINAPI GdipGetFontHeight(GpFont *, GpGraphics *, float *);
GpStatus WINAPI GdipGetFontHeightGivenDPI(GpFont *, float, float *);
GpStatus WINAPI GdipCreateFromHDC(HDC, GpGraphics **);
GpStatus WINAPI GdipDeleteGraphics(GpGraphics *);

static void hexw(const WCHAR *s) { printf("["); for (; *s; s++) printf("%04x", *s); printf("]"); }

static void family(const WCHAR *name) {
    GpFontFamily *f = NULL, *g = NULL;
    int s = GdipCreateFontFamilyFromName(name, NULL, &f);
    printf("family ");
    hexw(name);
    printf(" %d", s);
    if (!s) {
        WCHAR n[LF_FACESIZE];
        n[0] = 0;
        GdipGetFamilyName(f, n, 0);
        printf(" name ");
        hexw(n);
        GdipGetFamilyName(f, n, 0x409);
        printf(" en ");
        hexw(n);
        GdipCreateFontFamilyFromName(name, NULL, &g);
        printf(" same %d", f == g);
        UINT16 em = 0, a = 0, d = 0, ls = 0;
        GdipGetEmHeight(f, 0, &em);
        GdipGetCellAscent(f, 0, &a);
        GdipGetCellDescent(f, 0, &d);
        GdipGetLineSpacing(f, 0, &ls);
        printf("\n  metrics em %u asc %u desc %u spacing %u", em, a, d, ls);
        GdipDeleteFontFamily(f);
    }
    printf("\n");
}

int main(void) {
    ULONG_PTR tok;
    StartupInput in = {1, NULL, FALSE, FALSE};
    GdiplusStartup(&tok, &in, NULL);
    GpFontFamily *f = NULL, *g = NULL;

    /* errors */
    printf("from name: NULL out %d NULL name %d\n", GdipCreateFontFamilyFromName(L"MS Gothic", NULL, NULL), GdipCreateFontFamilyFromName(NULL, NULL, &f));
    printf("delete NULL family %d font %d\n", GdipDeleteFontFamily(NULL), GdipDeleteFont(NULL));
    printf("name of NULL %d\n", GdipGetFamilyName(NULL, NULL, 0));
    printf("installed NULL %d\n", GdipNewInstalledFontCollection(NULL));

    /* families */
    family(L"MS Gothic");
    family(L"\xff2d\xff33 \x30b4\x30b7\x30c3\x30af");
    family(L"ms ui gothic");
    family(L"MS PGothic");
    family(L"Tahoma");
    family(L"Arial");
    family(L"Times New Roman");
    family(L"Courier New");
    family(L"MS Sans Serif");
    family(L"ThisFontShouldNotExist");
    GpStatus (WINAPI *gen[3])(GpFontFamily **) = {GdipGetGenericFontFamilySansSerif, GdipGetGenericFontFamilySerif, GdipGetGenericFontFamilyMonospace};
    for (int i = 0; i < 3; i++) {
        WCHAR n[LF_FACESIZE];
        n[0] = 0;
        int s = gen[i](&f);
        GdipGetFamilyName(f, n, 0);
        printf("generic %d: %d ", i, s);
        hexw(n);
        printf("\n");
    }

    /* the installed collection */
    GpFontCollection *c1, *c2;
    GdipNewInstalledFontCollection(&c1);
    GdipNewInstalledFontCollection(&c2);
    int n = 0, found = 0;
    GdipGetFontCollectionFamilyCount(c1, &n);
    printf("installed: same %d some %d\n", c1 == c2, n > 0);
    printf("list: 0 sought %d NULL list %d\n", GdipGetFontCollectionFamilyList(c1, 0, &f, &found), GdipGetFontCollectionFamilyList(c1, 1, NULL, &found));
    GpFontFamily *l1 = NULL, *l2 = NULL;
    GdipGetFontCollectionFamilyList(c1, 1, &l1, &found);
    GdipGetFontCollectionFamilyList(c1, 1, &l2, &found);
    printf("list: found %d same %d\n", found, l1 == l2);
    GdipCreateFontFamilyFromName(L"MS Gothic", c1, &f);
    GpFontFamily **all = malloc(n * sizeof *all);
    GdipGetFontCollectionFamilyList(c1, n, all, &found);
    int in_list = 0;
    for (int i = 0; i < found; i++) in_list |= all[i] == f;
    printf("list: all %d has MS Gothic %d\n", found == n, in_list);
    GdipCloneFontFamily(f, &g);
    printf("clone same %d\n", f == g);

    /* fonts from the family */
    HDC dc = CreateCompatibleDC(NULL);
    GpGraphics *gr;
    GdipCreateFromHDC(dc, &gr);
    UINT16 em = 0, ls = 0;
    GdipGetEmHeight(f, 0, &em);
    GdipGetLineSpacing(f, 0, &ls);
    for (int unit = 0; unit <= 7; unit++) {
        GpFont *font = NULL;
        int s = GdipCreateFont(f, 24, 0, unit, &font);
        printf("font unit %d: %d", unit, s);
        if (!s) {
            float size = 0, h = 0, h2 = 0, h3 = 0;
            int u = -1;
            GdipGetFontSize(font, &size);
            GdipGetFontUnit(font, &u);
            GdipGetFontHeightGivenDPI(font, 96, &h);
            GdipGetFontHeightGivenDPI(font, 192, &h2);
            GdipGetFontHeight(font, gr, &h3);
            LOGFONTW lf;
            memset(&lf, 0xcc, sizeof lf);
            GdipGetLogFontW(font, gr, &lf);
            /* the heights over the spacing per em (pixels at the DPI) */
            double k = (double)ls / em;
            printf(" size %.1f unit %d height96/k %.3f height192/k %.3f height/k %.3f lf %ld %ld %ld %ld %ld %d %d %d %d %d %d %d %d", size, u, h / k, h2 / k, h3 / k,
                   lf.lfHeight, lf.lfWidth, lf.lfEscapement, lf.lfOrientation, lf.lfWeight, lf.lfItalic, lf.lfUnderline, lf.lfStrikeOut,
                   lf.lfCharSet, lf.lfOutPrecision, lf.lfClipPrecision, lf.lfQuality, lf.lfPitchAndFamily);
            GdipDeleteFont(font);
        }
        printf("\n");
    }
    GpFont *font = NULL;
    printf("font: size 0 %d negative %d NULL family %d\n", GdipCreateFont(f, 0, 0, 2, &font), GdipCreateFont(f, -3, 0, 2, &font), GdipCreateFont(NULL, 12, 0, 2, &font));
    for (int style = 0; style < 16; style += 5) {
        int s = GdipCreateFont(f, 12, style, 2, &font);
        int st = -1;
        GdipGetFontStyle(font, &st);
        LOGFONTW lf;
        GdipGetLogFontW(font, gr, &lf);
        GpFontFamily *ff = NULL;
        GdipGetFamily(font, &ff);
        GpFont *cl = NULL;
        GdipCloneFont(font, &cl);
        float cs = 0;
        GdipGetFontSize(cl, &cs);
        printf("style %d: %d got %d weight %ld %d %d %d family same %d clone size %.1f\n", style, s, st, lf.lfWeight, lf.lfItalic, lf.lfUnderline, lf.lfStrikeOut, ff == f, cs);
        GdipDeleteFont(cl);
        GdipDeleteFont(font);
    }
    printf("logfont NULL graphics %d\n", (GdipCreateFont(f, 12, 0, 2, &font), GdipGetLogFontW(font, NULL, (LOGFONTW *)malloc(sizeof(LOGFONTW)))));
    GdipDeleteFont(font);

    /* fonts from a LOGFONT */
    static const struct { const WCHAR *face; int height, weight, charset; } lfs[] = {
        {L"MS Gothic", -20, 400, 128}, {L"MS Gothic", 20, 400, 128}, {L"MS Gothic", 0, 400, 128},
        {L"ThisFontShouldNotExist", -20, 400, 0}, {L"", -20, 400, 0}, {L"MS Shell Dlg", -20, 400, 0},
        {L"ThisFontShouldNotExist", -20, 0, 0}, {L"", -20, 0, 0}, {L"MS Gothic", -20, 0, 0},
    };
    for (int i = 0; i < (int)(sizeof lfs / sizeof lfs[0]); i++) {
        LOGFONTW lf;
        memset(&lf, 0, sizeof lf);
        lstrcpyW(lf.lfFaceName, lfs[i].face);
        lf.lfHeight = lfs[i].height;
        lf.lfWeight = lfs[i].weight;
        lf.lfCharSet = lfs[i].charset;
        font = NULL;
        int s = GdipCreateFontFromLogfontW(dc, &lf, &font);
        printf("logfont %d: %d", i, s);
        if (!s) {
            float size = 0;
            int u = -1;
            GdipGetFontSize(font, &size);
            GdipGetFontUnit(font, &u);
            LOGFONTW o;
            GdipGetLogFontW(font, gr, &o);
            printf(" unit %d", u);
            if (lfs[i].height <= 0) printf(" size %.1f", size);
            printf(" lf %ld %ld charset %d face ", o.lfHeight, o.lfWeight, o.lfCharSet);
            hexw(o.lfFaceName);
            GdipDeleteFont(font);
        }
        printf("\n");
    }
    GdipDeleteGraphics(gr);

    /* GDI: the same family enumerated and its outline metrics */
    LOGFONTW q = { 0 };
    lstrcpyW(q.lfFaceName, L"Tahoma");
    q.lfCharSet = DEFAULT_CHARSET;
    struct found fd = { 0 };
    int er = EnumFontFamiliesExW(dc, &q, enum_tt, (LPARAM)&fd, 0);
    printf("enum Tahoma: %d calls %d truetype %d em>0 %d\n", er, fd.calls, fd.truetype, fd.em > 0);
    q.lfHeight = -13;
    HFONT hf = CreateFontIndirectW(&q);
    HGDIOBJ old = SelectObject(dc, hf);
    UINT need = GetOutlineTextMetricsW(dc, 0, NULL);
    OUTLINETEXTMETRICW *otm = calloc(1, need + 16);
    UINT got = GetOutlineTextMetricsW(dc, need, otm);
    printf("otm: need>=sizeof %d got==need %d otmSize==need %d small %u\n", need >= sizeof *otm, got == need, otm->otmSize == need,
           GetOutlineTextMetricsW(dc, sizeof *otm - 1, otm));
    size_t fam = (size_t)otm->otmpFamilyName, full = (size_t)otm->otmpFullName;
    printf("otm names: family in %d after %d full in %d ascent==-height %d\n", fam > 0 && fam < need, fam >= sizeof *otm, full > 0 && full < need,
           otm->otmTextMetrics.tmAscent + otm->otmTextMetrics.tmDescent - otm->otmTextMetrics.tmInternalLeading == 13);
    free(otm);
    SelectObject(dc, old);
    DeleteObject(hf);
    DeleteDC(dc);
    return 0;
}
