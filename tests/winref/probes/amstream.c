/* amstream's multimedia streaming as ShiinaRio plays its movies (#49): IAMMultiMediaStream with
 * a DirectDraw video stream, OpenFile, a sample of a surface it makes (CreateSample with none),
 * SetState(RUN), then Update after Update into the surface until MS_S_ENDOFSTREAM; the end's
 * event; GetFilterGraph and FindFilterByName on it; the errors on the way. The movie is
 * dshow.c's: an uncompressed AVI (64x48, 10 fps, 20 frames, each frame's halves two grays) and
 * the `.vnv` masu pack makes of it, which sake plays.
 * (x64 too: the 64-bit DDSURFACEDESC, #128)
 * libs: -lole32 -loleaut32 -luuid -lddraw */
#include "../probe.h"
#include <initguid.h>
#include <ddraw.h>
#include <mmstream.h>
#include <amstream.h>
#include <ddstream.h>
#define NO_DSHOW_STRSAFE
#include <dshow.h>

#define W 64
#define H 48
#define FRAMES 20
#define FPS 10

/* (spelled out: the import libraries' copies differ between mingw's versions) */
DEFINE_GUID(CLSID_AMMS, 0x49c47ce5, 0x9ba4, 0x11d0, 0x82, 0x12, 0x00, 0xc0, 0x4f, 0xc3, 0x2c, 0x45);
DEFINE_GUID(IID_AMMS, 0xbebe595c, 0x9a6f, 0x11d0, 0x8f, 0xde, 0x00, 0xc0, 0x4f, 0xd9, 0x18, 0x9d);
DEFINE_GUID(IID_DDMS, 0xf4104fce, 0x9a70, 0x11d0, 0x8f, 0xde, 0x00, 0xc0, 0x4f, 0xd9, 0x18, 0x9d);
DEFINE_GUID(PID_VIDEO, 0xa35ff56a, 0x9fda, 0x11d0, 0x8f, 0xdf, 0x00, 0xc0, 0x4f, 0xd9, 0x18, 0x9d);
DEFINE_GUID(PID_AUDIO, 0xa35ff56b, 0x9fda, 0x11d0, 0x8f, 0xdf, 0x00, 0xc0, 0x4f, 0xd9, 0x18, 0x9d);
DEFINE_GUID(IID_FG, 0x56a8689f, 0x0ad4, 0x11ce, 0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70);

static int gray_y(int k, int bottom) { return 16 + 8 * k + (bottom ? 40 : 0); }
static int gray(int y) { return ((y - 16) * 255 * 2 + 219) / (2 * 219); }
static void put32le(FILE *f, unsigned v) { fwrite(&v, 4, 1, f); }
static void put16le(FILE *f, unsigned v) { unsigned short s = v; fwrite(&s, 2, 1, f); }
static void fourcc(FILE *f, const char *s) { fwrite(s, 4, 1, f); }

static void write_avi(const char *path) {
    FILE *f = fopen(path, "wb");
    int frame = W * 3 * H;
    int hdrl = 4 + (8 + 56) + (8 + 4 + (8 + 56) + (8 + 40));
    int movi = 4 + FRAMES * (8 + frame);
    int idx = FRAMES * 16;
    fourcc(f, "RIFF"); put32le(f, 4 + (8 + hdrl) + (8 + movi) + (8 + idx)); fourcc(f, "AVI ");
    fourcc(f, "LIST"); put32le(f, hdrl); fourcc(f, "hdrl");
    fourcc(f, "avih"); put32le(f, 56);
    unsigned avih[14] = { 1000000 / FPS, frame * FPS, 0, 0x10, FRAMES, 0, 1, frame, W, H, 0, 0, 0, 0 };
    fwrite(avih, 4, 14, f);
    fourcc(f, "LIST"); put32le(f, 4 + (8 + 56) + (8 + 40)); fourcc(f, "strl");
    fourcc(f, "strh"); put32le(f, 56);
    fourcc(f, "vids"); put32le(f, 0);
    unsigned strh[10] = { 0, 0, 0, 1, FPS, 0, FRAMES, frame, 0xffffffff, 0 };
    fwrite(strh, 4, 10, f);
    put16le(f, 0); put16le(f, 0); put16le(f, W); put16le(f, H);
    fourcc(f, "strf"); put32le(f, 40);
    BITMAPINFOHEADER bi = { 40, W, H, 1, 24, BI_RGB, frame, 0, 0, 0, 0 };
    fwrite(&bi, 40, 1, f);
    fourcc(f, "LIST"); put32le(f, movi); fourcc(f, "movi");
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        for (int y = 0; y < H; y++) memset(px + y * W * 3, gray(gray_y(k, H - 1 - y >= H / 2)), W * 3);
        fourcc(f, "00db"); put32le(f, frame); fwrite(px, frame, 1, f);
    }
    fourcc(f, "idx1"); put32le(f, idx);
    for (int k = 0; k < FRAMES; k++) { fourcc(f, "00db"); put32le(f, 0x10); put32le(f, 4 + k * (8 + frame)); put32le(f, frame); }
    fclose(f);
    free(px);
}

