/* DirectWrite's system font collection for the Japanese faces games and engines ask for (MS
 * Gothic, MS PGothic, MS UI Gothic; the runner has no MS Mincho): FindFamilyName by English and
 * Japanese names in any case, the family's localized names, the matching font's properties,
 * HasCharacter and design metrics, its font face's type, index and file (the local loader's
 * path), and the system font fallback's MapCharacters (Godot finds fonts for text its own
 * fonts lack this way) for text those faces cover.
 * libs: -ldwrite -luuid */
#define COBJMACROS
#include "../probe.h"
#include <dwrite_2.h>
#include <wchar.h>

static const GUID IID_F = { 0xb859ee5a, 0xd838, 0x4b5b, { 0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48 } };
static const GUID IID_F2 = { 0x0439fc60, 0xca44, 0x4994, { 0x8d, 0xee, 0x3a, 0x9a, 0xf7, 0xb7, 0x32, 0xec } };
static const GUID IID_LOCAL = { 0xb2d9f3ec, 0xc9fe, 0x4a11, { 0xa2, 0xec, 0xd8, 0x62, 0x08, 0xf7, 0xc0, 0xa2 } };

static void hexw(const WCHAR *s) { printf("["); for (; *s; s++) printf("%04x", *s); printf("]"); }

static void names(IDWriteLocalizedStrings *s) {
    UINT32 n = IDWriteLocalizedStrings_GetCount(s);
    const WCHAR *loc[] = { L"en-us", L"ja-jp", L"EN-US" };
    for (int k = 0; k < 3; k++) {
        UINT32 i = 99;
        BOOL ex = 7;
        HRESULT hr = IDWriteLocalizedStrings_FindLocaleName(s, loc[k], &i, &ex);
        printf(" %ls:%08lx,%d", loc[k], hr, ex);
        if (ex && i < n) {
            WCHAR b[64];
            UINT32 len = 0;
            IDWriteLocalizedStrings_GetStringLength(s, i, &len);
            printf(",%u", len);
            if (!IDWriteLocalizedStrings_GetString(s, i, b, 64)) hexw(b);
            printf(",short %08lx", IDWriteLocalizedStrings_GetString(s, i, b, len));
        }
    }
    printf("\n");
}

static void family_name(IDWriteFont *f) {
    IDWriteFontFamily *fam = NULL;
    IDWriteLocalizedStrings *s = NULL;
    if (IDWriteFont_GetFontFamily(f, &fam) || IDWriteFontFamily_GetFamilyNames(fam, &s)) { printf("?"); return; }
    UINT32 i = 0;
    BOOL ex = 0;
    WCHAR b[64] = { 0 };
    IDWriteLocalizedStrings_FindLocaleName(s, L"en-us", &i, &ex);
    if (ex) IDWriteLocalizedStrings_GetString(s, i, b, 64);
    printf("%ls", b);
    IDWriteLocalizedStrings_Release(s);
    IDWriteFontFamily_Release(fam);
}

