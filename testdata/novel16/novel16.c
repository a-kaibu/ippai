/* novel16: the Win16 benchmark's game (tests/bench, group "win16"): a small visual novel engine
 * as Windows 3.1 games were written, a 16-bit program (large model, Open Watcom) drawing with GDI
 * only: 256-colour DIBs (StretchDIBits), masked sprites (SRCAND + SRCPAINT), transitions made of
 * raster operations (an ordered-dither dissolve through pattern brushes, a wipe, blinds, a mosaic
 * through StretchBlt), Shift_JIS text with TextOut, voices with sndPlaySound. It plays
 * script.txt (tests/bench/pack-novel16.py: the KiriKiri game's story, pictures and voices) and
 * says where it is on its standard output (DOS handle 1), as the other games do:
 *
 *   BENCH-STATE <what> <n>   something was shown (title, scene, line, saved, restored, end, fx_*)
 *   BENCH-WAIT <n> <what>    waiting for a click (click, idle, choice: two buttons)
 *   BENCH w16_petals_frame <ms>  the mean frame of 60 frames of 48 moving sprites
 *
 * "load" on the command line restores save.dat and stops there (restored).
 *
 * Build: tests/bench/build-novel16.sh (wcl -bt=windows -ml) */
#include <windows.h>
#include <mmsystem.h>
#include <stdlib.h>
#include <string.h>

#define W 1280
#define H 720
#define TX 40      /* the text window */
#define TY 520
#define TW 1200
#define TH 180

static HWND wnd;
static HDC scene_dc, back_dc, new_dc, src_dc, small_dc;
static HBITMAP scene_bm, back_bm, new_bm, small_bm;
static HFONT font;
static int clicked, click_x, click_y, quit, step, textwin = 1;
static char bg_name[16], ch_name[3][16];
static char text[512];
static int text_len;
static const int ch_x[3] = { 40, 440, 840 };

/* ---- output ---- */
static void out(const char *s)
{
    _lwrite(1, (LPCSTR)s, lstrlen(s));
}
static void state(const char *what)
{
    char b[80];
    wsprintf(b, "BENCH-STATE %s %d\n", (LPSTR)what, ++step);
    out(b);
}
static void bench_wait(const char *what)
{
    char b[80];
    wsprintf(b, "BENCH-WAIT %d %s\n", ++step, (LPSTR)what);
    out(b);
}
static void fail(const char *what)
{
    char b[160];
    wsprintf(b, "BENCH-FAIL %s\n", (LPSTR)what);
    out(b);
    quit = 1;
}

/* ---- messages ---- */
static void present(void)
{
    HDC dc = GetDC(wnd);
    BitBlt(dc, 0, 0, W, H, back_dc, 0, 0, SRCCOPY);
    ReleaseDC(wnd, dc);
}

