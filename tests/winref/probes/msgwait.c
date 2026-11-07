/* Waits on messages and handles together, and the window manager pieces games' launchers lean
 * on: MsgWaitForMultipleObjects(Ex)'s wake mask (d7b4a8a), the thread pool's QueueUserWorkItem
 * (fabaab4), DDE string handles (be58f13), FindWindowEx (c66357d) and IsDialogMessage turning
 * Enter/Escape into IDOK/IDCANCEL (dcb9e8f, 5c42c3f). */
#include "../probe.h"
#include <ddeml.h>

static LONG worked;
static HANDLE done;
static DWORD WINAPI work(void *arg) {
    InterlockedAdd(&worked, (LONG)(INT_PTR)arg);
    InterlockedIncrement(&worked);
    SetEvent(done);
    return 0;
}

static const char *wait_name(DWORD r) {
    static char b[32];
    if (r == WAIT_TIMEOUT) return "timeout";
    if (r == WAIT_FAILED) return "failed";
    sprintf(b, "%lu", r);
    return b;
}

static void drain(void) {
    MSG m;
    while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) {}
}

static int commands[8], ncommands;
static INT_PTR CALLBACK dlg_proc(HWND d, UINT msg, WPARAM wp, LPARAM lp) {
    if (msg == WM_INITDIALOG) return TRUE;
    if (msg == WM_COMMAND && ncommands < 8) {
        commands[ncommands++] = LOWORD(wp);
        return TRUE;
    }
    return FALSE;
}

/* an in-memory dialog: two push buttons (OK default, Cancel) and an edit */
static void *dialog_template(void) {
    static WORD t[512];
    WORD *p = t;
    DLGTEMPLATE *d = (DLGTEMPLATE *)p;
    d->style = WS_POPUP | DS_SETFONT | DS_MODALFRAME;
    d->cdit = 3;
    d->cx = 200, d->cy = 100;
    p = (WORD *)(d + 1);
    *p++ = 0, *p++ = 0; /* menu, class */
    *p++ = 0; /* title */
    *p++ = 8;
    for (const char *f = "MS Shell Dlg"; (*p++ = *f); f++) {}
    struct { DWORD style; short x, y, cx, cy; WORD id; WORD cls; } items[3] = {
        { WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON, 10, 70, 50, 14, IDOK, 0x80 },
        { WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON, 70, 70, 50, 14, IDCANCEL, 0x80 },
        { WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL | WS_BORDER, 10, 10, 100, 14, 100, 0x81 },
    };
    for (int i = 0; i < 3; i++) {
        p = (WORD *)(((ULONG_PTR)p + 3) & ~3);
        DLGITEMTEMPLATE *it = (DLGITEMTEMPLATE *)p;
        it->style = items[i].style;
        it->dwExtendedStyle = 0;
        it->x = items[i].x, it->y = items[i].y, it->cx = items[i].cx, it->cy = items[i].cy;
        it->id = items[i].id;
        p = (WORD *)(it + 1);
        *p++ = 0xffff, *p++ = items[i].cls;
        *p++ = 0; /* text */
        *p++ = 0; /* creation data */
    }
    return t;
}

static void key(HWND to, UINT vk) {
    PostMessageA(to, WM_KEYDOWN, vk, 1);
    PostMessageA(to, WM_KEYUP, vk, 0xc0000001);
}
static void pump_dialog(HWND d) {
    MSG m;
    while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE))
        if (!IsDialogMessageA(d, &m)) {
            TranslateMessage(&m);
            DispatchMessageA(&m);
        }
}

static HDDEDATA CALLBACK dde_cb(UINT t, UINT f, HCONV c, HSZ a, HSZ b, HDDEDATA d, ULONG_PTR x, ULONG_PTR y) { return 0; }