static void face(IDWriteFontCollection *coll, const WCHAR *name) {
    UINT32 i = 0;
    BOOL ex = 0;
    HRESULT hr = IDWriteFontCollection_FindFamilyName(coll, name, &i, &ex);
    printf("family "); hexw(name); printf(" %08lx exists %d\n", hr, ex);
    if (!ex) return;
    IDWriteFontFamily *fam = NULL;
    IDWriteFontCollection_GetFontFamily(coll, i, &fam);
    IDWriteLocalizedStrings *s = NULL;
    IDWriteFontFamily_GetFamilyNames(fam, &s);
    printf("  names"); names(s);
    IDWriteLocalizedStrings_Release(s);
    IDWriteFont *f = NULL;
    hr = IDWriteFontFamily_GetFirstMatchingFont(fam, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, &f);
    printf("  match %08lx weight %d stretch %d style %d sim %d symbol %d\n", hr, IDWriteFont_GetWeight(f), IDWriteFont_GetStretch(f), IDWriteFont_GetStyle(f),
           IDWriteFont_GetSimulations(f), IDWriteFont_IsSymbolFont(f));
    IDWriteFont_GetFaceNames(f, &s);
    printf("  face names"); names(s);
    IDWriteLocalizedStrings_Release(s);
    UINT32 chars[] = { 'A', 0x3042, 0x6f22, 0xff21, 0x2460, 0x1f600 };
    printf("  has");
    for (int k = 0; k < 6; k++) { BOOL b = 7; IDWriteFont_HasCharacter(f, chars[k], &b); printf(" %x:%d", chars[k], b); }
    printf("\n");
    DWRITE_FONT_METRICS m;
    IDWriteFont_GetMetrics(f, &m);
    printf("  metrics em %u asc %u desc %u gap %d cap %u x %u ul %d %u st %d %u\n", m.designUnitsPerEm, m.ascent, m.descent, m.lineGap, m.capHeight, m.xHeight,
           m.underlinePosition, m.underlineThickness, m.strikethroughPosition, m.strikethroughThickness);
    IDWriteFontFace *ff = NULL;
    hr = IDWriteFont_CreateFontFace(f, &ff);
    UINT32 nf = 0;
    IDWriteFontFace_GetFiles(ff, &nf, NULL);
    printf("  face %08lx type %d index %u files %u\n", hr, IDWriteFontFace_GetType(ff), IDWriteFontFace_GetIndex(ff), nf);
    IDWriteFontFile *file = NULL;
    nf = 1;
    IDWriteFontFace_GetFiles(ff, &nf, &file);
    const void *key = NULL;
    UINT32 ks = 0;
    IDWriteFontFile_GetReferenceKey(file, &key, &ks);
    IDWriteFontFileLoader *ld = NULL;
    IDWriteFontFile_GetLoader(file, &ld);
    IDWriteLocalFontFileLoader *local = NULL;
    hr = IDWriteFontFileLoader_QueryInterface(ld, &IID_LOCAL, (void **)&local);
    printf("  local loader %08lx", hr);
    if (!hr) {
        UINT32 len = 0;
        WCHAR p[MAX_PATH];
        IDWriteLocalFontFileLoader_GetFilePathLengthFromKey(local, key, ks, &len);
        hr = IDWriteLocalFontFileLoader_GetFilePathFromKey(local, key, ks, p, MAX_PATH);
        /* the file name, lowercase (the directory is the system's) */
        WCHAR *nm = wcsrchr(p, L'\\');
        nm = nm ? nm + 1 : p;
        for (WCHAR *q = nm; *q; q++) *q = towlower(*q);
        printf(" path %08lx len %d %ls short %08lx", hr, (int)len - (int)wcslen(p), nm, IDWriteLocalFontFileLoader_GetFilePathFromKey(local, key, ks, p, len));
        IDWriteLocalFontFileLoader_Release(local);
    }
    BOOL sup = 0;
    DWRITE_FONT_FILE_TYPE ft = 0;
    DWRITE_FONT_FACE_TYPE fct = 0;
    UINT32 nfaces = 0;
    hr = IDWriteFontFile_Analyze(file, &sup, &ft, &fct, &nfaces);
    printf("\n  analyze %08lx %d %d %d %u\n", hr, sup, ft, fct, nfaces);
    IDWriteFont *back = NULL;
    hr = IDWriteFontCollection_GetFontFromFontFace(coll, ff, &back);
    printf("  from face %08lx ", hr);
    if (!hr) { family_name(back); IDWriteFont_Release(back); }
    printf("\n");
    IDWriteFontFileLoader_Release(ld);
    IDWriteFontFile_Release(file);
    IDWriteFontFace_Release(ff);
    IDWriteFont_Release(f);
    IDWriteFontFamily_Release(fam);
}

/* an IDWriteTextAnalysisSource over a fixed string */
typedef struct { IDWriteTextAnalysisSource src; const WCHAR *text; UINT32 len; } Source;
static HRESULT STDMETHODCALLTYPE s_qi(IDWriteTextAnalysisSource *t, REFIID r, void **o) { *o = t; return 0; }
static ULONG STDMETHODCALLTYPE s_ref(IDWriteTextAnalysisSource *t) { return 1; }
static HRESULT STDMETHODCALLTYPE s_at(IDWriteTextAnalysisSource *t, UINT32 pos, const WCHAR **text, UINT32 *len) {
    Source *s = (Source *)t;
    if (pos >= s->len) { *text = NULL; *len = 0; } else { *text = s->text + pos; *len = s->len - pos; }
    return 0;
}
static HRESULT STDMETHODCALLTYPE s_before(IDWriteTextAnalysisSource *t, UINT32 pos, const WCHAR **text, UINT32 *len) {
    Source *s = (Source *)t;
    if (pos == 0 || pos > s->len) { *text = NULL; *len = 0; } else { *text = s->text; *len = pos; }
    return 0;
}
static DWRITE_READING_DIRECTION STDMETHODCALLTYPE s_dir(IDWriteTextAnalysisSource *t) { return DWRITE_READING_DIRECTION_LEFT_TO_RIGHT; }
static HRESULT STDMETHODCALLTYPE s_loc(IDWriteTextAnalysisSource *t, UINT32 pos, UINT32 *len, const WCHAR **loc) { *len = ((Source *)t)->len - pos; *loc = L"ja-jp"; return 0; }
static HRESULT STDMETHODCALLTYPE s_num(IDWriteTextAnalysisSource *t, UINT32 pos, UINT32 *len, IDWriteNumberSubstitution **n) { *len = ((Source *)t)->len - pos; *n = NULL; return 0; }
static IDWriteTextAnalysisSourceVtbl svt = { s_qi, s_ref, s_ref, s_at, s_before, s_dir, s_loc, s_num };

