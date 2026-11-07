// WASAPI shared-mode output as game audio engines (FMOD in Unity 5.x-2022, Wwise) drive it:
// the mix format, format negotiation, Initialize with an event, the render client's buffer
// rules, playback (the event fires, the padding drains, the clock moves and stops), Reset,
// the stream / session / channel volumes, the session manager and the endpoint volume.
// Prints what sake's one browser endpoint gives (a machine's own device differs: its mix
// format and periods), only what does not depend on timing.
// libs: -lole32 -luuid
#define COBJMACROS
#define INITGUID
#include <windows.h>
#include <initguid.h>
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <audiopolicy.h>
#include <endpointvolume.h>
#include <mmreg.h>
#include <ks.h>
#include <ksmedia.h>
#include <stdio.h>

DEFINE_GUID(IID_IAudioClockAdjustment_, 0xf6e4c0a0, 0x46d9, 0x4fb8, 0xbe, 0x21, 0x57, 0xa3, 0xef, 0x2b, 0x62, 0x6c);

static void fmt(const char *what, const WAVEFORMATEX *f) {
    printf("%s tag %04x ch %u rate %lu avg %lu align %u bits %u cb %u", what, f->wFormatTag, f->nChannels,
           f->nSamplesPerSec, f->nAvgBytesPerSec, f->nBlockAlign, f->wBitsPerSample, f->cbSize);
    if (f->wFormatTag == WAVE_FORMAT_EXTENSIBLE) {
        const WAVEFORMATEXTENSIBLE *e = (const void *)f;
        printf(" valid %u mask %lx sub %08lx", e->Samples.wValidBitsPerSample, e->dwChannelMask, e->SubFormat.Data1);
    }
    printf("\n");
}

/* an out pointer as the call left it (the same on x86 and x64) */
static const char *ptr(const void *p) {
    return !p ? "null" : p == (void *)1 ? "untouched" : "set";
}

static WAVEFORMATEX pcm(WORD ch, DWORD rate, WORD bits) {
    WAVEFORMATEX f = { WAVE_FORMAT_PCM, ch, rate, rate * ch * bits / 8, ch * bits / 8, bits, 0 };
    return f;
}

