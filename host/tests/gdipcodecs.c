/* libs: -lgdiplus -lole32 */
/* GDI+'s decoder list as programs read it (EntisGLS' WitchWizard, #162): GdipGetImageDecodersSize,
 * then GdipGetImageDecoders into a heap buffer of exactly that size. Every string and signature
 * the ImageCodecInfo structs point to must lie inside the buffer (sake wrote the DLL names past
 * its end, over the game's next object), the bytes after it stay untouched, and the built-in
 * BMP/JPEG/GIF/PNG decoders are there with their signatures. Then a stream holding an image
 * GDI+ can't read (an Entis ERI header) is refused: the status and that no bitmap is made.
 * (sake: gdiplus.rs decoders_blob; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <objbase.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

typedef int GpStatus;
typedef struct { UINT32 GdiplusVersion; void *DebugEventCallback; BOOL SuppressBackgroundThread, SuppressExternalCodecs; } StartupInput;
typedef struct {
    CLSID Clsid;
    GUID FormatID;
    const WCHAR *CodecName, *DllName, *FormatDescription, *FilenameExtension, *MimeType;
    DWORD Flags, Version, SigCount, SigSize;
    const BYTE *SigPattern, *SigMask;
} ImageCodecInfo;
GpStatus WINAPI GdiplusStartup(ULONG_PTR *, const StartupInput *, void *);
GpStatus WINAPI GdipGetImageDecodersSize(UINT *, UINT *);
GpStatus WINAPI GdipGetImageDecoders(UINT, UINT, ImageCodecInfo *);
GpStatus WINAPI GdipCreateBitmapFromStream(IStream *, void **);
GpStatus WINAPI GdipLoadImageFromStream(IStream *, void **);

static const BYTE *lo, *hi;
static int inside(const void *p, size_t n) { return (const BYTE *)p >= lo && (const BYTE *)p + n <= hi; }
static int wstr_inside(const WCHAR *s) {
    if (!inside(s, 2))
        return 0;
    for (const WCHAR *q = s; inside(q, 2); q++)
        if (!*q)
            return 1;
    return 0;
}

int main(void) {
    ULONG_PTR tok;
    StartupInput si = {1, NULL, FALSE, FALSE};
    printf("startup %d\n", GdiplusStartup(&tok, &si, NULL));
    UINT n = 0, size = 0;
    GpStatus s = GdipGetImageDecodersSize(&n, &size);
    printf("size status %d, count nonzero %d\n", s, n > 0);
    BYTE *buf = HeapAlloc(GetProcessHeap(), 0, size + 256);
    memset(buf, 0xcc, size + 256);
    lo = buf, hi = buf + size;
    ImageCodecInfo *c = (ImageCodecInfo *)buf;
    s = GdipGetImageDecoders(n, size, c);
    printf("decoders status %d\n", s);
    int guard = 1;
    for (int i = 0; i < 256; i++)
        guard &= buf[size + i] == 0xcc;
    printf("bytes after the buffer untouched %d\n", guard);
    int strings = 1, sigs = 1, decoder = 1;
    int bmp = 0, jpeg = 0, gif = 0, png = 0;
    for (UINT i = 0; i < n; i++) {
        strings &= wstr_inside(c[i].CodecName) && wstr_inside(c[i].FormatDescription) && wstr_inside(c[i].FilenameExtension) && wstr_inside(c[i].MimeType)
                   && (!c[i].DllName || wstr_inside(c[i].DllName));
        sigs &= c[i].SigCount * c[i].SigSize == 0 || (inside(c[i].SigPattern, c[i].SigCount * c[i].SigSize) && inside(c[i].SigMask, c[i].SigCount * c[i].SigSize));
        decoder &= (c[i].Flags & 2) != 0;
        if (!wstr_inside(c[i].MimeType))
            continue;
        const BYTE *sp = c[i].SigPattern;
        if (!wcscmp(c[i].MimeType, L"image/bmp"))
            bmp = c[i].SigSize == 2 && !memcmp(sp, "BM", 2);
        if (!wcscmp(c[i].MimeType, L"image/jpeg"))
            jpeg = c[i].SigSize == 2 && !memcmp(sp, "\xff\xd8", 2);
        if (!wcscmp(c[i].MimeType, L"image/gif"))
            gif = c[i].SigSize >= 4 && !memcmp(sp, "GIF8", 4);
        if (!wcscmp(c[i].MimeType, L"image/png"))
            png = c[i].SigSize == 8 && !memcmp(sp, "\x89PNG\r\n\x1a\n", 8);
    }
    printf("strings inside %d, signatures inside %d, all decoders %d\n", strings, sigs, decoder);
    printf("bmp %d jpeg %d gif %d png %d\n", bmp, jpeg, gif, png);
    s = GdipGetImageDecoders(n, size - 1, c);
    printf("too small refused %d\n", s != 0);

    /* an ERI image: "Entis\x1a" and zeros */
    static BYTE eri[0x400] = "Entis\x1a\0\0\0\x01\0\x02";
    HGLOBAL g = GlobalAlloc(GMEM_MOVEABLE, sizeof eri);
    memcpy(GlobalLock(g), eri, sizeof eri);
    GlobalUnlock(g);
    IStream *st;
    CreateStreamOnHGlobal(g, TRUE, &st);
    void *bm = (void *)1;
    s = GdipCreateBitmapFromStream(st, &bm);
    printf("eri bitmap from stream %d, bitmap %s\n", s, bm == NULL ? "NULL" : bm == (void *)1 ? "untouched" : "made");
    LARGE_INTEGER z = {0};
    st->lpVtbl->Seek(st, z, STREAM_SEEK_SET, NULL);
    bm = (void *)1;
    s = GdipLoadImageFromStream(st, &bm);
    printf("eri image from stream %d, image %s\n", s, bm == NULL ? "NULL" : bm == (void *)1 ? "untouched" : "made");
    st->lpVtbl->Release(st);
    return 0;
}
