/* msacm32 as games decode their voices and sound effects with it: IMA ADPCM and Microsoft ADPCM
 * to 16-bit PCM through Windows' own codecs (imaadp32.acm, msadp32.acm), mono and stereo,
 * several block sizes, on pseudo-random block bytes (any bytes are a valid ADPCM block, so this
 * covers clamping, step-index limits and odd predictors), with partial input blocks; plus the
 * answers of acmFormatSuggest, acmStreamSize and acmStreamOpen for PCM <-> PCM conversions.
 * Decoded samples are compared exactly (a hash and the first samples of each conversion).
 * libs: -lmsacm32 -lwinmm */
#include "../probe.h"
#include <mmreg.h>
#include <msacm.h>

static unsigned rng = 12345;
static unsigned next(void) { rng = rng * 1103515245 + 12345; return rng >> 8; }

static void fmt(WAVEFORMATEX *f, WORD tag, WORD ch, DWORD rate, WORD bits, WORD align) {
    f->wFormatTag = tag; f->nChannels = ch; f->nSamplesPerSec = rate; f->wBitsPerSample = bits;
    f->nBlockAlign = align; f->nAvgBytesPerSec = rate * align; f->cbSize = 0;
}

/* ADPCM source formats with their extra bytes */
static union { WAVEFORMATEX w; IMAADPCMWAVEFORMAT ima; BYTE ms[sizeof(ADPCMWAVEFORMAT) + 7 * sizeof(ADPCMCOEFSET)]; } srcf;

static void ima_fmt(WORD ch, WORD align) {
    memset(&srcf, 0, sizeof srcf);
    fmt(&srcf.w, WAVE_FORMAT_IMA_ADPCM, ch, 22050, 4, align);
    srcf.w.cbSize = 2;
    srcf.ima.wSamplesPerBlock = (align - 4 * ch) * 8 / (4 * ch) + 1;
    srcf.w.nAvgBytesPerSec = 22050 * align / srcf.ima.wSamplesPerBlock;
}

static void ms_fmt(WORD ch, WORD align) {
    static const short c1[7] = { 256, 512, 0, 192, 240, 460, 392 }, c2[7] = { 0, -256, 0, 64, 0, -208, -232 };
    memset(&srcf, 0, sizeof srcf);
    ADPCMWAVEFORMAT *a = (ADPCMWAVEFORMAT *)srcf.ms;
    fmt(&a->wfx, WAVE_FORMAT_ADPCM, ch, 22050, 4, align);
    a->wfx.cbSize = 4 + 7 * 4;
    a->wSamplesPerBlock = (align - 7 * ch) * 8 / (4 * ch) + 2;
    a->wNumCoef = 7;
    for (int i = 0; i < 7; i++) { a->aCoef[i].iCoef1 = c1[i]; a->aCoef[i].iCoef2 = c2[i]; }
    a->wfx.nAvgBytesPerSec = 22050 * align / a->wSamplesPerBlock;
}

static void decode(const char *name, int blocks, int extra) {
    WAVEFORMATEX *s = &srcf.w, d;
    MMRESULT r = acmFormatSuggest(NULL, s, &d, sizeof d, ACM_FORMATSUGGESTF_WFORMATTAG | 0);
    d.wFormatTag = WAVE_FORMAT_PCM;
    r = acmFormatSuggest(NULL, s, &d, sizeof d, ACM_FORMATSUGGESTF_WFORMATTAG);
    printf("%s ch %u align %u: suggest %u -> tag %u ch %u rate %lu bits %u align %u avg %lu\n", name, s->nChannels, s->nBlockAlign, r, d.wFormatTag, d.nChannels, d.nSamplesPerSec, d.wBitsPerSample, d.nBlockAlign, d.nAvgBytesPerSec);
    HACMSTREAM h;
    r = acmStreamOpen(&h, NULL, s, &d, NULL, 0, 0, 0);
    if (r) { printf("  open %u\n", r); return; }
    DWORD slen = blocks * s->nBlockAlign + extra, dsize = 0, ssize = 0;
    printf("  size %u", acmStreamSize(h, slen, &dsize, ACM_STREAMSIZEF_SOURCE));
    printf(" %lu; back %u", dsize, acmStreamSize(h, dsize, &ssize, ACM_STREAMSIZEF_DESTINATION));
    printf(" %lu; one byte %u", ssize, acmStreamSize(h, 1, &ssize, ACM_STREAMSIZEF_SOURCE));
    printf(" %lu\n", ssize);
    BYTE *src = malloc(slen), *dst = malloc(dsize + 64);
    for (DWORD i = 0; i < slen; i++) src[i] = next();
    /* block headers: keep step indices in range on some blocks, out of range on others */
    for (int b = 0; b < blocks; b++)
        if (b % 3) for (int c = 0; c < s->nChannels; c++) {
            BYTE *hb = src + b * s->nBlockAlign + (s->wFormatTag == WAVE_FORMAT_IMA_ADPCM ? 4 * c + 2 : c);
            *hb %= s->wFormatTag == WAVE_FORMAT_IMA_ADPCM ? 89 : 7;
        }
    memset(dst, 0xcd, dsize + 64);
    ACMSTREAMHEADER hd = { sizeof hd };
    hd.pbSrc = src; hd.cbSrcLength = slen; hd.pbDst = dst; hd.cbDstLength = dsize + 64;
    printf("  prepare %u", acmStreamPrepareHeader(h, &hd, 0));
    r = acmStreamConvert(h, &hd, ACM_STREAMCONVERTF_BLOCKALIGN);
    printf(" convert %u used %lu -> %lu status %lx", r, hd.cbSrcLengthUsed, hd.cbDstLengthUsed, hd.fdwStatus & ~ACMSTREAMHEADER_STATUSF_PREPARED);
    unsigned hash = 2166136261u;
    for (DWORD i = 0; i < hd.cbDstLengthUsed; i++) hash = (hash ^ dst[i]) * 16777619u;
    printf(" hash %08x:", hash);
    for (int i = 0; i < 24 && i * 2 < (int)hd.cbDstLengthUsed; i++) printf(" %d", ((short *)dst)[i]);
    printf("\n");
    /* the remainder, without BLOCKALIGN */
    if (extra) {
        hd.pbSrc = src + hd.cbSrcLengthUsed; hd.cbSrcLength = slen - hd.cbSrcLengthUsed;
        r = acmStreamConvert(h, &hd, 0);
        printf("  rest %u used %lu -> %lu\n", r, hd.cbSrcLengthUsed, hd.cbDstLengthUsed);
        hd.pbSrc = src; hd.cbSrcLength = slen;
    }
    acmStreamUnprepareHeader(h, &hd, 0);
    acmStreamClose(h, 0);
    free(src); free(dst);
}