long FAR PASCAL __export wnd_proc(HWND h, UINT m, WPARAM wp, LPARAM lp)
{
    PAINTSTRUCT ps;
    switch (m) {
    case WM_LBUTTONDOWN:
        clicked = 1;
        click_x = LOWORD(lp);
        click_y = HIWORD(lp);
        return 0;
    case WM_PAINT:
        BeginPaint(h, &ps);
        if (back_dc) BitBlt(ps.hdc, 0, 0, W, H, back_dc, 0, 0, SRCCOPY);
        EndPaint(h, &ps);
        return 0;
    case WM_DESTROY:
        quit = 1;
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProc(h, m, wp, lp);
}

static void pump_one(int block)
{
    MSG msg;
    if (block) {
        if (!GetMessage(&msg, NULL, 0, 0)) { quit = 1; return; }
    } else if (!PeekMessage(&msg, NULL, 0, 0, PM_REMOVE)) {
        return;
    } else if (msg.message == WM_QUIT) {
        quit = 1;
        return;
    }
    TranslateMessage(&msg);
    DispatchMessage(&msg);
}

/* sleeps on a timer, handling messages meanwhile */
static void sleep_ms(UINT ms)
{
    DWORD end = timeGetTime() + ms;
    SetTimer(wnd, 1, ms ? ms : 1, NULL);
    while (!quit && (long)(end - timeGetTime()) > 0) pump_one(1);
    KillTimer(wnd, 1);
    while (!quit) {  /* and what came meanwhile */
        MSG msg;
        if (!PeekMessage(&msg, NULL, 0, 0, PM_NOREMOVE)) break;
        pump_one(0);
    }
}

static void wait_click(void)
{
    clicked = 0;
    while (!quit && !clicked) pump_one(1);
}

/* ---- pictures ---- */
/* a .bmp file (8-bit) as a W-wide picture: drawn twice its size into a new bitmap of dc's kind */
static HBITMAP load_bmp(const char *name, int *pw, int *ph)
{
    char path[80];
    HFILE f;
    long size;
    HGLOBAL g;
    char __huge *buf;
    BITMAPINFO FAR *bi;
    HBITMAP bm, old;
    int w, h;
    DWORD off;
    wsprintf(path, "%s.bmp", (LPSTR)name);
    f = _lopen(path, OF_READ);
    if (f == HFILE_ERROR) { fail(path); return 0; }
    size = _llseek(f, 0, 2);
    _llseek(f, 0, 0);
    g = GlobalAlloc(GMEM_MOVEABLE, size);
    buf = (char __huge *)GlobalLock(g);
    _hread(f, buf, size);
    _lclose(f);
    bi = (BITMAPINFO FAR *)(buf + sizeof(BITMAPFILEHEADER));
    off = ((BITMAPFILEHEADER FAR *)buf)->bfOffBits;
    w = (int)bi->bmiHeader.biWidth;
    h = (int)bi->bmiHeader.biHeight;
    bm = CreateCompatibleBitmap(back_dc, w * 2, h * 2);
    old = SelectObject(src_dc, bm);
    SetStretchBltMode(src_dc, COLORONCOLOR);
    StretchDIBits(src_dc, 0, 0, w * 2, h * 2, 0, 0, w, h, (void FAR *)(buf + off), bi, DIB_RGB_COLORS, SRCCOPY);
    SelectObject(src_dc, old);
    GlobalUnlock(g);
    GlobalFree(g);
    if (pw) *pw = w * 2;
    if (ph) *ph = h * 2;
    return bm;
}

/* loaded pictures by name (a sprite's mask is <name>m.bmp) */
typedef struct { char name[16]; HBITMAP bm, mask; int w, h; } Pic;
static Pic pics[32];
static int npics;

static Pic *pic(const char *name, int sprite)
{
    int i;
    char m[24];
    for (i = 0; i < npics; i++)
        if (!lstrcmp(pics[i].name, name)) return &pics[i];
    if (npics == 32) { fail("too many pictures"); return &pics[0]; }
    lstrcpy(pics[npics].name, name);
    pics[npics].bm = load_bmp(name, &pics[npics].w, &pics[npics].h);
    if (sprite) {
        wsprintf(m, "%sm", (LPSTR)name);
        pics[npics].mask = load_bmp(m, NULL, NULL);
    }
    return &pics[npics++];
}

static void blit(HDC dst, int x, int y, HBITMAP bm, int w, int h, DWORD rop)
{
    HBITMAP old = SelectObject(src_dc, bm);
    BitBlt(dst, x, y, w, h, src_dc, 0, 0, rop);
    SelectObject(src_dc, old);
}

static void sprite(HDC dst, Pic *p, int x, int y)
{
    blit(dst, x, y, p->mask, p->w, p->h, SRCAND);
    blit(dst, x, y, p->bm, p->w, p->h, SRCPAINT);
}

/* the background and the characters into dc */
static void compose(HDC dc)
{
    int i;
    if (bg_name[0]) {
        Pic *p = pic(bg_name, 0);
        blit(dc, 0, 0, p->bm, W, H, SRCCOPY);
    } else {
        PatBlt(dc, 0, 0, W, H, BLACKNESS);
    }
    for (i = 0; i < 3; i++)
        if (ch_name[i][0]) sprite(dc, pic(ch_name[i], 1), ch_x[i], 80);
}

static int is_lead(unsigned char c)
{
    return (c >= 0x81 && c <= 0x9f) || (c >= 0xe0 && c <= 0xfc);
}

/* the scene, the text window and the first n bytes of the text into back_dc */
static void draw_text(int n)
{
    HBRUSH br;
    HPEN pen, oldp;
    HFONT oldf;
    RECT r;
    int i, line = 0, start = 0, col = 0;
    BitBlt(back_dc, 0, 0, W, H, scene_dc, 0, 0, SRCCOPY);
    if (!textwin) return;
    br = CreateSolidBrush(RGB(32, 32, 64));
    pen = CreatePen(PS_SOLID, 2, RGB(200, 200, 230));
    SetRect(&r, TX, TY, TX + TW, TY + TH);
    FillRect(back_dc, &r, br);
    oldp = SelectObject(back_dc, pen);
    MoveTo(back_dc, TX, TY);
    LineTo(back_dc, TX + TW, TY);
    SelectObject(back_dc, oldp);
    DeleteObject(pen);
    DeleteObject(br);
    oldf = SelectObject(back_dc, font);
    SetBkMode(back_dc, TRANSPARENT);
    SetTextColor(back_dc, RGB(255, 255, 255));
    /* 40 full-width characters to a line */
    for (i = 0; i <= n; i++) {
        if (i == n || col >= 80) {
            TextOut(back_dc, TX + 24, TY + 20 + line * 38, text + start, i - start);
            line++;
            start = i;
            col = 0;
            if (i == n) break;
        }
        if (is_lead((unsigned char)text[i]) && i + 1 < n) { i++; col += 2; } else col++;
    }
    SelectObject(back_dc, oldf);
}

/* ---- transitions: from back_dc's picture to new_dc's ---- */
static void dissolve(UINT ms)
{
    static const BYTE bayer[4][4] = { { 0, 8, 2, 10 }, { 12, 4, 14, 6 }, { 3, 11, 1, 9 }, { 15, 7, 13, 5 } };
    int k, x, y;
    for (k = 1; k <= 16 && !quit; k++) {
        WORD bits[8];
        HBITMAP pat;
        HBRUSH br, old;
        for (y = 0; y < 8; y++) {
            bits[y] = 0;
            for (x = 0; x < 8; x++)
                if (bayer[y & 3][x & 3] < k) bits[y] |= 0x80 >> x;
        }
        pat = CreateBitmap(8, 8, 1, 1, bits);
        br = CreatePatternBrush(pat);
        old = SelectObject(back_dc, br);
        SetTextColor(back_dc, RGB(0, 0, 0));
        SetBkColor(back_dc, RGB(255, 255, 255));
        BitBlt(back_dc, 0, 0, W, H, new_dc, 0, 0, 0x00E20746L); /* DSPDxax: S where P */
        SelectObject(back_dc, old);
        DeleteObject(br);
        DeleteObject(pat);
        present();
        sleep_ms(ms / 16);
    }
}

static void wipe(UINT ms)
{
    int k;
    for (k = 1; k <= 32 && !quit; k++) {
        int x = W * k / 32;
        BitBlt(back_dc, 0, 0, x, H, new_dc, 0, 0, SRCCOPY);
        present();
        sleep_ms(ms / 32);
    }
}

static void blinds(UINT ms)
{
    int k, y;
    for (k = 1; k <= 24 && !quit; k++) {
        for (y = 0; y < H; y += 24)
            BitBlt(back_dc, 0, y, W, k, new_dc, 0, y, SRCCOPY);
        present();
        sleep_ms(ms / 24);
    }
}

/* blocks growing over the old picture, then shrinking over the new one */
static void mosaic(UINT ms)
{
    static const int blocks[] = { 2, 4, 8, 16, 32, 32, 16, 8, 4, 2 };
    int k;
    SetStretchBltMode(small_dc, COLORONCOLOR);
    SetStretchBltMode(back_dc, COLORONCOLOR);
    for (k = 0; k < 10 && !quit; k++) {
        int b = blocks[k];
        HDC from = k < 5 ? back_dc : new_dc;
        StretchBlt(small_dc, 0, 0, W / b, H / b, from, 0, 0, W, H, SRCCOPY);
        StretchBlt(back_dc, 0, 0, W, H, small_dc, 0, 0, W / b, H / b, SRCCOPY);
        present();
        sleep_ms(ms / 10);
    }
}

static void show(const char *how, UINT ms)
{
    compose(scene_dc);
    /* the new picture: the scene with the text window as it is */
    BitBlt(new_dc, 0, 0, W, H, scene_dc, 0, 0, SRCCOPY);
    if (!lstrcmp(how, "fade")) dissolve(ms);
    else if (!lstrcmp(how, "wipe")) wipe(ms);
    else if (!lstrcmp(how, "blinds")) blinds(ms);
    else if (!lstrcmp(how, "mosaic")) mosaic(ms);
    text_len = 0;
    text[0] = 0;
    draw_text(0);
    present();
}

/* ---- the moving parts ---- */
static void slide(int slot, const char *name)
{
    int k;
    Pic *p = pic(name, 1);
    ch_name[slot][0] = 0;
    compose(scene_dc);
    for (k = 0; k <= 30 && !quit; k++) {
        BitBlt(back_dc, 0, 0, W, H, scene_dc, 0, 0, SRCCOPY);
        sprite(back_dc, p, -400 + k * (ch_x[slot] + 400) / 30, 80);
        present();
        sleep_ms(16);
    }
    lstrcpy(ch_name[slot], name);
    compose(scene_dc);
}

static void shake(UINT ms)
{
    DWORD end = timeGetTime() + ms;
    int k = 0;
    HDC dc;
    while (!quit && (long)(end - timeGetTime()) > 0) {
        int dx = (k & 1) ? 8 : -8, dy = (k & 2) ? 6 : -6;
        dc = GetDC(wnd);
        BitBlt(dc, dx, dy, W, H, back_dc, 0, 0, SRCCOPY);
        ReleaseDC(wnd, dc);
        k++;
        sleep_ms(16);
    }
    present();
}

/* sine in 1/255ths, from a table a degree a step */
static int isin(int deg)
{
    static const BYTE q[91] = { 0, 4, 9, 13, 18, 22, 27, 31, 35, 40, 44, 49, 53, 57, 62, 66, 70, 75, 79, 83, 87, 91,
        96, 100, 104, 108, 112, 116, 120, 124, 127, 131, 135, 139, 143, 146, 150, 153, 157, 160, 164, 167, 171, 174, 177,
        180, 183, 186, 190, 192, 195, 198, 201, 204, 206, 209, 211, 214, 216, 219, 221, 223, 225, 227, 229, 231, 233, 235,
        236, 238, 240, 241, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 253, 254, 254, 254, 255, 255, 255, 255 };
    deg %= 360;
    if (deg < 0) deg += 360;
    if (deg <= 90) return q[deg];
    if (deg <= 180) return q[180 - deg];
    if (deg <= 270) return -q[deg - 180];
    return -q[360 - deg];
}

static void petals(void)
{
    Pic *p = pic("petal", 1);
    int f, i;
    DWORD t0 = timeGetTime(), t;
    char b[64];
    for (f = 1; f <= 60 && !quit; f++) {
        draw_text(text_len);
        for (i = 0; i < 48; i++) {
            int x = i * 27 + isin(f * 6 + i * 30) * 40 / 255;
            int y = (f * 11 + i * 37) % 800 - 64;
            sprite(back_dc, p, x, y);
        }
        present();
        pump_one(0);
    }
    t = timeGetTime() - t0;
    wsprintf(b, "BENCH w16_petals_frame %ld.%ld\n", t / 60, (t * 10 / 60) % 10);
    out(b);
    draw_text(text_len);
    present();
}

static int choice(const char *a, const char *b2)
{
    static const RECT r[2] = { { 400, 400, 880, 460 }, { 400, 520, 880, 580 } };
    const char *label[2];
    HBRUSH br = CreateSolidBrush(RGB(40, 40, 90));
    HFONT oldf;
    int i;
    label[0] = a;
    label[1] = b2;
    BitBlt(back_dc, 0, 0, W, H, scene_dc, 0, 0, SRCCOPY);
    oldf = SelectObject(back_dc, font);
    SetBkMode(back_dc, TRANSPARENT);
    SetTextColor(back_dc, RGB(255, 255, 255));
    for (i = 0; i < 2; i++) {
        FillRect(back_dc, &r[i], br);
        TextOut(back_dc, r[i].left + 140, r[i].top + 14, label[i], lstrlen(label[i]));
    }
    SelectObject(back_dc, oldf);
    DeleteObject(br);
    present();
    state("fx_choice");
    bench_wait("choice");
    for (;;) {
        POINT pt;
        wait_click();
        if (quit) return 0;
        pt.x = click_x;
        pt.y = click_y;
        for (i = 0; i < 2; i++)
            if (PtInRect(&r[i], pt)) return i;
    }
}

/* ---- saves ---- */
static void save(int line)
{
    HFILE f = _lcreat("save.dat", 0);
    char b[80];
    if (f == HFILE_ERROR) { fail("save.dat"); return; }
    wsprintf(b, "%d %s %s %s %s\n", line, (LPSTR)(bg_name[0] ? bg_name : "-"), (LPSTR)(ch_name[0][0] ? ch_name[0] : "-"),
             (LPSTR)(ch_name[1][0] ? ch_name[1] : "-"), (LPSTR)(ch_name[2][0] ? ch_name[2] : "-"));
    _lwrite(f, b, lstrlen(b));
    _lclose(f);
}

/* the next space-separated word of *s into w */
static void word(char FAR **s, char *w, int n)
{
    char FAR *p = *s;
    int i = 0;
    while (*p == ' ') p++;
    while ((unsigned char)*p > ' ' && i < n - 1) w[i++] = *p++;
    w[i] = 0;
    *s = p;
}

static int restore(void)
{
    HFILE f = _lopen("save.dat", OF_READ);
    char b[80], FAR *p = b;
    char w[16];
    int i, n;
    if (f == HFILE_ERROR) return 0;
    n = _lread(f, b, sizeof(b) - 1);
    _lclose(f);
    b[n < 0 ? 0 : n] = 0;
    word(&p, w, sizeof(w));
    word(&p, w, sizeof(w));
    lstrcpy(bg_name, lstrcmp(w, "-") ? w : "");
    for (i = 0; i < 3; i++) {
        word(&p, w, sizeof(w));
        lstrcpy(ch_name[i], lstrcmp(w, "-") ? w : "");
    }
    return 1;
}

/* ---- the script ---- */
static char FAR *script;
static char FAR *lines[400];
static int nlines;

static void load_script(void)
{
    HFILE f = _lopen("script.txt", OF_READ);
    long size;
    char FAR *p;
    if (f == HFILE_ERROR) { fail("no script.txt"); return; }
    size = _llseek(f, 0, 2);
    _llseek(f, 0, 0);
    script = (char FAR *)GlobalLock(GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size + 1));
    _lread(f, script, (UINT)size);
    _lclose(f);
    for (p = script; *p && nlines < 400;) {
        lines[nlines++] = p;
        while (*p && *p != '\r' && *p != '\n') p++;
        while (*p == '\r' || *p == '\n') *p++ = 0;
    }
}

