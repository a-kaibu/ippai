/* DirectWrite text layouts as games draw them with their own renderer (AdvHD): the line
 * metrics (lengths, trailing white space, newlines), the cluster metrics (lengths and flags),
 * character spacing, and IDWriteTextLayout::Draw calling an IDWriteTextRenderer's DrawGlyphRun
 * for each line (glyph counts, the text each run covers, its cluster map). Then
 * IDWriteFactory2::TranslateColorGlyphRun on a plain font (no color glyphs) and
 * IDWriteFontFace::GetGlyphRunOutline into a Direct2D path geometry (as outlined text is drawn).
 * Only what the font does not change is printed (no widths or positions).
 * libs: -ldwrite -ld2d1 -luuid
 * needs: fonts */
#define COBJMACROS
#define INITGUID
#include "../probe.h"
#include <d2d1.h>
#include <dwrite_2.h>

/* ---- a renderer that prints the runs ---- */
typedef struct { IDWriteTextRendererVtbl *lpVtbl; int runs; float last_y; } Renderer;

static HRESULT STDMETHODCALLTYPE r_qi(IDWriteTextRenderer *t, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_IDWritePixelSnapping) || IsEqualIID(iid, &IID_IDWriteTextRenderer)) {
        *out = t;
        return S_OK;
    }
    *out = NULL;
    return E_NOINTERFACE;
}
static ULONG STDMETHODCALLTYPE r_ref(IDWriteTextRenderer *t) { return 1; }
static HRESULT STDMETHODCALLTYPE r_snap_disabled(IDWriteTextRenderer *t, void *ctx, BOOL *v) { *v = FALSE; return S_OK; }
static HRESULT STDMETHODCALLTYPE r_transform(IDWriteTextRenderer *t, void *ctx, DWRITE_MATRIX *m) {
    DWRITE_MATRIX id = { 1, 0, 0, 1, 0, 0 };
    *m = id;
    return S_OK;
}
static HRESULT STDMETHODCALLTYPE r_ppd(IDWriteTextRenderer *t, void *ctx, FLOAT *v) { *v = 1; return S_OK; }
static HRESULT STDMETHODCALLTYPE r_glyphs(IDWriteTextRenderer *t, void *ctx, FLOAT x, FLOAT y, DWRITE_MEASURING_MODE mode,
                                          const DWRITE_GLYPH_RUN *run, const DWRITE_GLYPH_RUN_DESCRIPTION *d, IUnknown *effect) {
    Renderer *r = (Renderer *)t;
    char map[256] = "", *o = map;
    for (UINT32 i = 0; i < d->stringLength && i < 40; i++) o += sprintf(o, " %u", d->clusterMap[i]);
    char text[256] = "", *p = text;
    for (UINT32 i = 0; i < d->stringLength && i < 40; i++) p += sprintf(p, "%04x", d->string[i]);
    printf("run %d ctx %d mode %d glyphs %u em %d sideways %d bidi %u text %s at %u map%s below %d face %d\n", r->runs, ctx == (void *)7,
           mode, run->glyphCount, (int)run->fontEmSize, run->isSideways, run->bidiLevel, text, d->textPosition, map,
           r->runs == 0 || y > r->last_y, run->fontFace != NULL);
    r->runs++;
    r->last_y = y;
    return S_OK;
}
static HRESULT STDMETHODCALLTYPE r_line(IDWriteTextRenderer *t, void *ctx, FLOAT x, FLOAT y, const void *u, IUnknown *e) { return S_OK; }
static HRESULT STDMETHODCALLTYPE r_inline(IDWriteTextRenderer *t, void *ctx, FLOAT x, FLOAT y, IDWriteInlineObject *o, BOOL s, BOOL rtl, IUnknown *e) { return S_OK; }
static IDWriteTextRendererVtbl r_vtbl = { r_qi, r_ref, r_ref, r_snap_disabled, r_transform, r_ppd, r_glyphs,
                                          (void *)r_line, (void *)r_line, r_inline };

static IDWriteFactory2 *dw;
static IDWriteTextFormat *fmt;

static IDWriteTextLayout *layout(const WCHAR *s) {
    IDWriteTextLayout *l = NULL;
    HRESULT hr = IDWriteFactory2_CreateTextLayout(dw, s, (UINT32)wcslen(s), fmt, 1000, 1000, &l);
    if (FAILED(hr)) printf("CreateTextLayout %#lx\n", hr);
    return l;
}

static void lines(const char *name, const WCHAR *s) {
    IDWriteTextLayout *l = layout(s);
    if (!l) return;
    DWRITE_LINE_METRICS m[8];
    UINT32 n = 0;
    HRESULT hr = IDWriteTextLayout_GetLineMetrics(l, m, 0, &n);
    printf("%s lines: too small %#lx count %u\n", name, hr, n);
    hr = IDWriteTextLayout_GetLineMetrics(l, m, 8, &n);
    for (UINT32 i = 0; i < n && i < 8; i++)
        printf("%s line %u: length %u trailing %u newline %u trimmed %d\n", name, i, m[i].length, m[i].trailingWhitespaceLength, m[i].newlineLength, m[i].isTrimmed);
    DWRITE_CLUSTER_METRICS c[32];
    hr = IDWriteTextLayout_GetClusterMetrics(l, c, 0, &n);
    printf("%s clusters: too small %#lx count %u\n", name, hr, n);
    hr = IDWriteTextLayout_GetClusterMetrics(l, c, 32, &n);
    for (UINT32 i = 0; i < n && i < 32; i++)
        printf("%s cluster %u: length %u wrap %d space %d newline %d\n", name, i, c[i].length, c[i].canWrapLineAfter, c[i].isWhitespace, c[i].isNewline);
    Renderer r = { &r_vtbl, 0, 0 };
    hr = IDWriteTextLayout_Draw(l, (void *)7, (IDWriteTextRenderer *)&r, 10, 20);
    printf("%s Draw %#lx runs %d\n", name, hr, r.runs);
    IDWriteTextLayout_Release(l);
}

