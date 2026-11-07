// winmm waveOut as games use it, on x86 and x64 (WAVEHDR's flags move from 16 to 24 there):
// format queries (PCM, float, extensible 5.1, G.711 through the mapper, bad ones), device ids
// and handles, the header flag rules, playing two headers to their WOM_DONE events, the
// position (TIME_MS answers in bytes), waveOutGetID / waveOutMessage. What Windows answers (Wine's
// winmm wave tests); the CI machines have no audio device, so this is not a winref probe.
// libs: -lwinmm
#include <windows.h>
#include <mmsystem.h>
#include <mmreg.h>
#include <ks.h>
#include <ksmedia.h>
#include <stdio.h>

#ifndef DRVM_MAPPER_PREFERRED_GET
#define DRVM_MAPPER_PREFERRED_GET (0x2000 + 21)
#endif

static const GUID float_sub = {3, 0, 0x10, {0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71}};

static WAVEFORMATEX pcm(WORD tag, WORD ch, WORD bits, DWORD rate) {
    WAVEFORMATEX f = {tag, ch, rate, 0, (WORD)(ch * bits / 8), bits, 0};
    f.nAvgBytesPerSec = f.nBlockAlign * rate;
    return f;
}

static void query(const char *what, const WAVEFORMATEX *f, DWORD flags) {
    HWAVEOUT h = (HWAVEOUT)(UINT_PTR)0xdead;
    MMRESULT r = waveOutOpen(&h, WAVE_MAPPER, f, 0, 0, CALLBACK_NULL | WAVE_FORMAT_QUERY | flags);
    printf("query %s%s: %u handle untouched %d\n", what, flags & WAVE_FORMAT_DIRECT ? " direct" : "", r, h == (HWAVEOUT)(UINT_PTR)0xdead);
}

