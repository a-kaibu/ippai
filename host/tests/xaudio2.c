/* XAudio2 2.7 (CoCreateInstance CLSID_XAudio2): a mastering voice, a source voice with a
 * callback, two buffers played to the end of the stream, the voice state, volume and frequency
 * ratio round trips. The 2.7 interfaces are declared here (mingw's xaudio2.h is 2.8+). Prints
 * only what any XAudio2 2.7 gives; built for x86 and x86-64 (the same lines from each). No C
 * runtime.
 * libs: -nostdlib -lole32 -luser32 -lkernel32 */
#include <windows.h>

void *memset(void *d, int c, size_t n)
{
    char *p = d;
    while (n--) *p++ = (char)c;
    return d;
}

static void out(const char *f, ...)
{
    char b[512];
    DWORD n;
    va_list a;
    va_start(a, f);
    n = wvsprintfA(b, f, a);
    va_end(a);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), b, n, &n, NULL);
}

typedef struct { UINT32 Flags; void *pOutputVoice; } SEND;
typedef struct { UINT32 SendCount; SEND *pSends; } SENDS;
typedef struct {
    UINT32 Flags, AudioBytes;
    const BYTE *pAudioData;
    UINT32 PlayBegin, PlayLength, LoopBegin, LoopLength, LoopCount;
    void *pContext;
} XBUFFER;
typedef struct { void *pCurrentBufferContext; UINT32 BuffersQueued; UINT64 SamplesPlayed; } XSTATE;
typedef struct { UINT32 CreationFlags, InputChannels, InputSampleRate; } XDETAILS;

typedef struct Voice Voice;
/* IXAudio2Voice then IXAudio2SourceVoice (2.7: no IUnknown) */
typedef struct {
    void(STDMETHODCALLTYPE *GetVoiceDetails)(Voice *, XDETAILS *);
    HRESULT(STDMETHODCALLTYPE *SetOutputVoices)(Voice *, const SENDS *);
    void *SetEffectChain, *EnableEffect, *DisableEffect, *GetEffectState, *SetEffectParameters,
        *GetEffectParameters, *SetFilterParameters, *GetFilterParameters, *SetOutputFilterParameters,
        *GetOutputFilterParameters;
    HRESULT(STDMETHODCALLTYPE *SetVolume)(Voice *, float, UINT32);
    void(STDMETHODCALLTYPE *GetVolume)(Voice *, float *);
    void *SetChannelVolumes, *GetChannelVolumes, *SetOutputMatrix, *GetOutputMatrix;
    void(STDMETHODCALLTYPE *DestroyVoice)(Voice *);
    HRESULT(STDMETHODCALLTYPE *Start)(Voice *, UINT32, UINT32);
    HRESULT(STDMETHODCALLTYPE *Stop)(Voice *, UINT32, UINT32);
    HRESULT(STDMETHODCALLTYPE *SubmitSourceBuffer)(Voice *, const XBUFFER *, const void *);
    HRESULT(STDMETHODCALLTYPE *FlushSourceBuffers)(Voice *);
    HRESULT(STDMETHODCALLTYPE *Discontinuity)(Voice *);
    HRESULT(STDMETHODCALLTYPE *ExitLoop)(Voice *, UINT32);
    void(STDMETHODCALLTYPE *GetState)(Voice *, XSTATE *);
    HRESULT(STDMETHODCALLTYPE *SetFrequencyRatio)(Voice *, float, UINT32);
    void(STDMETHODCALLTYPE *GetFrequencyRatio)(Voice *, float *);
} VoiceVtbl;
struct Voice { const VoiceVtbl *v; };

typedef struct XA XA;
typedef struct {
    HRESULT(STDMETHODCALLTYPE *QueryInterface)(XA *, REFIID, void **);
    ULONG(STDMETHODCALLTYPE *AddRef)(XA *);
    ULONG(STDMETHODCALLTYPE *Release)(XA *);
    HRESULT(STDMETHODCALLTYPE *GetDeviceCount)(XA *, UINT32 *);
    void *GetDeviceDetails;
    HRESULT(STDMETHODCALLTYPE *Initialize)(XA *, UINT32, UINT32);
    void *RegisterForCallbacks, *UnregisterForCallbacks;
    HRESULT(STDMETHODCALLTYPE *CreateSourceVoice)(XA *, Voice **, const WAVEFORMATEX *, UINT32, float, void *, const SENDS *, const void *);
    HRESULT(STDMETHODCALLTYPE *CreateSubmixVoice)(XA *, Voice **, UINT32, UINT32, UINT32, UINT32, const SENDS *, const void *);
    HRESULT(STDMETHODCALLTYPE *CreateMasteringVoice)(XA *, Voice **, UINT32, UINT32, UINT32, UINT32, const void *);
    HRESULT(STDMETHODCALLTYPE *StartEngine)(XA *);
    void(STDMETHODCALLTYPE *StopEngine)(XA *);
    void *CommitChanges, *GetPerformanceData, *SetDebugConfiguration;
} XAVtbl;
struct XA { const XAVtbl *v; };

/* the callback: counts, and the contexts in the order they end */
typedef struct Cb Cb;
typedef struct {
    void(STDMETHODCALLTYPE *OnVoiceProcessingPassStart)(Cb *, UINT32);
    void(STDMETHODCALLTYPE *OnVoiceProcessingPassEnd)(Cb *);
    void(STDMETHODCALLTYPE *OnStreamEnd)(Cb *);
    void(STDMETHODCALLTYPE *OnBufferStart)(Cb *, void *);
    void(STDMETHODCALLTYPE *OnBufferEnd)(Cb *, void *);
    void(STDMETHODCALLTYPE *OnLoopEnd)(Cb *, void *);
    void(STDMETHODCALLTYPE *OnVoiceError)(Cb *, void *, HRESULT);
} CbVtbl;
struct Cb { const CbVtbl *v; volatile LONG passes, starts, ends, streams; void *ended[4]; };