static void fallback(IDWriteFactory *fac, IDWriteFontCollection *coll) {
    IDWriteFactory2 *f2 = NULL;
    if (IDWriteFactory_QueryInterface(fac, &IID_F2, (void **)&f2)) { printf("no factory2\n"); return; }
    IDWriteFontFallback *fb = NULL;
    HRESULT hr = IDWriteFactory2_GetSystemFontFallback(f2, &fb);
    printf("fallback %08lx\n", hr);
    IDWriteNumberSubstitution *ns = NULL;
    hr = IDWriteFactory_CreateNumberSubstitution(fac, DWRITE_NUMBER_SUBSTITUTION_METHOD_NONE, L"ja-jp", TRUE, &ns);
    printf("number substitution %08lx %d\n", hr, ns != NULL);
    if (ns) IDWriteNumberSubstitution_Release(ns);
    static const struct { const WCHAR *text, *family; UINT32 pos; } t[] = {
        { L"abc\x3042\x3044\x3046", L"MS Gothic", 0 }, { L"\x6f22\x5b57 abc", L"MS PGothic", 0 }, { L"\xff21\xff22\x3000\x3042", L"\xff2d\xff33 \x30b4\x30b7\x30c3\x30af", 0 },
        { L"xyz\x3042\x3044", L"MS UI Gothic", 3 }, { L"\x3042\x3044\x3046", L"MS PGothic", 1 },
    };
    for (unsigned i = 0; i < sizeof t / sizeof *t; i++) {
        Source s = { { &svt }, t[i].text, (UINT32)wcslen(t[i].text) };
        printf("  map "); hexw(t[i].text); printf(" at %u\n", t[i].pos);
        UINT32 mapped = 99;
        IDWriteFont *font = NULL;
        FLOAT scale = -1;
        hr = IDWriteFontFallback_MapCharacters(fb, &s.src, t[i].pos, s.len - t[i].pos, coll, t[i].family, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                                               DWRITE_FONT_STRETCH_NORMAL, &mapped, &font, &scale);
        printf("   %08lx mapped %u scale %d font ", hr, mapped, (int)(scale * 100));
        if (font) { family_name(font); IDWriteFont_Release(font); }
        printf("\n");
    }
    IDWriteFontFallback_Release(fb);
    IDWriteFactory2_Release(f2);
}

int main(void) {
    IDWriteFactory *f = NULL;
    HRESULT hr = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, &IID_F, (IUnknown **)&f);
    IDWriteFontCollection *coll = NULL;
    HRESULT hc = IDWriteFactory_GetSystemFontCollection(f, &coll, FALSE);
    UINT32 i = 0;
    BOOL ex = 7;
    /* (sake's side without the Windows fonts in its C:\Windows\Fonts: nothing to compare) */
    IDWriteFontFamily *fam = NULL;
    IDWriteFont *font = NULL;
    BOOL a = FALSE;
    if (IDWriteFontCollection_FindFamilyName(coll, L"MS Gothic", &i, &ex) || !ex || IDWriteFontCollection_GetFontFamily(coll, i, &fam) ||
        IDWriteFontFamily_GetFirstMatchingFont(fam, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, &font) ||
        IDWriteFont_HasCharacter(font, 'A', &a) || !a) {
        printf("skip no MS Gothic\n");
        return 0;
    }
    IDWriteFont_Release(font);
    IDWriteFontFamily_Release(fam);
    printf("factory %08lx collection %08lx\n", hr, hc);
    ex = 7;
    IDWriteFontCollection_FindFamilyName(coll, L"No Such Family", &i, &ex);
    printf("missing exists %d index %d\n", ex, (int)i);
    const WCHAR *fams[] = { L"MS Gothic", L"\xff2d\xff33 \x30b4\x30b7\x30c3\x30af", L"ms pgothic", L"MS UI Gothic" };
    for (int k = 0; k < 4; k++) face(coll, fams[k]);
    fallback(f, coll);
    IDWriteFontCollection_Release(coll);
    IDWriteFactory_Release(f);
    return 0;
}