static void pcm(WORD ch1, DWORD rate1, WORD bits1, WORD ch2, DWORD rate2, WORD bits2) {
    WAVEFORMATEX s, d;
    fmt(&s, WAVE_FORMAT_PCM, ch1, rate1, bits1, ch1 * bits1 / 8);
    fmt(&d, WAVE_FORMAT_PCM, ch2, rate2, bits2, ch2 * bits2 / 8);
    HACMSTREAM h;
    MMRESULT r = acmStreamOpen(&h, NULL, &s, &d, NULL, 0, 0, ACM_STREAMOPENF_NONREALTIME);
    printf("pcm %u/%lu/%u -> %u/%lu/%u: open %u", ch1, rate1, bits1, ch2, rate2, bits2, r);
    if (r) { printf("\n"); return; }
    DWORD n = 64 * s.nBlockAlign, out = 0;
    printf(" size %u", acmStreamSize(h, n, &out, ACM_STREAMSIZEF_SOURCE));
    printf(" %lu", out);
    BYTE src[1024], dst[4096];
    for (DWORD i = 0; i < n; i++) src[i] = next();
    ACMSTREAMHEADER hd = { sizeof hd };
    hd.pbSrc = src; hd.cbSrcLength = n; hd.pbDst = dst; hd.cbDstLength = sizeof dst;
    acmStreamPrepareHeader(h, &hd, 0);
    r = acmStreamConvert(h, &hd, ACM_STREAMCONVERTF_BLOCKALIGN | ACM_STREAMCONVERTF_START | ACM_STREAMCONVERTF_END);
    unsigned hash = 2166136261u;
    for (DWORD i = 0; i < hd.cbDstLengthUsed; i++) hash = (hash ^ dst[i]) * 16777619u;
    printf(" convert %u used %lu -> %lu hash %08x:", r, hd.cbSrcLengthUsed, hd.cbDstLengthUsed, hash);
    for (int i = 0; i < 12 && i < (int)hd.cbDstLengthUsed; i++) printf(" %02x", dst[i]);
    printf("\n");
    acmStreamUnprepareHeader(h, &hd, 0);
    acmStreamClose(h, 0);
}

int main(void) {
    printf("version %lx\n", acmGetVersion() >> 16);
    WORD aligns[] = { 256, 512, 1024, 36 };
    for (int ch = 1; ch <= 2; ch++)
        for (int i = 0; i < 4; i++) {
            ima_fmt(ch, aligns[i] * ch);
            decode("ima", 5, i == 1 ? 17 : 0);
        }
    for (int ch = 1; ch <= 2; ch++)
        for (int i = 0; i < 4; i++) {
            ms_fmt(ch, aligns[i] * ch);
            decode("msadpcm", 5, i == 1 ? 17 : 0);
        }
    /* a nonstandard samples-per-block field */
    ima_fmt(1, 256);
    srcf.ima.wSamplesPerBlock = 100;
    decode("ima-spb100", 3, 0);
    /* PCM conversions games ask for */
    pcm(1, 22050, 16, 1, 22050, 16);
    pcm(1, 22050, 8, 1, 22050, 16);
    pcm(1, 22050, 16, 1, 22050, 8);
    pcm(2, 44100, 16, 1, 44100, 16);
    pcm(1, 44100, 16, 2, 44100, 16);
    pcm(1, 22050, 16, 1, 44100, 16);
    pcm(2, 44100, 16, 2, 22050, 16);
    pcm(1, 11025, 8, 2, 44100, 16);
    pcm(2, 48000, 16, 2, 44100, 16);
    return 0;
}