static void STDMETHODCALLTYPE pass_start(Cb *c, UINT32 n) { (void)n; InterlockedIncrement(&c->passes); }
static void STDMETHODCALLTYPE pass_end(Cb *c) { (void)c; }
static void STDMETHODCALLTYPE stream_end(Cb *c) { InterlockedIncrement(&c->streams); }
static void STDMETHODCALLTYPE buf_start(Cb *c, void *p) { (void)p; InterlockedIncrement(&c->starts); }
static void STDMETHODCALLTYPE buf_end(Cb *c, void *p)
{
    LONG i = InterlockedIncrement(&c->ends) - 1;
    if (i < 4) c->ended[i] = p;
}
static void STDMETHODCALLTYPE loop_end(Cb *c, void *p) { (void)c; (void)p; }
static void STDMETHODCALLTYPE voice_error(Cb *c, void *p, HRESULT h) { (void)c; (void)p; (void)h; }
static const CbVtbl cbv = {pass_start, pass_end, stream_end, buf_start, buf_end, loop_end, voice_error};

static const GUID CLSID_XAudio2_27 = {0x5a508685, 0xa254, 0x4fba, {0x9b, 0x82, 0x9a, 0x24, 0xb0, 0x03, 0x06, 0xaf}};
static const GUID IID_IXAudio2_27 = {0x8bcf1f58, 0x9fe7, 0x4583, {0x8a, 0xc6, 0xe2, 0xad, 0xc4, 0x65, 0xc8, 0xbb}};

static short pcm[2][4410 * 2];
/* the buffers' contexts: static data (on x64 in the image, above 4 GB) */
static int ctx_a, ctx_b;

void mainCRTStartup(void)
{
    XA *xa = NULL;
    Voice *master = NULL, *src = NULL;
    static Cb cb = {&cbv};
    HRESULT hr;
    CoInitializeEx(NULL, 0);
    hr = CoCreateInstance(&CLSID_XAudio2_27, NULL, CLSCTX_INPROC_SERVER, &IID_IXAudio2_27, (void **)&xa);
    out("create %x %d\n", (unsigned)hr, xa != NULL);
    if (!xa) ExitProcess(1);
    out("initialize %x\n", (unsigned)xa->v->Initialize(xa, 0, 1));
    hr = xa->v->CreateMasteringVoice(xa, &master, 2, 48000, 0, 0, NULL);
    out("mastering %x %d\n", (unsigned)hr, master != NULL);

    WAVEFORMATEX wf = {WAVE_FORMAT_PCM, 2, 44100, 44100 * 4, 4, 16, 0};
    SEND send = {0, master};
    SENDS sends = {1, &send};
    hr = xa->v->CreateSourceVoice(xa, &src, &wf, 0, 2.0f, &cb, &sends, NULL);
    out("source %x %d\n", (unsigned)hr, src != NULL);
    XDETAILS d;
    src->v->GetVoiceDetails(src, &d);
    out("details %u %u\n", d.InputChannels, d.InputSampleRate);

    for (int i = 0; i < 4410 * 2; i++) pcm[0][i] = pcm[1][i] = (short)(i * 37);
    XBUFFER b1 = {0, sizeof pcm[0], (BYTE *)pcm[0], 0, 0, 0, 0, 0, &ctx_a};
    XBUFFER b2 = {0x40, sizeof pcm[1], (BYTE *)pcm[1], 0, 0, 0, 0, 0, &ctx_b};
    hr = src->v->SubmitSourceBuffer(src, &b1, NULL);
    out("submit %x %x\n", (unsigned)hr, (unsigned)src->v->SubmitSourceBuffer(src, &b2, NULL));
    XSTATE st;
    src->v->GetState(src, &st);
    out("state queued %u current a %d played %d\n", st.BuffersQueued, st.pCurrentBufferContext == &ctx_a, (int)st.SamplesPlayed);

    float f = 0;
    src->v->SetVolume(src, 0.5f, 0);
    src->v->GetVolume(src, &f);
    out("volume %d\n", (int)(f * 1000));
    src->v->SetFrequencyRatio(src, 1.5f, 0);
    src->v->GetFrequencyRatio(src, &f);
    out("ratio %d\n", (int)(f * 1000));
    src->v->SetFrequencyRatio(src, 1.0f, 0);

    out("start %x\n", (unsigned)src->v->Start(src, 0, 0));
    for (int i = 0; i < 500 && !cb.streams; i++) Sleep(10);
    Sleep(50);
    out("callbacks starts %d ends %d streams %d passes %d\n", (int)cb.starts, (int)cb.ends, (int)cb.streams, cb.passes > 0);
    out("ended a %d b %d\n", cb.ended[0] == &ctx_a, cb.ended[1] == &ctx_b);
    src->v->GetState(src, &st);
    out("state queued %u current %d played all %d\n", st.BuffersQueued, st.pCurrentBufferContext != NULL, st.SamplesPlayed >= 8820);

    src->v->DestroyVoice(src);
    master->v->DestroyVoice(master);
    out("release %u\n", (unsigned)xa->v->Release(xa));
    ExitProcess(0);
}