int main(void) {
    HRESULT hr = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, &IID_IDWriteFactory2, (IUnknown **)&dw);
    printf("DWriteCreateFactory %#lx\n", hr);
    if (FAILED(hr)) return 0;
    hr = IDWriteFactory2_CreateTextFormat(dw, L"Arial", NULL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 20, L"ja-jp", &fmt);
    printf("CreateTextFormat %#lx\n", hr);
    IDWriteTextFormat_SetWordWrapping(fmt, DWRITE_WORD_WRAPPING_NO_WRAP);

    lines("plain", L"ab cd");
    lines("newlines", L"ab\ncd  \r\nef\n");
    lines("empty", L"");
    lines("japanese", L"\x4e16\x754c\x3092\x898b");
    lines("surrogates", L"a\xd83d\xde00" L"b");

    /* character spacing: each advance grows; a negative minimum is refused */
    IDWriteTextLayout *l = layout(L"abc");
    IDWriteTextLayout1 *l1 = NULL;
    if (l && SUCCEEDED(IDWriteTextLayout_QueryInterface(l, &IID_IDWriteTextLayout1, (void **)&l1))) {
        DWRITE_TEXT_METRICS before, after;
        DWRITE_TEXT_RANGE all = { 0, 3 };
        IDWriteTextLayout_GetMetrics(l, &before);
        hr = IDWriteTextLayout1_SetCharacterSpacing(l1, 1, 2, 0, all);
        IDWriteTextLayout_GetMetrics(l, &after);
        printf("SetCharacterSpacing %#lx grows %d\n", hr, (int)(after.widthIncludingTrailingWhitespace - before.widthIncludingTrailingWhitespace + 0.5f));
        hr = IDWriteTextLayout1_SetCharacterSpacing(l1, 0, 0, -1, all);
        printf("SetCharacterSpacing negative minimum %#lx\n", hr);
        IDWriteTextLayout1_Release(l1);
    }

    /* a plain font has no color glyphs; its outline goes into a path geometry */
    IDWriteFontCollection *fc = NULL;
    IDWriteFontFamily *fam = NULL;
    IDWriteFont *font = NULL;
    IDWriteFontFace *face = NULL;
    UINT32 idx = 0;
    BOOL exists = FALSE;
    IDWriteFactory2_GetSystemFontCollection(dw, &fc, FALSE);
    IDWriteFontCollection_FindFamilyName(fc, L"Arial", &idx, &exists);
    if (!exists) idx = 0;
    IDWriteFontCollection_GetFontFamily(fc, idx, &fam);
    IDWriteFontFamily_GetFirstMatchingFont(fam, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, &font);
    hr = IDWriteFont_CreateFontFace(font, &face);
    printf("CreateFontFace %#lx\n", hr);
    UINT32 cps[2] = { 'A', 'B' };
    UINT16 gi[2] = { 0, 0 };
    hr = IDWriteFontFace_GetGlyphIndices(face, cps, 2, gi);
    printf("GetGlyphIndices %#lx nonzero %d %d differ %d\n", hr, gi[0] != 0, gi[1] != 0, gi[0] != gi[1]);
    FLOAT adv[2] = { 10, 10 };
    DWRITE_GLYPH_RUN run = { face, 20, 2, gi, adv, NULL, FALSE, 0 };
    IDWriteColorGlyphRunEnumerator *en = (void *)1;
    hr = IDWriteFactory2_TranslateColorGlyphRun(dw, 0, 0, &run, NULL, DWRITE_MEASURING_MODE_NATURAL, NULL, 0, &en);
    printf("TranslateColorGlyphRun %#lx enumerator %d\n", hr, en != NULL);

    ID2D1Factory *d2 = NULL;
    hr = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, &IID_ID2D1Factory, NULL, (void **)&d2);
    printf("D2D1CreateFactory %#lx\n", hr);
    ID2D1PathGeometry *path = NULL;
    ID2D1GeometrySink *sink = NULL;
    hr = ID2D1Factory_CreatePathGeometry(d2, &path);
    printf("CreatePathGeometry %#lx\n", hr);
    hr = ID2D1PathGeometry_Open(path, &sink);
    printf("Open %#lx\n", hr);
    hr = IDWriteFontFace_GetGlyphRunOutline(face, 20, gi, adv, NULL, 2, FALSE, FALSE, (ID2D1SimplifiedGeometrySink *)sink);
    printf("GetGlyphRunOutline %#lx\n", hr);
    hr = ID2D1GeometrySink_Close(sink);
    printf("Close %#lx\n", hr);
    D2D1_MATRIX_3X2_F m = { 1, 0, 0, 1, 5, 7 };
    ID2D1TransformedGeometry *moved = NULL;
    hr = ID2D1Factory_CreateTransformedGeometry(d2, (ID2D1Geometry *)path, &m, &moved);
    printf("CreateTransformedGeometry %#lx\n", hr);
    D2D1_STROKE_STYLE_PROPERTIES sp = { D2D1_CAP_STYLE_FLAT, D2D1_CAP_STYLE_FLAT, D2D1_CAP_STYLE_FLAT, D2D1_LINE_JOIN_ROUND, 10, D2D1_DASH_STYLE_SOLID, 0 };
    ID2D1StrokeStyle *ss = NULL;
    hr = ID2D1Factory_CreateStrokeStyle(d2, &sp, NULL, 0, &ss);
    printf("CreateStrokeStyle %#lx\n", hr);
    return 0;
}
