/* winmm's MCI and msvfw32's MCIWnd as games use them (#149): a WAVE file through mciSendString
 * (open with an alias, status length / mode / position / time format, set time format, play
 * wait, play notify and what a stop, a second play or the end report, seek, close) and through
 * mciSendCommand (open by type, status, play wait, close); the same sound with no extension
 * opened as `type mpegvideo` (LiLiM's Data\Sound\se\lilim); the errors and their texts; an
 * MCIWnd child playing an uncompressed AVI (its `.vnv` for sake, as masu pack makes) with
 * MCIWNDF_NOTIFYMODE: its size, length, CAN_PLAY / CAN_WINDOW, and the modes it reports.
 * libs: -lwinmm -lvfw32 -lgdi32 */
#include "../probe.h"
#include <mmsystem.h>
#include <vfw.h>

#define RATE 8000
#define SAMPLES 3200 /* 400 ms */
#define W 64
#define H 48
#define FRAMES 20
#define FPS 10

static char dir[MAX_PATH];
static int notes[32], nnotes;
static UINT note_dev[32];
static int modes[32], nmodes;

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == MM_MCINOTIFY && nnotes < 32) { note_dev[nnotes] = (UINT)l; notes[nnotes++] = (int)w; return 0; }
    if (m == MCIWNDM_NOTIFYMODE && nmodes < 32) {
        if (nmodes == 0 || modes[nmodes - 1] != (int)l) modes[nmodes++] = (int)l;
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

static void pump(DWORD ms) {
    DWORD end = GetTickCount() + ms;
    MSG msg;
    while ((LONG)(end - GetTickCount()) > 0) {
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) { TranslateMessage(&msg); DispatchMessageA(&msg); }
        Sleep(5);
    }
}

/* waits for `n` notifications (at most `ms`) */
static void pump_notes(int n, DWORD ms) {
    DWORD end = GetTickCount() + ms;
    MSG msg;
    while (nnotes < n && (LONG)(end - GetTickCount()) > 0) {
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) { TranslateMessage(&msg); DispatchMessageA(&msg); }
        Sleep(5);
    }
}

static void show_notes(const char *what) {
    printf("%s: notes", what);
    for (int i = 0; i < nnotes; i++) printf(" %d", notes[i]);
    printf("\n");
    nnotes = 0;
}

static void str(const char *cmd, HWND cb) {
    char ret[256] = "?";
    MCIERROR e = mciSendStringA(cmd, ret, sizeof ret, cb);
    /* (the paths in commands are the probe's folder: printed without it) */
    const char *shown = cmd, *at = strstr(cmd, dir);
    char buf[512];
    if (at) { snprintf(buf, sizeof buf, "%.*s%s", (int)(at - cmd), cmd, at + strlen(dir)); shown = buf; }
    if (e) printf("[%s] error %lu\n", shown, e);
    else printf("[%s] \"%s\"\n", shown, ret);
}

static void put16(FILE *f, unsigned v) { unsigned short s = v; fwrite(&s, 2, 1, f); }
static void put32(FILE *f, unsigned v) { fwrite(&v, 4, 1, f); }

static void write_wav(const char *path) {
    FILE *f = fopen(path, "wb");
    fwrite("RIFF", 4, 1, f); put32(f, 36 + SAMPLES * 2); fwrite("WAVEfmt ", 8, 1, f);
    put32(f, 16); put16(f, 1); put16(f, 1); put32(f, RATE); put32(f, RATE * 2); put16(f, 2); put16(f, 16);
    fwrite("data", 4, 1, f); put32(f, SAMPLES * 2);
    for (int i = 0; i < SAMPLES; i++) put16(f, (i / 10) % 2 ? 0x1000 : 0xf000);
    fclose(f);
}

static int gray_y(int k, int bottom) { return 16 + 8 * k + (bottom ? 40 : 0); }
static int gray(int y) { return ((y - 16) * 255 * 2 + 219) / (2 * 219); }

