/* MMSYSTEM's waveOut in a 16-bit program: devices and their caps, a format query, errors, then
 * two buffers played through a window callback (MM_WOM_OPEN, MM_WOM_DONE with the headers' far
 * pointers in order, MM_WOM_CLOSE), the headers' flags and the position in bytes. (#37)
 * libs: mmsystem.lib */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>
#include <string.h>

#define N 1102
static char far buf1[N], buf2[N];
static WAVEHDR far h1, h2;
static HWAVEOUT hwo;
static int done;

LONG FAR PASCAL __export WndProc(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    switch (m) {
    case MM_WOM_OPEN:
        printf("MM_WOM_OPEN same handle %d\n", (HWAVEOUT)wp == hwo);
        return 0;
    case MM_WOM_DONE:
        printf("MM_WOM_DONE %s flags %lx\n", (LPWAVEHDR)lp == &h1 ? "h1" : (LPWAVEHDR)lp == &h2 ? "h2" : "?",
               ((LPWAVEHDR)lp)->dwFlags & (WHDR_DONE | WHDR_PREPARED | WHDR_INQUEUE));
        if (++done == 2) PostQuitMessage(0);
        return 0;
    case MM_WOM_CLOSE:
        printf("MM_WOM_CLOSE\n");
        return 0;
    case WM_TIMER:
        printf("timed out\n");
        PostQuitMessage(1);
        return 0;
    }
    return DefWindowProc(w, m, wp, lp);
}

int PASCAL WinMain(HINSTANCE hi, HINSTANCE hp, LPSTR cmd, int sh) {
    WNDCLASS wc;
    HWND w;
    MSG msg;
    WAVEOUTCAPS caps;
    PCMWAVEFORMAT f;
    MMTIME t;
    UINT r;
    int i;
    char err[80];
    memset(&wc, 0, sizeof wc);
    wc.lpfnWndProc = WndProc;
    wc.hInstance = hi;
    wc.lpszClassName = "wave16";
    RegisterClass(&wc);
    w = CreateWindow("wave16", "wave16", WS_OVERLAPPED, 0, 0, 100, 100, 0, 0, hi, 0);
    printf("devices %d\n", waveOutGetNumDevs() > 0);
    r = waveOutGetDevCaps(0, &caps, sizeof caps);
    printf("caps %u name %d channels %u\n", r, caps.szPname[0] != 0, caps.wChannels);
    f.wf.wFormatTag = WAVE_FORMAT_PCM;
    f.wf.nChannels = 1;
    f.wf.nSamplesPerSec = 11025;
    f.wf.nAvgBytesPerSec = 11025;
    f.wf.nBlockAlign = 1;
    f.wBitsPerSample = 8;
    printf("query %u\n", waveOutOpen(0, WAVE_MAPPER, (LPWAVEFORMAT)&f, 0, 0, WAVE_FORMAT_QUERY));
    f.wf.nChannels = 0;
    printf("bad format %u\n", waveOutOpen(&hwo, WAVE_MAPPER, (LPWAVEFORMAT)&f, 0, 0, 0));
    waveOutGetErrorText(WAVERR_BADFORMAT, err, sizeof err);
    printf("error text %d\n", err[0] != 0);
    f.wf.nChannels = 1;
    r = waveOutOpen(&hwo, WAVE_MAPPER, (LPWAVEFORMAT)&f, (DWORD)(UINT)w, 0, CALLBACK_WINDOW);
    printf("open %u handle %d\n", r, hwo != 0);
    for (i = 0; i < N; i++) buf1[i] = buf2[i] = (char)(i & 64 ? 0xc0 : 0x40);
    h1.lpData = buf1;
    h1.dwBufferLength = N;
    h2.lpData = buf2;
    h2.dwBufferLength = N;
    printf("write unprepared %u\n", waveOutWrite(hwo, &h1, sizeof h1));
    r = waveOutPrepareHeader(hwo, &h1, sizeof h1);
    printf("prepare %u", r);
    r = waveOutPrepareHeader(hwo, &h2, sizeof h2);
    printf(" %u flags %lx\n", r, h1.dwFlags);
    r = waveOutWrite(hwo, &h1, sizeof h1);
    printf("write %u", r);
    r = waveOutWrite(hwo, &h2, sizeof h2);
    printf(" %u flags %lx\n", r, h2.dwFlags & (WHDR_DONE | WHDR_PREPARED | WHDR_INQUEUE));
    printf("write again %u\n", waveOutWrite(hwo, &h2, sizeof h2));
    SetTimer(w, 1, 10000, 0);
    while (GetMessage(&msg, 0, 0, 0)) DispatchMessage(&msg);
    KillTimer(w, 1);
    t.wType = TIME_BYTES;
    r = waveOutGetPosition(hwo, &t, sizeof t);
    printf("position %u type %u bytes %lu\n", r, t.wType, t.u.cb);
    r = waveOutUnprepareHeader(hwo, &h1, sizeof h1);
    printf("unprepare %u", r);
    r = waveOutUnprepareHeader(hwo, &h2, sizeof h2);
    printf(" %u flags %lx\n", r, h1.dwFlags);
    printf("close %u\n", waveOutClose(hwo));
    while (PeekMessage(&msg, 0, 0, 0, PM_REMOVE)) DispatchMessage(&msg);
    return 0;
}