static int find_label(const char FAR *name)
{
    int i;
    for (i = 0; i < nlines; i++)
        if (lines[i][0] == '*' && !lstrcmp(lines[i] + 1, name)) return i;
    fail("no such label");
    return nlines;
}

static void say(const char FAR *t, int ms)
{
    int n;
    lstrcpyn(text, t, sizeof(text));
    n = lstrlen(text);
    for (text_len = 0; text_len < n && !quit;) {
        text_len += is_lead((unsigned char)text[text_len]) ? 2 : 1;
        draw_text(text_len);
        present();
        sleep_ms(ms);
    }
}

static void run(int load_mode)
{
    int pc = 0, ms = 30;
    char cmd[16], a[32], b[32];
    if (load_mode) {
        if (!restore()) { fail("no save to load"); return; }
        show("cut", 0);
        say("\x81\x69\x83\x8d\x81\x5b\x83\x68\x82\xb5\x82\xdc\x82\xb5\x82\xbd\x81\x6a", 0); /* （ロードしました） */
        state("restored");
        out("BENCH-END\n");
        return;
    }
    while (pc < nlines && !quit) {
        char FAR *s = lines[pc++];
        if (!*s || *s == ';' || *s == '*') continue;
        word(&s, cmd, sizeof(cmd));
        while (*s == ' ') s++;
        if (!lstrcmp(cmd, "bg")) {          /* bg NAME|-: drawn by the next show */
            lstrcpy(bg_name, lstrcmp(s, "-") ? s : "");
        } else if (!lstrcmp(cmd, "show")) { /* show cut|fade|wipe|blinds|mosaic MS */
            word(&s, a, sizeof(a));
            show(a, (UINT)atoi(s));
        } else if (!lstrcmp(cmd, "ch")) {   /* ch SLOT NAME|- */
            word(&s, a, sizeof(a));
            word(&s, b, sizeof(b));
            lstrcpy(ch_name[a[0] - '0'], lstrcmp(b, "-") ? b : "");
        } else if (!lstrcmp(cmd, "voice")) {
            char path[40];
            wsprintf(path, "sound\\%s.wav", (LPSTR)s);
            sndPlaySound(path, SND_ASYNC | SND_NODEFAULT);
        } else if (!lstrcmp(cmd, "speed")) {
            ms = atoi(s);
        } else if (!lstrcmp(cmd, "say")) {
            say(s, ms);
        } else if (!lstrcmp(cmd, "textwin")) {
            textwin = !lstrcmp(s, "on");
            draw_text(text_len);
            present();
        } else if (!lstrcmp(cmd, "st")) {
            state(s);
        } else if (!lstrcmp(cmd, "bw")) {
            bench_wait(s);
            wait_click();
        } else if (!lstrcmp(cmd, "slide")) {
            word(&s, a, sizeof(a));
            word(&s, b, sizeof(b));
            slide(a[0] - '0', b);
        } else if (!lstrcmp(cmd, "shake")) {
            shake((UINT)atoi(s));
        } else if (!lstrcmp(cmd, "petals")) {
            petals();
        } else if (!lstrcmp(cmd, "choice")) { /* choice LABEL2 first|second: jumps to LABEL2 on the second */
            char FAR *x = lines[pc++];
            char FAR *y = lines[pc++];
            word(&s, a, sizeof(a));
            if (choice(x, y) == 1) pc = find_label(a);
        } else if (!lstrcmp(cmd, "goto")) {
            pc = find_label(s);
        } else if (!lstrcmp(cmd, "save")) {
            save(pc);
        } else if (!lstrcmp(cmd, "end")) {
            out("BENCH-END\n");
            return;
        } else {
            fail("unknown command");
        }
    }
}