static void write_vnv(const char *path) {
    FILE *f = fopen(path, "wb");
    const char *codec = "raw-i420";
    int frame = W * H + 2 * (W / 2) * (H / 2);
    int table = 8 + (int)strlen(codec) + 12 * FRAMES;
    fourcc(f, "VNV1"); put32le(f, W); put32le(f, H); put32le(f, 1);
    put32le(f, strlen(codec)); fwrite(codec, strlen(codec), 1, f); put32le(f, FRAMES);
    for (int k = 0; k < FRAMES; k++) { put32le(f, 16 + table + k * frame); put32le(f, frame); put32le(f, 1); }
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        for (int y = 0; y < H; y++) memset(px + y * W, gray_y(k, y >= H / 2), W);
        memset(px + W * H, 128, 2 * (W / 2) * (H / 2));
        fwrite(px, frame, 1, f);
    }
    unsigned long long dur = 10000000ull * FRAMES / FPS;
    put32le(f, FPS); put32le(f, 1); put32le(f, (unsigned)dur); put32le(f, (unsigned)(dur >> 32));
    put32le(f, 0); put32le(f, 0); fourcc(f, "VNVt");
    fclose(f);
    free(px);
}

/* which frame the surface holds: by the grays of its two halves (-1: neither) */
static int frame_of(IDirectDrawSurface *s, int *bpp_out) {
    DDSURFACEDESC d = { sizeof d };
    if (FAILED(s->lpVtbl->Lock(s, NULL, &d, DDLOCK_WAIT, NULL))) return -2;
    int bpp = d.ddpfPixelFormat.dwRGBBitCount / 8, k = -1;
    *bpp_out = bpp * 8;
    unsigned char *top = (unsigned char *)d.lpSurface + 2 * d.lPitch + 5 * bpp;
    unsigned char *bottom = (unsigned char *)d.lpSurface + (H - 3) * d.lPitch + 5 * bpp;
    if (bpp >= 3) {
        for (int i = 0; i < FRAMES; i++)
            if (abs(top[1] - gray(gray_y(i, 0))) <= 2 && abs(bottom[1] - gray(gray_y(i, 1))) <= 2) k = i;
    }
    s->lpVtbl->Unlock(s, NULL);
    return k;
}

