/* msacm32's MP3 codec (Windows: l3codecp.acm; sake: the page's WebCodecs AudioDecoder) as games
 * decode MP3 in a WAVE with it: the format tag's details, acmFormatSuggest, which destination
 * formats acmStreamOpen takes, acmStreamSize both ways, and conversions in pieces of several
 * sizes (what each call takes and gives, and in total). The two decoders' samples are not
 * equal to the bit, so the sound is checked by what it is: per channel the tone's frequency
 * (zero crossings in the middle of it) and its loudness (RMS), coarsely, and where it starts.
 * run: browser (sake decodes MP3 only with a page)
 * libs: -lmsacm32 -lwinmm */
#include "../probe.h"
#include <math.h>
#include <mmreg.h>
#include <msacm.h>
#include "acmmp3_data.h"

static MPEGLAYER3WAVEFORMAT mp3fmt(WORD ch, DWORD rate, DWORD kbps, WORD block) {
    MPEGLAYER3WAVEFORMAT f;
    memset(&f, 0, sizeof f);
    f.wfx.wFormatTag = WAVE_FORMAT_MPEGLAYER3;
    f.wfx.nChannels = ch;
    f.wfx.nSamplesPerSec = rate;
    f.wfx.nAvgBytesPerSec = kbps * 1000 / 8;
    f.wfx.nBlockAlign = 1;
    f.wfx.wBitsPerSample = 0;
    f.wfx.cbSize = MPEGLAYER3_WFX_EXTRA_BYTES;
    f.wID = MPEGLAYER3_ID_MPEG;
    f.fdwFlags = MPEGLAYER3_FLAG_PADDING_OFF;
    f.nBlockSize = block;
    f.nFramesPerBlock = 1;
    f.nCodecDelay = 1393;
    return f;
}

static WAVEFORMATEX pcm(WORD ch, DWORD rate, WORD bits) {
    WAVEFORMATEX d = { WAVE_FORMAT_PCM, ch, rate, rate * ch * bits / 8, ch * bits / 8, bits, 0 };
    return d;
}

/* frequency (Hz, to 10), RMS (to 0.05 of full scale) and the first sample past 1% of full scale */
static void shape(const char *name, const short *s, int frames, int ch, DWORD rate) {
    for (int c = 0; c < ch; c++) {
        int first = -1, cross = 0, from = frames / 4, to = frames * 3 / 4;
        double sq = 0;
        for (int i = 0; i < frames; i++) {
            int v = s[i * ch + c];
            if (first < 0 && abs(v) > 327) first = i;
            if (i > from && i <= to) {
                int p = s[(i - 1) * ch + c];
                if ((p < 0) != (v < 0)) cross++;
                sq += (double)v * v;
            }
        }
        double hz = cross / 2.0 * rate / (to - from), rms = sqrt(sq / (to - from)) / 32768;
        printf("  %s ch %d: %ld Hz, rms %ld/20, starts in the first %s\n", name, c, lround(hz / 10) * 10, lround(rms * 20),
               first < 0 ? "never" : first < 1152 ? "frame" : first < 2304 ? "two frames" : "later");
    }
}

static void convert(const char *name, const unsigned char *mp3, DWORD len, WORD ch, DWORD rate, DWORD kbps, WORD block, DWORD piece) {
    MPEGLAYER3WAVEFORMAT s = mp3fmt(ch, rate, kbps, block);
    WAVEFORMATEX d = pcm(ch, rate, 16);
    HACMSTREAM h;
    MMRESULT r = acmStreamOpen(&h, NULL, &s.wfx, &d, NULL, 0, 0, 0);
    printf("%s, pieces of %lu: open %u\n", name, piece, r);
    if (r) return;
    DWORD out = 0;
    r = acmStreamSize(h, piece, &out, ACM_STREAMSIZEF_SOURCE);
    printf("  size %u %lu\n", r, out);
    if (r || !out) out = 65536;
    short *all = calloc(1, 4 << 20);
    BYTE *buf = malloc(out);
    DWORD total = 0, at = 0;
    int calls = 0;
    for (int k = 0; at < len || k < 50; k++) {
        DWORD n = at < len ? min(piece, len - at) : 0;
        ACMSTREAMHEADER hd;
        memset(&hd, 0, sizeof hd);
        hd.cbStruct = sizeof hd;
        hd.pbSrc = (BYTE *)mp3 + at;
        hd.cbSrcLength = n;
        hd.pbDst = buf;
        hd.cbDstLength = out;
        if ((r = acmStreamPrepareHeader(h, &hd, 0))) { printf("  prepare %u\n", r); break; }
        DWORD flags = ACM_STREAMCONVERTF_BLOCKALIGN | (k == 0 ? ACM_STREAMCONVERTF_START : 0) | (at + n >= len && n ? ACM_STREAMCONVERTF_END : 0);
        r = acmStreamConvert(h, &hd, flags);
        if (calls < 4 || r) printf("  call %d: %u took %lu gave %lu done %d\n", calls, r, hd.cbSrcLengthUsed, hd.cbDstLengthUsed, !!(hd.fdwStatus & ACMSTREAMHEADER_STATUSF_DONE));
        calls++;
        acmStreamUnprepareHeader(h, &hd, 0);
        if (r) break;
        if (total + hd.cbDstLengthUsed <= (4u << 20)) memcpy((BYTE *)all + total, buf, hd.cbDstLengthUsed);
        total += hd.cbDstLengthUsed;
        at += hd.cbSrcLengthUsed;
        if (n == 0 && hd.cbDstLengthUsed == 0) break;
        if (n && !hd.cbSrcLengthUsed && !hd.cbDstLengthUsed) break;
    }
    /* (the decoders keep different delays: the length to the frame) */
    printf("  took all %d, gave %lu frames of 1152\n", at == len, lround(total / (2.0 * ch) / 1152));
    shape(name, all, total / (2 * ch), ch, rate);
    acmStreamClose(h, 0);
    free(all);
    free(buf);
}

