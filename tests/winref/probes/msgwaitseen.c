/* MsgWaitForMultipleObjects after PeekMessage(PM_NOREMOVE): a message the thread has already
 * looked at does not wake the wait again, so the wait answers the handle. InstallShield 12's
 * setup launcher (マスクド上海) waits for msiexec that way, peeking between waits and leaving the
 * message in the queue. MsgWait itself does not count as looking (msgwait.c), GetMessage and
 * PeekMessage(PM_REMOVE) of another message do; MWMO_INPUTAVAILABLE counts every message. */
#include "../probe.h"

static const char *wait_name(DWORD r) {
    static char b[32];
    if (r == WAIT_TIMEOUT) return "timeout";
    if (r == WAIT_FAILED) return "failed";
    sprintf(b, "%lu", r);
    return b;
}
static void drain(void) {
    MSG m;
    while (PeekMessageA(&m, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&m);
}

int main(void) {
    HANDLE ev = CreateEventA(NULL, TRUE, FALSE, NULL);
    MSG m = { 0 };
    DWORD me = GetCurrentThreadId();
    drain();
    PostThreadMessageA(me, WM_USER, 0, 0);
    printf("posted: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 0, QS_ALLINPUT)));
    { BOOL got = PeekMessageA(&m, NULL, 0, 0, PM_NOREMOVE); printf("peek noremove %d msg %x\n", got, got ? m.message : 0); }
    printf("after peek: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_ALLINPUT)));
    printf("after peek, mask QS_POSTMESSAGE: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_POSTMESSAGE)));
    printf("Ex MWMO_INPUTAVAILABLE: %s\n", wait_name(MsgWaitForMultipleObjectsEx(1, &ev, 30, QS_ALLINPUT, MWMO_INPUTAVAILABLE)));
    SetEvent(ev);
    printf("event set: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_ALLINPUT)));
    ResetEvent(ev);
    PostThreadMessageA(me, WM_USER + 1, 0, 0);
    printf("second post: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_ALLINPUT)));
    printf("peek filtered to WM_USER+5 %d\n", PeekMessageA(&m, NULL, WM_USER + 5, WM_USER + 5, PM_NOREMOVE));
    printf("after filtered peek: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_ALLINPUT)));
    { BOOL got = PeekMessageA(&m, NULL, 0, 0, PM_REMOVE); printf("remove first %d msg %x\n", got, got ? m.message : 0); }
    printf("after remove: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_ALLINPUT)));
    printf("Ex MWMO_INPUTAVAILABLE: %s\n", wait_name(MsgWaitForMultipleObjectsEx(1, &ev, 30, QS_ALLINPUT, MWMO_INPUTAVAILABLE)));
    drain();
    PostQuitMessage(3);
    printf("quit: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 0, QS_ALLINPUT)));
    { BOOL got = PeekMessageA(&m, NULL, 0, 0, PM_NOREMOVE); printf("peek quit %d msg %x\n", got, got ? m.message : 0); }
    printf("after quit peek: %s\n", wait_name(MsgWaitForMultipleObjects(1, &ev, FALSE, 30, QS_ALLINPUT)));
    drain();
    return 0;
}