int main(void) {
    printf("devices %u\n", waveOutGetNumDevs());
    WAVEFORMATEX f = pcm(WAVE_FORMAT_PCM, 2, 16, 44100);
    query("pcm16", &f, 0);
    WAVEFORMATEX z = pcm(WAVE_FORMAT_PCM, 0, 16, 44100);
    query("no channels", &z, 0);
    WAVEFORMATEX three = pcm(WAVE_FORMAT_PCM, 3, 16, 44100);
    query("3 channels", &three, 0);
    WAVEFORMATEX odd = pcm(WAVE_FORMAT_PCM, 1, 40, 44100);
    query("40-bit", &odd, 0);
    query("40-bit", &odd, WAVE_FORMAT_DIRECT);
    WAVEFORMATEX fl = pcm(WAVE_FORMAT_IEEE_FLOAT, 2, 32, 48000);
    query("float", &fl, 0);
    WAVEFORMATEX alaw = pcm(WAVE_FORMAT_ALAW, 1, 8, 8000);
    query("alaw", &alaw, 0);
    query("alaw", &alaw, WAVE_FORMAT_DIRECT);
    WAVEFORMATEXTENSIBLE x = {{WAVE_FORMAT_EXTENSIBLE, 6, 48000, 6 * 4 * 48000, 6 * 4, 32, 22}};
    x.Samples.wValidBitsPerSample = 32;
    x.dwChannelMask = KSAUDIO_SPEAKER_5POINT1;
    x.SubFormat = float_sub;
    query("float 5.1", &x.Format, 0);
    x.Format.nAvgBytesPerSec = 0;
    query("float 5.1 no rate", &x.Format, 0);
    query("float 5.1 no rate", &x.Format, WAVE_FORMAT_DIRECT);

    HWAVEOUT h = (HWAVEOUT)(UINT_PTR)0xdead;
    MMRESULT r = waveOutOpen(&h, WAVE_MAPPER, &z, 0, 0, CALLBACK_NULL);
    printf("open bad format %u handle %d\n", r, h == NULL);
    printf("open no format %u\n", waveOutOpen(&h, WAVE_MAPPER, NULL, 0, 0, CALLBACK_NULL));
    printf("open device 2 %u\n", waveOutOpen(&h, 2, &f, 0, 0, CALLBACK_NULL));
    HANDLE ev = CreateEventA(NULL, FALSE, FALSE, NULL);
    r = waveOutOpen(&h, WAVE_MAPPER, &f, (DWORD_PTR)ev, 0, CALLBACK_EVENT);
    printf("open %u open event %lu\n", r, WaitForSingleObject(ev, 2000));
    if (r) return 0;
    UINT id = 7;
    r = waveOutGetID(h, &id);
    printf("getid %u id %d\n", r, (int)id);
    printf("open on a handle %u\n", waveOutOpen(NULL, (UINT)(UINT_PTR)h, &f, 0, 0, CALLBACK_NULL));
    WAVEOUTCAPSA caps;
    printf("caps of handle %u null caps %u device 2 %u\n", waveOutGetDevCapsA((UINT_PTR)h, &caps, sizeof caps),
           waveOutGetDevCapsA(0, NULL, sizeof caps), waveOutGetDevCapsA(2, &caps, sizeof caps));
    printf("volume null %u\n", waveOutGetVolume(h, NULL));
    DWORD pref = 9, status = 9;
    r = waveOutMessage((HWAVEOUT)(UINT_PTR)WAVE_MAPPER, DRVM_MAPPER_PREFERRED_GET, (DWORD_PTR)&pref, (DWORD_PTR)&status);
    printf("preferred %u %lu %lu, no status %u\n", r, pref, status,
           waveOutMessage((HWAVEOUT)(UINT_PTR)WAVE_MAPPER, DRVM_MAPPER_PREFERRED_GET, (DWORD_PTR)&pref, 0));

    // two headers of 1/20 s; the flags at their x86 / x64 place
    static short data[2][4410];
    WAVEHDR hd[2] = {0};
    for (int i = 0; i < 2; i++) {
        hd[i].lpData = (char *)data[i];
        hd[i].dwBufferLength = sizeof data[i];
    }
    printf("write unprepared %u\n", waveOutWrite(h, &hd[0], sizeof hd[0]));
    hd[0].dwFlags = WHDR_DONE | WHDR_INQUEUE;
    r = waveOutPrepareHeader(h, &hd[0], sizeof hd[0]);
    printf("prepare %u flags %lx\n", r, hd[0].dwFlags);
    hd[0].dwFlags |= WHDR_INQUEUE;
    r = waveOutPrepareHeader(h, &hd[0], sizeof hd[0]);
    printf("prepare again %u flags %lx\n", r, hd[0].dwFlags);
    printf("unprepare queued %u\n", waveOutUnprepareHeader(h, &hd[0], sizeof hd[0]));
    hd[0].dwFlags &= ~WHDR_INQUEUE;
    waveOutPrepareHeader(h, &hd[1], sizeof hd[1]);
    r = waveOutWrite(h, &hd[0], sizeof hd[0]);
    printf("write %u flags %lx\n", r, hd[0].dwFlags);
    printf("write again %u\n", waveOutWrite(h, &hd[0], sizeof hd[0]));
    waveOutWrite(h, &hd[1], sizeof hd[1]);
    int done = 0;
    for (int i = 0; i < 40 && done < 2; i++) {
        WaitForSingleObject(ev, 100);
        done = !!(hd[0].dwFlags & WHDR_DONE) + !!(hd[1].dwFlags & WHDR_DONE);
    }
    printf("done %d flags %lx %lx\n", done, hd[0].dwFlags, hd[1].dwFlags);
    MMTIME t = {TIME_BYTES};
    printf("position short %u\n", waveOutGetPosition(h, &t, sizeof t - 1));
    t.wType = TIME_MS;
    r = waveOutGetPosition(h, &t, sizeof t);
    printf("position %u type %u bytes %lu\n", r, t.wType, t.u.cb);
    t.wType = TIME_SAMPLES;
    r = waveOutGetPosition(h, &t, sizeof t);
    printf("position %u type %u samples %lu\n", r, t.wType, t.u.sample);
    for (int i = 0; i < 2; i++) {
        r = waveOutUnprepareHeader(h, &hd[i], sizeof hd[i]);
        printf("unprepare %u flags %lx\n", r, hd[i].dwFlags);
    }
    printf("close %u\n", waveOutClose(h));
    char text[200];
    r = waveOutGetErrorTextA(WAVERR_STILLPLAYING, text, sizeof text);
    printf("error text %u [%s]\n", r, text);
    return 0;
}