int main(void) {
    MPEGLAYER3WAVEFORMAT s = mp3fmt(2, 44100, 64, 208);
    ACMFORMATTAGDETAILSA t;
    memset(&t, 0, sizeof t);
    t.cbStruct = sizeof t;
    t.dwFormatTag = WAVE_FORMAT_MPEGLAYER3;
    MMRESULT r = acmFormatTagDetailsA(NULL, &t, ACM_FORMATTAGDETAILSF_FORMATTAG);
    if (r) {
        /* (a Windows without the codec, or sake with no page) */
        printf("skip no MP3 codec (%u)\n", r);
        return 0;
    }
    printf("tag details %u: tag %#lx size %lu\n", r, t.dwFormatTag, t.cbFormatSize);

    WAVEFORMATEX d;
    memset(&d, 0, sizeof d);
    d.wFormatTag = WAVE_FORMAT_PCM;
    r = acmFormatSuggest(NULL, &s.wfx, &d, sizeof d, ACM_FORMATSUGGESTF_WFORMATTAG);
    printf("suggest %u: tag %u ch %u rate %lu bits %u align %u avg %lu\n", r, d.wFormatTag, d.nChannels, d.nSamplesPerSec, d.wBitsPerSample, d.nBlockAlign, d.nAvgBytesPerSec);

    struct { const char *what; WAVEFORMATEX d; } outs[] = {
        { "16-bit stereo 44100", pcm(2, 44100, 16) },
        { "8-bit stereo 44100", pcm(2, 44100, 8) },
        { "16-bit mono 44100", pcm(1, 44100, 16) },
        { "16-bit stereo 22050", pcm(2, 22050, 16) },
    };
    for (int i = 0; i < 4; i++) {
        HACMSTREAM h;
        r = acmStreamOpen(&h, NULL, &s.wfx, &outs[i].d, NULL, 0, 0, ACM_STREAMOPENF_QUERY);
        printf("query to %s: %u\n", outs[i].what, r);
    }

    HACMSTREAM h;
    d = pcm(2, 44100, 16);
    r = acmStreamOpen(&h, NULL, &s.wfx, &d, NULL, 0, 0, 0);
    printf("open %u\n", r);
    if (!r) {
        DWORD ins[] = { 1, 208, 209, 4096, 100000 }, outs2[] = { 1, 4608, 7608, 20000 };
        for (int i = 0; i < 5; i++) {
            DWORD o = 0;
            r = acmStreamSize(h, ins[i], &o, ACM_STREAMSIZEF_SOURCE);
            printf("size of %lu source bytes: %u %lu\n", ins[i], r, o);
        }
        for (int i = 0; i < 4; i++) {
            DWORD o = 0;
            r = acmStreamSize(h, outs2[i], &o, ACM_STREAMSIZEF_DESTINATION);
            printf("source for %lu bytes: %u %lu\n", outs2[i], r, o);
        }
        acmStreamClose(h, 0);
    }

    DWORD pieces[] = { 4096, 208, 1000, sizeof mp3_s };
    for (int i = 0; i < 4; i++) convert("44.1 kHz stereo 64 kbps", mp3_s, sizeof mp3_s, 2, 44100, 64, 208, pieces[i]);
    convert("22.05 kHz mono 32 kbps", mp3_m, sizeof mp3_m, 1, 22050, 32, 104, 4096);
    return 0;
}