int PASCAL WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmdline, int show_cmd)
{
    WNDCLASS wc;
    HDC dc;
    (void)show_cmd;
    if (!prev) {
        memset(&wc, 0, sizeof(wc));
        wc.lpfnWndProc = wnd_proc;
        wc.hInstance = inst;
        wc.hCursor = LoadCursor(NULL, IDC_ARROW);
        wc.hbrBackground = GetStockObject(BLACK_BRUSH);
        wc.lpszClassName = "novel16";
        RegisterClass(&wc);
    }
    wnd = CreateWindow("novel16", "novel16", WS_POPUP | WS_VISIBLE, 0, 0, W, H, NULL, NULL, inst, NULL);
    dc = GetDC(wnd);
    scene_dc = CreateCompatibleDC(dc);
    back_dc = CreateCompatibleDC(dc);
    new_dc = CreateCompatibleDC(dc);
    src_dc = CreateCompatibleDC(dc);
    small_dc = CreateCompatibleDC(dc);
    scene_bm = CreateCompatibleBitmap(dc, W, H);
    back_bm = CreateCompatibleBitmap(dc, W, H);
    new_bm = CreateCompatibleBitmap(dc, W, H);
    small_bm = CreateCompatibleBitmap(dc, W / 2, H / 2);
    SelectObject(scene_dc, scene_bm);
    SelectObject(back_dc, back_bm);
    SelectObject(new_dc, new_bm);
    SelectObject(small_dc, small_bm);
    ReleaseDC(wnd, dc);
    /* ＭＳ ゴシック, 28 px */
    font = CreateFont(-28, 0, 0, 0, FW_NORMAL, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0, 0, FIXED_PITCH,
                      "\x82\x6c\x82\x72 \x83\x53\x83\x56\x83\x62\x83\x4e");
    PatBlt(back_dc, 0, 0, W, H, BLACKNESS);
    ShowWindow(wnd, SW_SHOW);
    UpdateWindow(wnd);
    load_script();
    if (!quit) run(cmdline && !lstrcmp(cmdline, "load"));
    while (!quit) pump_one(1);
    return 0;
}
