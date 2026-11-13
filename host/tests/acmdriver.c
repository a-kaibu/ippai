/* libs: -lmsacm32 -lwinmm */
/* A driver the program adds itself (acmDriverAdd with ACM_DRIVERADDF_FUNCTION), as games add the
 * Vorbis.acm they ship (交姦条件 体験版: LoadLibrary, GetProcAddress "DriverProc", acmDriverAdd,
 * acmDriverOpen, then acmStreamOpen of its Vorbis WAVs). The driver here converts its own format
 * (tag 0x7777: 8-bit mono bytes XOR 0x5A) to 8-bit PCM. Only the API's answers are printed: which
 * messages Windows sends a driver, and when, is its own business. */
#include <windows.h>
#include <mmreg.h>
#include <msacm.h>
/* (msacmdrv.h, which mingw-w64 does not have) */
#define ACMDM_USER (DRV_USER + 0x0000)
#define ACMDM_DRIVER_DETAILS 0x600a
#define ACMDM_FORMATTAG_DETAILS 0x6019
#define ACMDM_FORMAT_DETAILS 0x601a
#define ACMDM_FORMAT_SUGGEST 0x601b
#define ACMDM_STREAM_OPEN 0x604c
#define ACMDM_STREAM_CLOSE 0x604d
#define ACMDM_STREAM_SIZE 0x604e
#define ACMDM_STREAM_CONVERT 0x604f
#define ACMDM_STREAM_RESET 0x6050
#define ACMDM_STREAM_PREPARE 0x6051
#define ACMDM_STREAM_UNPREPARE 0x6052
#include <pshpack1.h> /* as msacmdrv.h */
typedef struct {
    DWORD cbStruct; LPWAVEFORMATEX pwfxSrc; LPWAVEFORMATEX pwfxDst; LPWAVEFILTER pwfltr;
    DWORD_PTR dwCallback; DWORD_PTR dwInstance; DWORD fdwOpen; DWORD fdwDriver; DWORD_PTR dwDriver; HACMSTREAM has;
} ACMDRVSTREAMINSTANCE;
typedef struct ACMDRVSTREAMHEADER {
    DWORD cbStruct; DWORD fdwStatus; DWORD_PTR dwUser; LPBYTE pbSrc; DWORD cbSrcLength; DWORD cbSrcLengthUsed;
    DWORD_PTR dwSrcUser; LPBYTE pbDst; DWORD cbDstLength; DWORD cbDstLengthUsed; DWORD_PTR dwDstUser;
    DWORD fdwConvert; struct ACMDRVSTREAMHEADER *padshNext; DWORD fdwDriver; DWORD_PTR dwDriver;
    DWORD fdwPrepared; DWORD_PTR dwPrepared; LPBYTE pbPreparedSrc; DWORD cbPreparedSrcLength; LPBYTE pbPreparedDst; DWORD cbPreparedDstLength;
} ACMDRVSTREAMHEADER;
typedef struct { DWORD cbStruct; DWORD fdwSize; DWORD cbSrcLength; DWORD cbDstLength; } ACMDRVSTREAMSIZE;
typedef struct { DWORD cbStruct; DWORD fdwSuggest; LPWAVEFORMATEX pwfxSrc; DWORD cbwfxSrc; LPWAVEFORMATEX pwfxDst; DWORD cbwfxDst; } ACMDRVFORMATSUGGEST;
#include <poppack.h>
#include <stdio.h>
#include <string.h>

#define TAG 0x7777
static int opened, streams, prepared, converts;

static void fmt(WAVEFORMATEX *w, WORD tag) {
    memset(w, 0, sizeof *w);
    w->wFormatTag = tag; w->nChannels = 1; w->nSamplesPerSec = 8000; w->nAvgBytesPerSec = 8000;
    w->nBlockAlign = 1; w->wBitsPerSample = 8; w->cbSize = 0;
}
static int ours(const WAVEFORMATEX *w, WORD tag) {
    return w->wFormatTag == tag && w->nChannels == 1 && w->wBitsPerSample == 8 && w->nBlockAlign == 1 && w->nSamplesPerSec;
}