int main(void) {
    /* wake masks */
    HANDLE ev = CreateEventA(NULL, TRUE, FALSE, NULL);
    drain();
    DWORD r = MsgWaitForMultipleObjects(1, &ev, FALSE, 0, QS_ALLINPUT);
    printf("nothing queued: %s\n", wait_name(r));
    PostThreadMessageA(GetCurrentThreadId(), WM_USER, 0, 0);
    printf("posted, mask QS_TIMER: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 50, QS_TIMER)));
    printf("posted, mask QS_KEY: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 50, QS_KEY)));
    printf("posted, mask QS_POSTMESSAGE: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 50, QS_POSTMESSAGE)));
    /* the message was seen (not removed): without MWMO_INPUTAVAILABLE a second wait does not wake */
    printf("posted again, same mask: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 50, QS_POSTMESSAGE)));
    printf("Ex MWMO_INPUTAVAILABLE: %s\n", wait_name(MsgWaitForMultipleObjectsEx(1, &ev, 50, QS_POSTMESSAGE, MWMO_INPUTAVAILABLE)));
    printf("GetQueueStatus post: %08lx\n", GetQueueStatus(QS_POSTMESSAGE));
    drain();
    printf("drained, QS_ALLINPUT: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 20, QS_ALLINPUT)));
    SetEvent(ev);
    PostThreadMessageA(GetCurrentThreadId(), WM_USER, 0, 0);
    printf("event set and posted: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 0, QS_ALLINPUT)));
    drain();
    printf("event set, zero handles: %s\n", wait_name(MsgWaitForMultipleObjects(0, NULL, FALSE, 20, QS_ALLINPUT)));
    ResetEvent(ev);
    HANDLE two[2] = { CreateEventA(NULL, FALSE, FALSE, NULL), ev };
    SetEvent(two[0]);
    printf("wait all, one set: %s\n", wait_name(MsgWaitForMultipleObjects(2, two, TRUE, 20, QS_ALLINPUT)));
    SetEvent(ev);
    r = MsgWaitForMultipleObjects(2, two, TRUE, 20, QS_ALLINPUT);
    printf("wait all, both set: %s", wait_name(r));
    printf(" auto-reset now %s\n", wait_name(WaitForSingleObject(two[0], 0)));
    /* a timer message (QS_TIMER) */
    UINT_PTR t = SetTimer(NULL, 0, 10, NULL);
    printf("timer, mask QS_TIMER: %s\n", wait_name(MsgWaitForMultipleObjects(0, NULL, FALSE, 2000, QS_TIMER)));
    KillTimer(NULL, t);
    drain();
    SetLastError(0xdead);
    r = MsgWaitForMultipleObjects(1, (HANDLE[]){ (HANDLE)0x1234 }, FALSE, 0, QS_ALLINPUT);
    printf("bad handle: %s err %lu\n", wait_name(r), GetLastError());

    /* the thread pool */
    done = CreateEventA(NULL, FALSE, FALSE, NULL);
    worked = -1000;
    printf("QueueUserWorkItem: %d\n", QueueUserWorkItem(work, (void *)(INT_PTR)1000, WT_EXECUTEDEFAULT) != 0);
    r = WaitForSingleObject(done, 5000);
    printf("work item ran: %s value %ld\n", wait_name(r), worked);
    worked = -1;
    QueueUserWorkItem(work, 0, WT_EXECUTELONGFUNCTION);
    printf("long work item ran: %s\n", wait_name(WaitForSingleObject(done, 5000)));

    /* DDE string handles */
    DWORD inst = 0;
    UINT dr = DdeInitializeA(&inst, dde_cb, APPCMD_CLIENTONLY, 0);
    printf("DdeInitialize: %u inst %d\n", dr, inst != 0);
    HSZ a = DdeCreateStringHandleA(inst, "Sake Topic", CP_WINANSI), b = DdeCreateStringHandleA(inst, "sake topic", CP_WINANSI),
        c = DdeCreateStringHandleA(inst, "Other", CP_WINANSI), w = DdeCreateStringHandleW(inst, L"Wide Name", CP_WINUNICODE);
    char s[64];
    memset(s, 'x', sizeof s);
    DWORD n = DdeQueryStringA(inst, a, s, sizeof s, CP_WINANSI);
    printf("DdeQueryString: %lu %s\n", n, s);
    printf("DdeQueryString length only: %lu\n", DdeQueryStringA(inst, a, NULL, 0, CP_WINANSI));
    memset(s, 'x', sizeof s);
    n = DdeQueryStringA(inst, a, s, 5, CP_WINANSI);
    printf("DdeQueryString 5: %lu %.8s\n", n, s);
    WCHAR ws[64];
    n = DdeQueryStringW(inst, w, ws, 64, CP_WINUNICODE);
    printf("DdeQueryStringW: %lu %ls\n", n, ws);
    n = DdeQueryStringA(inst, w, s, 64, CP_WINANSI);
    printf("DdeQueryStringA of a wide one: %lu %s\n", n, s);
    printf("DdeCmpStringHandles same text other case %d, other %d %d, null %d %d\n", DdeCmpStringHandles(a, b), DdeCmpStringHandles(a, c),
           DdeCmpStringHandles(c, a), DdeCmpStringHandles(0, a), DdeCmpStringHandles(0, 0));
    printf("same handle for other case: %d\n", a == b);
    BOOL fa = DdeFreeStringHandle(inst, a);
    BOOL fb = DdeFreeStringHandle(inst, b);
    BOOL fc = DdeFreeStringHandle(inst, c);
    printf("DdeFreeStringHandle %d %d %d\n", fa, fb, fc);
    printf("DdeUninitialize %d\n", DdeUninitialize(inst));

    /* FindWindowEx */
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = DefWindowProcA;
    wc.lpszClassName = "SakeProbeParent";
    RegisterClassA(&wc);
    wc.lpszClassName = "SakeProbeChild";
    RegisterClassA(&wc);
    HWND top = CreateWindowExA(0, "SakeProbeParent", "probe top", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, NULL, NULL, NULL);
    HWND k1 = CreateWindowExA(0, "SakeProbeChild", "one", WS_CHILD, 0, 0, 10, 10, top, NULL, NULL, NULL);
    HWND k2 = CreateWindowExA(0, "SakeProbeChild", "two", WS_CHILD, 0, 0, 10, 10, top, NULL, NULL, NULL);
    HWND k3 = CreateWindowExA(0, "STATIC", "three", WS_CHILD, 0, 0, 10, 10, top, NULL, NULL, NULL);
    printf("FindWindow class %d title %d both %d wrong title %d\n", FindWindowA("SakeProbeParent", NULL) == top, FindWindowA(NULL, "probe top") == top,
           FindWindowA("SakeProbeParent", "probe top") == top, FindWindowA("SakeProbeParent", "nope") == NULL);
    printf("FindWindowEx first child of class %d next %d after last %d\n", FindWindowExA(top, NULL, "SakeProbeChild", NULL) == k1,
           FindWindowExA(top, k1, "SakeProbeChild", NULL) == k2, FindWindowExA(top, k2, "SakeProbeChild", NULL) == NULL);
    printf("FindWindowEx by title %d any %d static %d\n", FindWindowExA(top, NULL, NULL, "two") == k2, FindWindowExA(top, NULL, NULL, NULL) == k1,
           FindWindowExA(top, NULL, "Static", NULL) == k3);
    printf("FindWindowExW %d top level %d\n", FindWindowExW(top, NULL, L"SakeProbeChild", L"two") == k2, FindWindowExA(NULL, NULL, "SakeProbeParent", NULL) == top);
    printf("FindWindowEx child of child %d\n", FindWindowExA(k1, NULL, NULL, NULL) == NULL);
    DestroyWindow(top);

    /* IsDialogMessage: Enter and Escape */
    HWND d = CreateDialogIndirectParamA(NULL, dialog_template(), NULL, dlg_proc, 0);
    printf("dialog %d\n", d != NULL);
    if (d) {
        ShowWindow(d, SW_SHOW);
        HWND edit = GetDlgItem(d, 100), ok = GetDlgItem(d, IDOK), cancel = GetDlgItem(d, IDCANCEL);
        printf("default id %08lx\n", (unsigned long)SendMessageA(d, DM_GETDEFID, 0, 0));
        SetFocus(edit);
        drain();
        ncommands = 0;
        key(edit, VK_RETURN);
        pump_dialog(d);
        key(edit, VK_ESCAPE);
        pump_dialog(d);
        printf("enter, escape in the edit:");
        for (int i = 0; i < ncommands; i++) printf(" %d", commands[i]);
        printf("\n");
        ncommands = 0;
        SetFocus(cancel);
        drain();
        printf("focus on cancel %d\n", GetFocus() == cancel);
        key(cancel, VK_RETURN);
        pump_dialog(d);
        printf("enter on cancel:");
        for (int i = 0; i < ncommands; i++) printf(" %d", commands[i]);
        printf("\n");
        ncommands = 0;
        SendMessageA(d, DM_SETDEFID, IDCANCEL, 0);
        printf("default id after set %08lx\n", (unsigned long)SendMessageA(d, DM_GETDEFID, 0, 0));
        SetFocus(edit);
        drain();
        key(edit, VK_RETURN);
        pump_dialog(d);
        printf("enter with cancel default:");
        for (int i = 0; i < ncommands; i++) printf(" %d", commands[i]);
        printf("\n");
        /* a key the dialog does not take goes to the control */
        ncommands = 0;
        MSG m = { edit, WM_CHAR, 'a', 1 };
        printf("IsDialogMessage WM_CHAR: %d commands %d\n", IsDialogMessageA(d, &m), ncommands);
        char text[16] = "";
        GetWindowTextA(edit, text, sizeof text);
        printf("edit text \"%s\"\n", text);
        (void)ok;
        DestroyWindow(d);
    }
    return 0;
}