static void write_avi(const char *path) {
    FILE *f = fopen(path, "wb");
    int frame = W * 3 * H;
    int hdrl = 4 + (8 + 56) + (8 + 4 + (8 + 56) + (8 + 40));
    int movi = 4 + FRAMES * (8 + frame);
    int idx = FRAMES * 16;
    fwrite("RIFF", 4, 1, f); put32(f, 4 + (8 + hdrl) + (8 + movi) + (8 + idx)); fwrite("AVI ", 4, 1, f);
    fwrite("LIST", 4, 1, f); put32(f, hdrl); fwrite("hdrl", 4, 1, f);
    fwrite("avih", 4, 1, f); put32(f, 56);
    unsigned avih[14] = { 1000000 / FPS, frame * FPS, 0, 0x10, FRAMES, 0, 1, frame, W, H, 0, 0, 0, 0 };
    fwrite(avih, 4, 14, f);
    fwrite("LIST", 4, 1, f); put32(f, 4 + (8 + 56) + (8 + 40)); fwrite("strl", 4, 1, f);
    fwrite("strh", 4, 1, f); put32(f, 56);
    fwrite("vids", 4, 1, f); put32(f, 0);
    unsigned strh[10] = { 0, 0, 0, 1, FPS, 0, FRAMES, frame, 0xffffffff, 0 };
    fwrite(strh, 4, 10, f);
    put16(f, 0); put16(f, 0); put16(f, W); put16(f, H);
    fwrite("strf", 4, 1, f); put32(f, 40);
    BITMAPINFOHEADER bi = { 40, W, H, 1, 24, BI_RGB, frame, 0, 0, 0, 0 };
    fwrite(&bi, 40, 1, f);
    fwrite("LIST", 4, 1, f); put32(f, movi); fwrite("movi", 4, 1, f);
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        for (int y = 0; y < H; y++) memset(px + y * W * 3, gray(gray_y(k, H - 1 - y >= H / 2)), W * 3);
        fwrite("00db", 4, 1, f); put32(f, frame); fwrite(px, frame, 1, f);
    }
    fwrite("idx1", 4, 1, f); put32(f, idx);
    for (int k = 0; k < FRAMES; k++) { fwrite("00db", 4, 1, f); put32(f, 0x10); put32(f, 4 + k * (8 + frame)); put32(f, frame); }
    fclose(f);
    free(px);
}

static void write_vnv(const char *path) {
    FILE *f = fopen(path, "wb");
    const char *codec = "raw-i420";
    int frame = W * H + 2 * (W / 2) * (H / 2);
    int table = 8 + (int)strlen(codec) + 12 * FRAMES;
    fwrite("VNV1", 4, 1, f); put32(f, W); put32(f, H); put32(f, 1);
    put32(f, strlen(codec)); fwrite(codec, strlen(codec), 1, f); put32(f, FRAMES);
    for (int k = 0; k < FRAMES; k++) { put32(f, 16 + table + k * frame); put32(f, frame); put32(f, 1); }
    unsigned char *px = malloc(frame);
    for (int k = 0; k < FRAMES; k++) {
        for (int y = 0; y < H; y++) memset(px + y * W, gray_y(k, y >= H / 2), W);
        memset(px + W * H, 128, 2 * (W / 2) * (H / 2));
        fwrite(px, frame, 1, f);
    }
    unsigned long long dur = 10000000ull * FRAMES / FPS;
    put32(f, FPS); put32(f, 1); put32(f, (unsigned)dur); put32(f, (unsigned)(dur >> 32));
    put32(f, 0); put32(f, 0); fwrite("VNVt", 4, 1, f);
    fclose(f);
    free(px);
}