static LRESULT tag_details(ACMFORMATTAGDETAILSW *d, DWORD flags) {
    DWORD i;
    switch (flags & ACM_FORMATTAGDETAILSF_QUERYMASK) {
    case ACM_FORMATTAGDETAILSF_INDEX: i = d->dwFormatTagIndex; break;
    case ACM_FORMATTAGDETAILSF_FORMATTAG: i = d->dwFormatTag == WAVE_FORMAT_PCM ? 0 : d->dwFormatTag == TAG ? 1 : 2; break;
    case ACM_FORMATTAGDETAILSF_LARGESTSIZE: i = 0; break;
    default: return MMSYSERR_NOTSUPPORTED;
    }
    if (i > 1) return ACMERR_NOTPOSSIBLE;
    d->dwFormatTagIndex = i;
    d->dwFormatTag = i ? TAG : WAVE_FORMAT_PCM;
    d->cbFormatSize = i ? sizeof(WAVEFORMATEX) : sizeof(PCMWAVEFORMAT);
    d->fdwSupport = ACMDRIVERDETAILS_SUPPORTF_CODEC;
    d->cStandardFormats = 1;
    lstrcpyW(d->szFormatTag, i ? L"XOR 8" : L"PCM");
    return MMSYSERR_NOERROR;
}

static LRESULT format_details(ACMFORMATDETAILSW *d, DWORD flags) {
    WORD tag = (WORD)d->dwFormatTag;
    if (tag != WAVE_FORMAT_PCM && tag != TAG) return ACMERR_NOTPOSSIBLE;
    if ((flags & ACM_FORMATDETAILSF_QUERYMASK) == ACM_FORMATDETAILSF_INDEX) {
        if (d->dwFormatIndex != 0 || d->cbwfx < sizeof(PCMWAVEFORMAT)) return ACMERR_NOTPOSSIBLE;
        fmt(d->pwfx, tag);
    } else if ((flags & ACM_FORMATDETAILSF_QUERYMASK) == ACM_FORMATDETAILSF_FORMAT) {
        if (!ours(d->pwfx, tag)) return ACMERR_NOTPOSSIBLE;
    } else return MMSYSERR_NOTSUPPORTED;
    d->fdwSupport = ACMDRIVERDETAILS_SUPPORTF_CODEC;
    lstrcpyW(d->szFormat, L"8 kHz mono");
    return MMSYSERR_NOERROR;
}

