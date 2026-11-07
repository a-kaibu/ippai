/* A movie sake has nothing to play for (no .vnv next to it) still ends: not inside Run but a
 * moment later, as CatSystem2 (ここから夏のイノセンス！ #48) needs: it stores its loop count
 * after Run returns and its movie thread acts on EC_COMPLETE from the notify message. Also: a
 * Pause + Run in between keeps the end coming, a Stop before the end drops it, Run after Stop
 * ends again, a rewound Run after the end plays on (a looping background movie).
 * libs: -lole32 -lstrmiids -luuid */
#include <stdio.h>
#include <windows.h>
#include <dshow.h>

#define WM_GRAPH (WM_APP + 1)

static IMediaControl *mc;
static IMediaEventEx *me;
static IMediaSeeking *ms;
static HWND w;
static HANDLE ev;

/* the notify messages within `ms`, and the EC_COMPLETEs GetEvent then hands out */
static void wait_end(const char *l, DWORD ms_) {
    DWORD t0 = GetTickCount();
    int msgs = 0, ends = 0;
    MSG m;
    while (GetTickCount() - t0 < ms_) {
        while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) {
            if (m.message == WM_GRAPH) {
                msgs++;
                long code;
                LONG_PTR a, b;
                while (me->lpVtbl->GetEvent(me, &code, &a, &b, 0) == S_OK) {
                    ends += code == EC_COMPLETE;
                    me->lpVtbl->FreeEventParams(me, code, a, b);
                }
            } else
                DispatchMessageA(&m);
        }
        MsgWaitForMultipleObjects(0, NULL, FALSE, 20, QS_ALLINPUT);
    }
    printf("%s: ends %d (notified %d)\n", l, ends, msgs > 0);
}

/* (EC_PAUSED comes as on Windows: only EC_COMPLETE counts) */
static void right_after_run(const char *l) {
    long code;
    LONG_PTR a, b;
    int ends = 0;
    while (me->lpVtbl->GetEvent(me, &code, &a, &b, 0) == S_OK) {
        ends += code == EC_COMPLETE;
        me->lpVtbl->FreeEventParams(me, code, a, b);
    }
    printf("%s: ends %d\n", l, ends);
}

int main(void) {
    CoInitialize(NULL);
    FILE *f = fopen("nothing.mpg", "wb");
    fputs("not a movie", f);
    fclose(f);
    WNDCLASSA wc = { 0 };
    wc.lpfnWndProc = DefWindowProcA;
    wc.lpszClassName = "dshowend";
    RegisterClassA(&wc);
    w = CreateWindowA("dshowend", "", WS_OVERLAPPEDWINDOW, 0, 0, 100, 100, NULL, NULL, NULL, NULL);
    IGraphBuilder *gb;
    if (CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&gb)) return 1;
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaControl, (void **)&mc);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaEventEx, (void **)&me);
    gb->lpVtbl->QueryInterface(gb, &IID_IMediaSeeking, (void **)&ms);
    printf("render %08lx\n", (unsigned long)gb->lpVtbl->RenderFile(gb, L"nothing.mpg", NULL));
    me->lpVtbl->SetNotifyWindow(me, (OAHWND)w, WM_GRAPH, 0);
    me->lpVtbl->GetEventHandle(me, (OAEVENT *)&ev);

    mc->lpVtbl->Run(mc);
    right_after_run("first");
    /* (CatSystem2 moves its window: Pause, Run) */
    mc->lpVtbl->Pause(mc);
    mc->lpVtbl->Run(mc);
    right_after_run("paused and run");
    wait_end("first", 1500);
    printf("signaled after GetEvent %d\n", WaitForSingleObject(ev, 0) == WAIT_OBJECT_0);

    LONGLONG zero = 0;
    ms->lpVtbl->SetPositions(ms, &zero, AM_SEEKING_AbsolutePositioning, NULL, AM_SEEKING_NoPositioning);
    mc->lpVtbl->Run(mc);
    wait_end("rewound", 1500);

    mc->lpVtbl->Stop(mc);
    mc->lpVtbl->Run(mc);
    right_after_run("after stop");
    mc->lpVtbl->Stop(mc);
    wait_end("stopped before its end", 1500);
    mc->lpVtbl->Run(mc);
    wait_end("run again", 1500);
    long code = -1;
    mc->lpVtbl->Stop(mc);
    mc->lpVtbl->Run(mc);
    HRESULT hr = me->lpVtbl->WaitForCompletion(me, 5000, &code);
    printf("wait for completion %08lx code %ld\n", (unsigned long)hr, code);

    mc->lpVtbl->Stop(mc);
    ms->lpVtbl->Release(ms);
    me->lpVtbl->Release(me);
    mc->lpVtbl->Release(mc);
    gb->lpVtbl->Release(gb);
    DeleteFileA("nothing.mpg");
    return 0;
}