int main(void) {
    char wav[MAX_PATH], bare[MAX_PATH], avi[MAX_PATH], vnv[MAX_PATH], cmd[1024];
    GetCurrentDirectoryA(MAX_PATH, dir);
    sprintf(wav, "%s\\mci_tone.wav", dir);
    sprintf(bare, "%s\\mci_tone", dir);
    sprintf(avi, "%s\\mci_movie.avi", dir);
    sprintf(vnv, "%s.vnv", avi);
    write_wav(wav);
    CopyFileA(wav, bare, FALSE);
    write_avi(avi);
    write_vnv(vnv);

    WNDCLASSA wc = { 0, proc, 0, 0, GetModuleHandleA(NULL), 0, 0, 0, 0, "mciprobe" };
    RegisterClassA(&wc);
    HWND hwnd = CreateWindowA("mciprobe", "mci", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 320, 240, 0, 0, wc.hInstance, 0);

    /* mciSendString */
    sprintf(cmd, "open \"%s\" alias w", wav); str(cmd, NULL);
    str("status w length", NULL);
    str("status w mode", NULL);
    str("status w time format", NULL);
    str("status w ready", NULL);
    str("set w time format samples", NULL);
    str("status w length", NULL);
    str("set w time format bytes", NULL);
    str("status w length", NULL);
    str("set w time format frames", NULL);
    str("set w time format ms", NULL);
    str("play w wait", NULL);
    str("status w mode", NULL);
    str("status w position", NULL);
    str("seek w to 100", NULL);
    str("status w position", NULL);
    str("seek w to end", NULL);
    str("status w position", NULL);
    str("seek w to 500", NULL);
    str("seek w to start", NULL);
    str("status w position", NULL);
    str("play w notify", hwnd);
    str("status w mode", NULL);
    pump_notes(1, 3000);
    show_notes("play to the end");
    str("status w mode", NULL);
    str("status w position", NULL);
    str("play w from 0 notify", hwnd);
    str("stop w", NULL);
    pump(100);
    show_notes("stopped");
    str("play w from 0 notify", hwnd);
    str("play w from 0 notify", hwnd);
    pump_notes(2, 3000);
    show_notes("played again");
    str("play w from 0 to 100 notify", hwnd);
    pump_notes(1, 3000);
    show_notes("played to 100");
    str("status w position", NULL);
    str("play w from 300 to 100", NULL);
    str("pause w", NULL);
    str("status w mode", NULL);
    str("play w from 0", NULL);
    str("pause w", NULL);
    str("status w mode", NULL);
    str("resume w", NULL);
    str("status w mode", NULL);
    str("stop w", NULL);
    str("status w mode", NULL);
    str("close w notify", hwnd);
    pump(50);
    show_notes("closed");
    str("status w mode", NULL);

    /* the sound with no extension: as mpegvideo, or no type at all */
    sprintf(cmd, "open \"%s\" type mpegvideo alias m", bare); str(cmd, NULL);
    str("status m length", NULL);
    str("play m wait", NULL);
    str("status m mode", NULL);
    str("close m", NULL);
    sprintf(cmd, "open \"%s\" alias n", bare); str(cmd, NULL);

    /* errors */
    sprintf(cmd, "open \"%s\\nothere.wav\" alias e", dir); str(cmd, NULL);
    sprintf(cmd, "open \"%s\" type nosuchdevice alias e", wav); str(cmd, NULL);
    str("frob w", NULL);
    str("status nosuch mode", NULL);
    sprintf(cmd, "open \"%s\" alias d", wav); str(cmd, NULL);
    sprintf(cmd, "open \"%s\" alias d", wav); str(cmd, NULL);
    str("status d nonsense", NULL);
    str("close all", NULL);
    static const MCIERROR codes[] = { 0, 257, 259, 261, 263, 275, 281, 282, 289, 293 };
    for (unsigned i = 0; i < sizeof codes / sizeof codes[0]; i++) {
        char text[256] = "?";
        BOOL ok = mciGetErrorStringA(codes[i], text, sizeof text);
        printf("mciGetErrorString %lu: %d \"%s\"\n", codes[i], ok, text);
    }

    /* mciSendCommand */
    MCI_OPEN_PARMSA op = { 0 };
    op.lpstrDeviceType = "waveaudio";
    op.lpstrElementName = wav;
    MCIERROR e = mciSendCommandA(0, MCI_OPEN, MCI_OPEN_TYPE | MCI_OPEN_ELEMENT, (DWORD_PTR)&op);
    printf("MCI_OPEN %lu id %s\n", e, op.wDeviceID ? "set" : "0");
    MCI_STATUS_PARMS st = { 0 };
    st.dwItem = MCI_STATUS_LENGTH;
    e = mciSendCommandA(op.wDeviceID, MCI_STATUS, MCI_STATUS_ITEM, (DWORD_PTR)&st);
    printf("MCI_STATUS length %lu %lu\n", e, (unsigned long)st.dwReturn);
    st.dwItem = MCI_STATUS_MODE;
    e = mciSendCommandA(op.wDeviceID, MCI_STATUS, MCI_STATUS_ITEM, (DWORD_PTR)&st);
    printf("MCI_STATUS mode %lu %lu\n", e, (unsigned long)st.dwReturn);
    MCI_PLAY_PARMS pp = { (DWORD_PTR)hwnd, 0, 0 };
    e = mciSendCommandA(op.wDeviceID, MCI_PLAY, MCI_NOTIFY, (DWORD_PTR)&pp);
    printf("MCI_PLAY notify %lu\n", e);
    pump_notes(1, 3000);
    printf("notify for the device: %s\n", nnotes == 1 && note_dev[0] == op.wDeviceID ? "yes" : "no");
    show_notes("MCI_PLAY");
    MCI_SEEK_PARMS sp = { 0, 0 };
    e = mciSendCommandA(op.wDeviceID, MCI_SEEK, MCI_SEEK_TO_START, (DWORD_PTR)&sp);
    printf("MCI_SEEK start %lu\n", e);
    e = mciSendCommandA(op.wDeviceID, MCI_PLAY, MCI_WAIT, (DWORD_PTR)&pp);
    printf("MCI_PLAY wait %lu\n", e);
    st.dwItem = MCI_STATUS_POSITION;
    e = mciSendCommandA(op.wDeviceID, MCI_STATUS, MCI_STATUS_ITEM, (DWORD_PTR)&st);
    printf("MCI_STATUS position %lu %lu\n", e, (unsigned long)st.dwReturn);
    e = mciSendCommandA(op.wDeviceID, MCI_STOP, 0, 0);
    printf("MCI_STOP %lu\n", e);
    e = mciSendCommandA(op.wDeviceID, MCI_CLOSE, 0, 0);
    printf("MCI_CLOSE %lu\n", e);
    e = mciSendCommandA(op.wDeviceID, MCI_STATUS, MCI_STATUS_ITEM, (DWORD_PTR)&st);
    printf("MCI_STATUS after close %lu\n", e);
    e = mciSendCommandA(0, MCI_OPEN, MCI_OPEN_TYPE | MCI_OPEN_ELEMENT, 0);
    printf("MCI_OPEN null %lu\n", e);

    /* MCIWnd */
    HWND mw = MCIWndCreateA(hwnd, wc.hInstance, WS_CHILD | WS_VISIBLE | MCIWNDF_NOPLAYBAR | MCIWNDF_NOMENU | MCIWNDF_NOTIFYMODE, NULL);
    printf("MCIWndCreate %s\n", mw ? "window" : "null");
    if (mw) {
        char cls[64] = "";
        GetClassNameA(mw, cls, sizeof cls);
        printf("class %s, parent %s\n", cls, GetParent(mw) == hwnd ? "ours" : "other");
        printf("CAN_PLAY before %ld\n", (long)SendMessageA(mw, MCIWNDM_CAN_PLAY, 0, 0));
        LRESULT r = SendMessageA(mw, MCIWNDM_OPENA, 0, (LPARAM)avi);
        printf("MCIWNDM_OPEN %ld\n", (long)r);
        RECT rc;
        GetClientRect(mw, &rc);
        printf("client %ldx%ld\n", rc.right, rc.bottom);
        printf("CAN_PLAY %ld CAN_WINDOW %ld\n", (long)SendMessageA(mw, MCIWNDM_CAN_PLAY, 0, 0), (long)SendMessageA(mw, MCIWNDM_CAN_WINDOW, 0, 0));
        printf("GETLENGTH %ld GETSTART %ld\n", (long)SendMessageA(mw, MCIWNDM_GETLENGTH, 0, 0), (long)SendMessageA(mw, MCIWNDM_GETSTART, 0, 0));
        char m[64] = "";
        LRESULT md = SendMessageA(mw, MCIWNDM_GETMODEA, sizeof m, (LPARAM)m);
        printf("GETMODE %ld \"%s\"\n", (long)md, m);
        pump(50);
        nmodes = 0;
        r = SendMessageA(mw, MCIWNDM_PLAYFROM, 0, 0);
        printf("MCIWNDM_PLAYFROM %ld\n", (long)r);
        printf("GETMODE playing %ld\n", (long)SendMessageA(mw, MCIWNDM_GETMODEA, 0, 0));
        DWORD end = GetTickCount() + 5000;
        while ((LONG)(end - GetTickCount()) > 0 && !(nmodes >= 2 && modes[nmodes - 1] == MCI_MODE_STOP)) pump(20);
        printf("modes");
        for (int i = 0; i < nmodes; i++) printf(" %d", modes[i]);
        printf("\n");
        printf("GETPOSITION at the end %ld\n", (long)SendMessageA(mw, MCIWNDM_GETPOSITIONA, 0, 0));
        MCIWndDestroy(mw);
        printf("destroyed %s\n", IsWindow(mw) ? "no" : "yes");
    }
    DestroyWindow(hwnd);
    DeleteFileA(wav);
    DeleteFileA(bare);
    DeleteFileA(avi);
    DeleteFileA(vnv);
    return 0;
}