LRESULT CALLBACK DriverProc(DWORD_PTR id, HDRVR drv, UINT msg, LPARAM l1, LPARAM l2) {
    switch (msg) {
    case DRV_LOAD: case DRV_ENABLE: case DRV_DISABLE: case DRV_FREE: return 1;
    case DRV_OPEN: opened++; return 0x1234;
    case DRV_CLOSE: opened--; return 1;
    case DRV_QUERYCONFIGURE: return 0;
    case ACMDM_DRIVER_DETAILS: {
        ACMDRIVERDETAILSW *d = (ACMDRIVERDETAILSW *)l1, x;
        memset(&x, 0, sizeof x);
        x.cbStruct = d->cbStruct < sizeof x ? d->cbStruct : sizeof x;
        x.fccType = ACMDRIVERDETAILS_FCCTYPE_AUDIOCODEC;
        x.wMid = 0xff; x.wPid = 1; x.vdwACM = 0x03320000; x.vdwDriver = 0x01000000;
        x.fdwSupport = ACMDRIVERDETAILS_SUPPORTF_CODEC; x.cFormatTags = 2; x.cFilterTags = 0;
        lstrcpyW(x.szShortName, L"XOR8"); lstrcpyW(x.szLongName, L"XOR 8 test codec");
        memcpy(d, &x, x.cbStruct);
        return MMSYSERR_NOERROR;
    }
    case ACMDM_FORMATTAG_DETAILS: return tag_details((ACMFORMATTAGDETAILSW *)l1, (DWORD)l2);
    case ACMDM_FORMAT_DETAILS: return format_details((ACMFORMATDETAILSW *)l1, (DWORD)l2);
    case ACMDM_FORMAT_SUGGEST: {
        ACMDRVFORMATSUGGEST *s = (ACMDRVFORMATSUGGEST *)l1;
        if (!ours(s->pwfxSrc, TAG) || s->cbwfxDst < sizeof(PCMWAVEFORMAT)) return ACMERR_NOTPOSSIBLE;
        fmt(s->pwfxDst, WAVE_FORMAT_PCM);
        s->pwfxDst->nSamplesPerSec = s->pwfxSrc->nSamplesPerSec;
        s->pwfxDst->nAvgBytesPerSec = s->pwfxSrc->nSamplesPerSec;
        return MMSYSERR_NOERROR;
    }
    case ACMDM_STREAM_OPEN: {
        ACMDRVSTREAMINSTANCE *si = (ACMDRVSTREAMINSTANCE *)l1;
        if (!ours(si->pwfxSrc, TAG) || !ours(si->pwfxDst, WAVE_FORMAT_PCM) || si->pwfxSrc->nSamplesPerSec != si->pwfxDst->nSamplesPerSec) return ACMERR_NOTPOSSIBLE;
        if (si->fdwOpen & ACM_STREAMOPENF_QUERY) return MMSYSERR_NOERROR;
        si->dwDriver = 0x5a;
        streams++;
        return MMSYSERR_NOERROR;
    }
    case ACMDM_STREAM_CLOSE: streams--; return MMSYSERR_NOERROR;
    case ACMDM_STREAM_SIZE: {
        ACMDRVSTREAMSIZE *z = (ACMDRVSTREAMSIZE *)l2;
        if ((z->fdwSize & ACM_STREAMSIZEF_QUERYMASK) == ACM_STREAMSIZEF_SOURCE) z->cbDstLength = z->cbSrcLength;
        else z->cbSrcLength = z->cbDstLength;
        return MMSYSERR_NOERROR;
    }
    case ACMDM_STREAM_PREPARE: prepared++; return MMSYSERR_NOTSUPPORTED;
    case ACMDM_STREAM_UNPREPARE: prepared--; return MMSYSERR_NOTSUPPORTED;
    case ACMDM_STREAM_CONVERT: {
        ACMDRVSTREAMINSTANCE *si = (ACMDRVSTREAMINSTANCE *)l1;
        ACMDRVSTREAMHEADER *h = (ACMDRVSTREAMHEADER *)l2;
        DWORD n = h->cbSrcLength < h->cbDstLength ? h->cbSrcLength : h->cbDstLength;
        for (DWORD i = 0; i < n; i++) h->pbDst[i] = h->pbSrc[i] ^ (BYTE)si->dwDriver;
        h->cbSrcLengthUsed = n; h->cbDstLengthUsed = n;
        converts++;
        return MMSYSERR_NOERROR;
    }
    }
    return msg >= ACMDM_USER ? MMSYSERR_NOTSUPPORTED : 0;
}