int main(void) {
    char dir[MAX_PATH], avi[MAX_PATH], vnv[MAX_PATH];
    WCHAR wavi[MAX_PATH];
    GetCurrentDirectoryA(MAX_PATH, dir);
    sprintf(avi, "%s\\amstream.avi", dir);
    sprintf(vnv, "%s.vnv", avi);
    write_avi(avi);
    write_vnv(vnv);
    MultiByteToWideChar(CP_ACP, 0, avi, -1, wavi, MAX_PATH);
    CoInitialize(NULL);

    IAMMultiMediaStream *mms = NULL;
    HRESULT hr = CoCreateInstance(&CLSID_AMMS, NULL, CLSCTX_INPROC_SERVER, &IID_AMMS, (void **)&mms);
    printf("CoCreateInstance %08lx %s\n", hr, mms ? "object" : "null");
    if (!mms) return 0;
    printf("Initialize %08lx\n", mms->lpVtbl->Initialize(mms, STREAMTYPE_READ, 0, NULL));
    IMediaStream *ms = NULL;
    printf("GetMediaStream before add %08lx %s\n", mms->lpVtbl->GetMediaStream(mms, &PID_VIDEO, &ms), ms ? "stream" : "null");
    printf("AddMediaStream video %08lx\n", mms->lpVtbl->AddMediaStream(mms, NULL, &PID_VIDEO, 0, NULL));
    printf("AddMediaStream video again %08lx\n", mms->lpVtbl->AddMediaStream(mms, NULL, &PID_VIDEO, 0, NULL));
    STREAM_TIME t = 0;
    printf("GetDuration before open %08lx\n", mms->lpVtbl->GetDuration(mms, &t));
    printf("OpenFile missing %08lx\n", mms->lpVtbl->OpenFile(mms, L"Z:\\no\\such\\movie.avi", 0));
    printf("OpenFile %08lx\n", mms->lpVtbl->OpenFile(mms, wavi, 0));
    hr = mms->lpVtbl->GetDuration(mms, &t);
    printf("GetDuration %08lx %.2f s\n", hr, t / 1e7);
    DWORD flags = 0;
    STREAM_TYPE type = 9;
    hr = mms->lpVtbl->GetInformation(mms, &flags, &type);
    printf("GetInformation %08lx flags %lx type %d\n", hr, flags, (int)type);

    IMediaStream *audio = NULL;
    printf("GetMediaStream audio %08lx %s\n", mms->lpVtbl->GetMediaStream(mms, &PID_AUDIO, &audio), audio ? "stream" : "null");
    hr = mms->lpVtbl->GetMediaStream(mms, &PID_VIDEO, &ms);
    printf("GetMediaStream video %08lx\n", hr);
    if (FAILED(hr)) return 0;
    IDirectDrawMediaStream *dds = NULL;
    printf("QueryInterface IDirectDrawMediaStream %08lx\n", ms->lpVtbl->QueryInterface(ms, &IID_DDMS, (void **)&dds));
    if (!dds) return 0;
    DDSURFACEDESC cur = { sizeof cur };
    hr = dds->lpVtbl->GetFormat(dds, &cur, NULL, NULL, NULL);
    printf("GetFormat %08lx %ldx%ld\n", hr, cur.dwWidth, cur.dwHeight);
    /* 32-bit RGB frames, as a game asks for its own surface's format */
    DDSURFACEDESC want = { sizeof want };
    want.dwFlags = DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    want.dwWidth = W;
    want.dwHeight = H;
    want.ddpfPixelFormat.dwSize = sizeof want.ddpfPixelFormat;
    want.ddpfPixelFormat.dwFlags = DDPF_RGB;
    want.ddpfPixelFormat.dwRGBBitCount = 32;
    want.ddpfPixelFormat.dwRBitMask = 0xff0000;
    want.ddpfPixelFormat.dwGBitMask = 0xff00;
    want.ddpfPixelFormat.dwBBitMask = 0xff;
    printf("SetFormat %08lx\n", dds->lpVtbl->SetFormat(dds, &want, NULL));
    STREAM_TIME per = 0;
    hr = dds->lpVtbl->GetTimePerFrame(dds, &per);
    printf("GetTimePerFrame %08lx %.3f s\n", hr, per / 1e7);
    IDirectDrawStreamSample *sample = NULL;
    hr = dds->lpVtbl->CreateSample(dds, NULL, NULL, 0, &sample);
    printf("CreateSample %08lx\n", hr);
    if (!sample) return 0;
    IDirectDrawSurface *surf = NULL;
    RECT r = { 0 };
    hr = sample->lpVtbl->GetSurface(sample, &surf, &r);
    printf("GetSurface %08lx %s rect %ld,%ld,%ld,%ld\n", hr, surf ? "surface" : "null", r.left, r.top, r.right, r.bottom);
    if (!surf) return 0;

    printf("Update stopped %08lx\n", sample->lpVtbl->Update(sample, 0, NULL, NULL, 0));
    HANDLE eos = NULL;
    printf("GetEndOfStreamEventHandle %08lx\n", mms->lpVtbl->GetEndOfStreamEventHandle(mms, &eos));
    printf("end event before run %s\n", WaitForSingleObject(eos, 0) == WAIT_OBJECT_0 ? "set" : "not set");
    printf("SetState run %08lx\n", mms->lpVtbl->SetState(mms, STREAMSTATE_RUN));
    STREAM_STATE st = 9;
    mms->lpVtbl->GetState(mms, &st);
    printf("GetState %d\n", (int)st);

    int first = -9, last = -9, n = 0, ordered = 1, bpp = 0;
    DWORD start = GetTickCount();
    for (;;) {
        hr = sample->lpVtbl->Update(sample, 0, NULL, NULL, 0);
        if (hr != S_OK) break;
        int k = frame_of(surf, &bpp);
        if (n == 0) first = k;
        if (n > 0 && k <= last) ordered = 0;
        last = k;
        n++;
        if (GetTickCount() - start > 10000) break;
    }
    DWORD took = GetTickCount() - start;
    printf("Update to the end %08lx\n", hr);
    printf("frames: first %d, last %d, in order %s, %s\n", first, last, ordered ? "yes" : "no", n >= FRAMES / 2 ? "most shown" : "few shown");
    printf("surface %d bits\n", bpp);
    printf("played in %s\n", took >= 1500 && took <= 4000 ? "about the movie's length" : "an unexpected time");
    if (took < 1500 || took > 4000) fprintf(stderr, "played in %lu ms, %d updates\n", took, n);
    printf("end event %s\n", WaitForSingleObject(eos, 0) == WAIT_OBJECT_0 ? "set" : "not set");
    printf("Update after the end %08lx\n", sample->lpVtbl->Update(sample, 0, NULL, NULL, 0));
    mms->lpVtbl->GetTime(mms, &t);
    printf("GetTime at the end %.1f s\n", t / 1e7);

    printf("Seek 0 %08lx\n", mms->lpVtbl->Seek(mms, 0));
    printf("end event after seek %s\n", WaitForSingleObject(eos, 0) == WAIT_OBJECT_0 ? "set" : "not set");
    hr = sample->lpVtbl->Update(sample, 0, NULL, NULL, 0);
    int k = frame_of(surf, &bpp);
    printf("Update after seek %08lx frame %s\n", hr, k >= 0 && k <= 2 ? "near the start" : "elsewhere");

    IGraphBuilder *gb = NULL;
    hr = mms->lpVtbl->GetFilterGraph(mms, &gb);
    printf("GetFilterGraph %08lx %s\n", hr, gb ? "graph" : "null");
    if (gb) {
        IFilterGraph *fg = NULL;
        printf("QueryInterface IFilterGraph %08lx\n", gb->lpVtbl->QueryInterface(gb, &IID_FG, (void **)&fg));
        IBaseFilter *f = (IBaseFilter *)1;
        hr = fg ? fg->lpVtbl->FindFilterByName(fg, L"no filter of this name", &f) : E_FAIL;
        printf("FindFilterByName %08lx %s\n", hr, f ? "filter" : "null");
        if (fg) fg->lpVtbl->Release(fg);
        gb->lpVtbl->Release(gb);
    }
    printf("SetState stop %08lx\n", mms->lpVtbl->SetState(mms, STREAMSTATE_STOP));
    printf("Update stopped again %08lx\n", sample->lpVtbl->Update(sample, 0, NULL, NULL, 0));
    surf->lpVtbl->Release(surf);
    sample->lpVtbl->Release(sample);
    dds->lpVtbl->Release(dds);
    ms->lpVtbl->Release(ms);
    mms->lpVtbl->Release(mms);
    printf("done\n");
    return 0;
}