int main(void) {
    CoInitializeEx(NULL, COINIT_MULTITHREADED);
    IMMDeviceEnumerator *en = NULL;
    IMMDevice *dev = NULL;
    CoCreateInstance(&CLSID_MMDeviceEnumerator, NULL, CLSCTX_ALL, &IID_IMMDeviceEnumerator, (void **)&en);
    if (!en || FAILED(IMMDeviceEnumerator_GetDefaultAudioEndpoint(en, eRender, eConsole, &dev))) return 1;

    IAudioClient *ac = NULL;
    HRESULT hr = IMMDevice_Activate(dev, &IID_IAudioClient, CLSCTX_ALL, NULL, (void **)&ac);
    printf("activate %08lx %d\n", hr, ac != NULL);
    if (!ac) return 1;

    // before Initialize
    // (each call is its own statement: printf's arguments are evaluated in no set order)
    UINT32 n = 7;
    HRESULT h1, h2, h3, h4, h5, h6;
    h1 = IAudioClient_GetBufferSize(ac, &n);
    h2 = IAudioClient_GetCurrentPadding(ac, &n);
    h3 = IAudioClient_Start(ac);
    printf("uninit size %08lx padding %08lx start %08lx\n", h1, h2, h3);
    REFERENCE_TIME def = 0, min = 0;
    hr = IAudioClient_GetDevicePeriod(ac, &def, &min);
    printf("period %08lx def %lld min %lld\n", hr, def, min);
    WAVEFORMATEX *mix = NULL;
    hr = IAudioClient_GetMixFormat(ac, &mix);
    printf("mix %08lx\n", hr);
    fmt("mix", mix);

    // format negotiation (shared mode)
    WAVEFORMATEX *close = (void *)1;
    hr = IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, mix, &close);
    printf("supported mix %08lx close %s\n", hr, ptr(close));
    WAVEFORMATEX f = pcm(2, mix->nSamplesPerSec, 16);
    close = (void *)1;
    hr = IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, &f, &close);
    printf("supported pcm16 %08lx close %s\n", hr, ptr(close));
    f = pcm(2, 22050, 16);
    hr = IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, &f, &close);
    printf("supported 22050 %08lx close %d\n", hr, close != NULL);
    if (close) { fmt("closest", close); CoTaskMemFree(close); }
    f = pcm(6, mix->nSamplesPerSec, 16);
    printf("supported 6ch plain %08lx\n", IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, &f, &close));
    f = pcm(2, mix->nSamplesPerSec, 16);
    f.nBlockAlign = 3;
    printf("supported bad align %08lx\n", IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, &f, &close));
    f = pcm(2, mix->nSamplesPerSec, 16);
    f.wFormatTag = WAVE_FORMAT_ALAW;
    printf("supported alaw %08lx\n", IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, &f, &close));
    printf("supported null %08lx\n", IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_SHARED, NULL, &close));
    printf("supported exclusive %08lx\n", IAudioClient_IsFormatSupported(ac, AUDCLNT_SHAREMODE_EXCLUSIVE, mix, NULL));

    // Initialize: a rate the mix does not have is refused, then 16-bit PCM with an event
    f = pcm(2, 22050, 16);
    printf("init 22050 %08lx\n", IAudioClient_Initialize(ac, AUDCLNT_SHAREMODE_SHARED, 0, 2000000, 0, &f, NULL));
    printf("init bad flags %08lx\n", IAudioClient_Initialize(ac, AUDCLNT_SHAREMODE_SHARED, 0x1, 2000000, 0, mix, NULL));
    f = pcm(2, mix->nSamplesPerSec, 16);
    hr = IAudioClient_Initialize(ac, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 2000000, 0, &f, NULL);
    h1 = IAudioClient_Initialize(ac, AUDCLNT_SHAREMODE_SHARED, 0, 2000000, 0, &f, NULL);
    printf("init %08lx again %08lx\n", hr, h1);
    UINT32 size = 0, pad = 9;
    h1 = IAudioClient_GetBufferSize(ac, &size);
    h2 = IAudioClient_GetCurrentPadding(ac, &pad);
    printf("size %08lx %u padding %08lx %u\n", h1, size, h2, pad);
    REFERENCE_TIME lat = -1;
    hr = IAudioClient_GetStreamLatency(ac, &lat);
    printf("latency %08lx %d\n", hr, lat >= 0);
    HANDLE ev = CreateEventA(NULL, FALSE, FALSE, NULL);
    h1 = IAudioClient_SetEventHandle(ac, NULL);
    h2 = IAudioClient_SetEventHandle(ac, ev);
    h3 = IAudioClient_SetEventHandle(ac, ev);
    printf("event null %08lx set %08lx again %08lx\n", h1, h2, h3);

    // services
    IAudioRenderClient *rc = NULL;
    IAudioClock *clock = NULL;
    IAudioStreamVolume *sv = NULL;
    ISimpleAudioVolume *simple = NULL;
    IChannelAudioVolume *chan = NULL;
    IAudioSessionControl *sc = NULL, *sc2 = NULL;
    void *adj = (void *)1;
    h1 = IAudioClient_GetService(ac, &IID_IAudioRenderClient, (void **)&rc);
    h2 = IAudioClient_GetService(ac, &IID_IAudioClock, (void **)&clock);
    h3 = IAudioClient_GetService(ac, &IID_IAudioStreamVolume, (void **)&sv);
    h4 = IAudioClient_GetService(ac, &IID_ISimpleAudioVolume, (void **)&simple);
    h5 = IAudioClient_GetService(ac, &IID_IChannelAudioVolume, (void **)&chan);
    h6 = IAudioClient_GetService(ac, &IID_IAudioSessionControl, (void **)&sc);
    printf("render %08lx clock %08lx stream %08lx simple %08lx channel %08lx session %08lx\n", h1, h2, h3, h4, h5, h6);
    IAudioClient_GetService(ac, &IID_IAudioSessionControl, (void **)&sc2);
    h1 = IAudioClient_GetService(ac, &IID_IAudioCaptureClient, &adj);
    adj = (void *)1;
    h2 = IAudioClient_GetService(ac, &IID_IAudioClockAdjustment_, &adj);
    printf("session same %d capture %08lx adjust %08lx %s\n", sc == sc2, h1, h2, ptr(adj));
    if (!rc || !clock || !sv || !simple || !chan || !sc) return 1;

    // the render client's buffer
    BYTE *buf = (void *)1;
    hr = IAudioRenderClient_GetBuffer(rc, size + 1, &buf);
    printf("get too large %08lx %s\n", hr, ptr(buf));
    buf = (void *)1;
    hr = IAudioRenderClient_GetBuffer(rc, 0, &buf);
    printf("get 0 %08lx %s\n", hr, ptr(buf));
    hr = IAudioRenderClient_GetBuffer(rc, size / 2, &buf);
    int zero = 1;
    for (UINT32 i = 0; buf && i < size / 2 * 4; i++) zero &= buf[i] == 0;
    BYTE *buf2 = (void *)1;
    h1 = IAudioRenderClient_GetBuffer(rc, 1, &buf2);
    printf("get half %08lx zeroed %d while held %08lx %s\n", hr, zero, h1, ptr(buf2));
    printf("reset while held %08lx\n", IAudioClient_Reset(ac));
    for (UINT32 i = 0; i < size / 2; i++) {
        short s = (short)((i % 100 < 50) ? 8000 : -8000);
        ((short *)buf)[2 * i] = s;
        ((short *)buf)[2 * i + 1] = s;
    }
    h1 = IAudioRenderClient_ReleaseBuffer(rc, size / 2 + 1, 0);
    h2 = IAudioRenderClient_ReleaseBuffer(rc, size / 2, 0);
    h3 = IAudioRenderClient_ReleaseBuffer(rc, 1, 0);
    printf("release too many %08lx release %08lx again %08lx\n", h1, h2, h3);
    IAudioClient_GetCurrentPadding(ac, &pad);
    printf("padding after write %u\n", pad);

    // the clock: frequency and a position that stays while stopped
    UINT64 freq = 0, pos = 1, qpc = 0, pos2 = 2;
    hr = IAudioClock_GetFrequency(clock, &freq);
    printf("frequency %08lx %llu\n", hr, freq);
    IAudioClock_GetPosition(clock, &pos, &qpc);
    printf("position stopped %llu\n", pos);

    // play: the event fires each period and the written audio drains
    h1 = IAudioClient_Start(ac);
    h2 = IAudioClient_Start(ac);
    printf("start %08lx again %08lx\n", h1, h2);
    DWORD w = WaitForSingleObject(ev, 2000);
    int drained = 0;
    for (int i = 0; i < 400 && !drained; i++) {
        WaitForSingleObject(ev, 50);
        IAudioClient_GetCurrentPadding(ac, &pad);
        drained = pad == 0;
    }
    IAudioClock_GetPosition(clock, &pos, &qpc);
    printf("event %lu drained %d position moved %d qpc %d\n", w, drained, pos >= (UINT64)size / 2 * 4, qpc != 0);
    h1 = IAudioClient_Stop(ac);
    h2 = IAudioClient_Stop(ac);
    printf("stop %08lx again %08lx\n", h1, h2);
    IAudioClock_GetPosition(clock, &pos, &qpc);
    Sleep(100);
    IAudioClock_GetPosition(clock, &pos2, &qpc);
    printf("position held %d\n", pos == pos2);
    IAudioRenderClient_GetBuffer(rc, size / 4, &buf);
    IAudioRenderClient_ReleaseBuffer(rc, size / 4, AUDCLNT_BUFFERFLAGS_SILENT);
    IAudioClient_GetCurrentPadding(ac, &pad);
    hr = IAudioClient_Reset(ac);
    printf("padding stopped %u reset %08lx ", pad, hr);
    IAudioClient_GetCurrentPadding(ac, &pad);
    IAudioClock_GetPosition(clock, &pos, &qpc);
    printf("padding %u position %llu\n", pad, pos);

    // volumes
    UINT32 cc = 0;
    float v = -1, vs[2] = { 0 };
    hr = IAudioStreamVolume_GetChannelCount(sv, &cc);
    h1 = IAudioStreamVolume_SetChannelVolume(sv, 1, 0.5f);
    h2 = IAudioStreamVolume_SetChannelVolume(sv, 2, 0.5f);
    h3 = IAudioStreamVolume_SetChannelVolume(sv, 0, 1.5f);
    IAudioStreamVolume_GetAllVolumes(sv, 2, vs);
    printf("stream channels %08lx %u set %08lx bad %08lx range %08lx all %.3f %.3f\n", hr, cc, h1, h2, h3, vs[0], vs[1]);
    hr = ISimpleAudioVolume_GetMasterVolume(simple, &v);
    float v0 = v;
    h1 = ISimpleAudioVolume_SetMasterVolume(simple, 0.25f, NULL);
    h2 = ISimpleAudioVolume_SetMasterVolume(simple, -0.5f, NULL);
    ISimpleAudioVolume_GetMasterVolume(simple, &v);
    BOOL mute = 2;
    h3 = ISimpleAudioVolume_SetMute(simple, TRUE, NULL);
    ISimpleAudioVolume_GetMute(simple, &mute);
    printf("simple get %08lx %.3f set %08lx bad %08lx %.3f mute %08lx %d\n", hr, v0, h1, h2, v, h3, mute);
    ISimpleAudioVolume_SetMute(simple, FALSE, NULL);
    ISimpleAudioVolume_SetMasterVolume(simple, 1.0f, NULL);
    hr = IChannelAudioVolume_GetChannelCount(chan, &cc);
    printf("channel count %08lx %u\n", hr, cc);

    // the session
    AudioSessionState st = 9;
    hr = IAudioSessionControl_GetState(sc, &st);
    LPWSTR name = NULL;
    h1 = IAudioSessionControl_GetDisplayName(sc, &name);
    printf("session state %08lx %d name %08lx empty %d\n", hr, st, h1, name && !*name);
    CoTaskMemFree(name);
    IAudioSessionControl2 *s2 = NULL;
    DWORD pid = 0;
    hr = IAudioSessionControl_QueryInterface(sc, &IID_IAudioSessionControl2, (void **)&s2);
    printf("control2 %08lx", hr);
    if (s2) {
        h1 = IAudioSessionControl2_GetProcessId(s2, &pid);
        h2 = IAudioSessionControl2_IsSystemSoundsSession(s2);
        printf(" pid %08lx ours %d system %08lx", h1, pid == GetCurrentProcessId(), h2);
        IAudioSessionControl2_Release(s2);
    }
    printf("\n");

    IAudioSessionManager2 *sm = NULL;
    hr = IMMDevice_Activate(dev, &IID_IAudioSessionManager2, CLSCTX_ALL, NULL, (void **)&sm);
    printf("manager %08lx", hr);
    if (sm) {
        IAudioSessionEnumerator *se = NULL;
        int count = -1;
        h1 = IAudioSessionManager2_GetSessionEnumerator(sm, &se);
        if (se) {
            IAudioSessionEnumerator_GetCount(se, &count);
            IAudioSessionEnumerator_Release(se);
        }
        ISimpleAudioVolume *mv = NULL;
        h2 = IAudioSessionManager2_GetSimpleAudioVolume(sm, NULL, FALSE, &mv);
        printf(" enum %08lx count %d volume %08lx %d", h1, count, h2, mv != NULL);
        if (mv) ISimpleAudioVolume_Release(mv);
        IAudioSessionManager2_Release(sm);
    }
    printf("\n");

    IAudioEndpointVolume *epv = NULL;
    hr = IMMDevice_Activate(dev, &IID_IAudioEndpointVolume, CLSCTX_ALL, NULL, (void **)&epv);
    printf("endpoint volume %08lx ", hr);
    if (epv) {
        float lo = 0, hi = 0, step = 0, db = 1, sc_ = 0;
        UINT chs = 0;
        IAudioEndpointVolume_GetVolumeRange(epv, &lo, &hi, &step);
        IAudioEndpointVolume_GetChannelCount(epv, &chs);
        printf("range %.5f %.5f %.5f channels %u\n", lo, hi, step, chs);
        h1 = IAudioEndpointVolume_SetMasterVolumeLevelScalar(epv, 1.0f, NULL);
        IAudioEndpointVolume_GetMasterVolumeLevel(epv, &db);
        IAudioEndpointVolume_GetMasterVolumeLevelScalar(epv, &sc_);
        h2 = IAudioEndpointVolume_SetMasterVolumeLevelScalar(epv, 1.5f, NULL);
        printf("set scalar %08lx db %.3f scalar %.3f bad %08lx\n", h1, db, sc_, h2);
        IAudioEndpointVolume_Release(epv);
    }

    IAudioClient_Stop(ac);
    IAudioRenderClient_Release(rc);
    IAudioClock_Release(clock);
    IAudioStreamVolume_Release(sv);
    ISimpleAudioVolume_Release(simple);
    IChannelAudioVolume_Release(chan);
    IAudioSessionControl_Release(sc);
    IAudioSessionControl_Release(sc2);
    printf("release client %lu\n", IAudioClient_Release(ac));
    CoTaskMemFree(mix);
    fflush(stdout);
    return 0;
}