int main(void) {
    HACMDRIVERID id = NULL;
    HACMDRIVER had = NULL;
    MMRESULT r = acmDriverAddW(&id, GetModuleHandleW(NULL), (LPARAM)DriverProc, 0, ACM_DRIVERADDF_FUNCTION | ACM_DRIVERADDF_LOCAL);
    printf("add: %u %s\n", r, id ? "id" : "no id");
    printf("add, no proc: %u\n", acmDriverAddW(&(HACMDRIVERID){ 0 }, GetModuleHandleW(NULL), 0, 0, ACM_DRIVERADDF_FUNCTION));
    r = acmDriverOpen(&had, id, 0);
    printf("open: %u %s, driver opened %d\n", r, had ? "had" : "none", opened > 0);
    ACMDRIVERDETAILSW dd = { sizeof dd };
    r = acmDriverDetailsW(id, &dd, 0);
    printf("details W: %u \"%ls\" \"%ls\" tags %lu\n", r, dd.szShortName, dd.szLongName, dd.cFormatTags);
    ACMDRIVERDETAILSA da = { sizeof da };
    r = acmDriverDetailsA(id, &da, 0);
    printf("details A: %u \"%s\" tags %lu\n", r, da.szShortName, da.cFormatTags);
    HACMDRIVERID back = NULL;
    r = acmDriverID((HACMOBJ)had, &back, 0);
    printf("id of had: %u %s\n", r, back == id ? "same" : "other");

    WAVEFORMATEX src, dst, sug;
    fmt(&src, TAG);
    fmt(&dst, WAVE_FORMAT_PCM);
    memset(&sug, 0xcc, sizeof sug);
    r = acmFormatSuggest(had, &src, &sug, sizeof sug, 0);
    printf("suggest: %u tag %u ch %u rate %lu bits %u\n", r, sug.wFormatTag, sug.nChannels, sug.nSamplesPerSec, sug.wBitsPerSample);
    printf("query: %u\n", acmStreamOpen(NULL, had, &src, &dst, NULL, 0, 0, ACM_STREAMOPENF_QUERY));
    WAVEFORMATEX bad = dst;
    bad.nSamplesPerSec = 11025;
    printf("query other rate: %u\n", acmStreamOpen(NULL, had, &src, &bad, NULL, 0, 0, ACM_STREAMOPENF_QUERY));

    for (int pass = 0; pass < 2; pass++) {
        HACMSTREAM s = NULL;
        /* the driver opened, then no driver named (any driver that can) */
        r = acmStreamOpen(&s, pass ? NULL : had, &src, &dst, NULL, 0, 0, 0);
        printf("stream %s: %u, streams %d\n", pass ? "any driver" : "on had", r, streams);
        if (r) continue;
        DWORD n = 0;
        r = acmStreamSize(s, 6, &n, ACM_STREAMSIZEF_SOURCE);
        printf("  size: %u %lu\n", r, n);
        BYTE in[6] = { 0x5a, 0x5b, 0x00, 0xff, 0x1a, 0x7a }, out[8];
        memset(out, 0xee, sizeof out);
        ACMSTREAMHEADER h = { sizeof h };
        h.pbSrc = in; h.cbSrcLength = sizeof in; h.pbDst = out; h.cbDstLength = sizeof out;
        printf("  convert unprepared: %u\n", acmStreamConvert(s, &h, 0));
        r = acmStreamPrepareHeader(s, &h, 0);
        printf("  prepare: %u prepared %d\n", r, !!(h.fdwStatus & ACMSTREAMHEADER_STATUSF_PREPARED));
        r = acmStreamConvert(s, &h, ACM_STREAMCONVERTF_BLOCKALIGN);
        printf("  convert: %u done %d used %lu -> %lu:", r, !!(h.fdwStatus & ACMSTREAMHEADER_STATUSF_DONE), h.cbSrcLengthUsed, h.cbDstLengthUsed);
        for (int i = 0; i < 8; i++) printf(" %02x", out[i]);
        printf("\n");
        h.cbSrcLength = 3;
        r = acmStreamConvert(s, &h, 0);
        printf("  convert 3: %u used %lu -> %lu\n", r, h.cbSrcLengthUsed, h.cbDstLengthUsed);
        r = acmStreamUnprepareHeader(s, &h, 0);
        printf("  unprepare: %u prepared %d\n", r, !!(h.fdwStatus & ACMSTREAMHEADER_STATUSF_PREPARED));
        printf("  reset: %u\n", acmStreamReset(s, 0));
        r = acmStreamClose(s, 0);
        printf("  close: %u, streams %d\n", r, streams);
    }
    printf("driver close: %u\n", acmDriverClose(had, 0));
    printf("remove: %u\n", acmDriverRemove(id, 0));
    HACMDRIVER h2 = NULL;
    printf("open removed: %s\n", acmDriverOpen(&h2, id, 0) == 0 ? "opened" : "refused");
    printf("converts %d\n", converts);
    return 0;
}
