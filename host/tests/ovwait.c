/* libs: -lws2_32 -liphlpapi */
/* GetOverlappedResult(bWait TRUE) on an OVERLAPPED with no event waits on the handle itself:
 * a NotifyAddrChange that another thread cancels (CancelIPChangeNotify) ends the wait as
 * aborted (sake completed it without waking anyone: the wait looked at the OVERLAPPED again
 * only when something else woke it, so it slept to its timeout, INFINITE), and a socket
 * receive that another thread's send completes later ends it with the bytes.
 * SetFileCompletionNotificationModes takes real handles only. */
#include <winsock2.h>
#include <windows.h>
#include <iphlpapi.h>
#include <stdio.h>

static OVERLAPPED change;
static volatile int cancelled = -1;

static DWORD WINAPI cancel_later(void *p) {
    Sleep(200);
    cancelled = CancelIPChangeNotify(&change);
    /* (alive a while: a thread's end wakes every waiter, which would hide a wait that only
     * the completion should end) */
    Sleep(3000);
    return 0;
}

static SOCKET a, b;

static DWORD WINAPI later(void *p) {
    Sleep(200);
    send(b, "hello", 5, 0);
    return 0;
}
static DWORD WINAPI watchdog(void *p) {
    Sleep(15000);
    printf("hung\n");
    fflush(stdout);
    ExitProcess(1);
    return 0;
}

int main(void) {
    WSADATA w;
    WSAStartup(0x202, &w);
    CloseHandle(CreateThread(NULL, 0, watchdog, NULL, 0, NULL));
    HANDLE h = NULL;
    DWORD r0 = NotifyAddrChange(&h, &change);
    printf("notify %lu\n", r0);
    if (r0 == ERROR_IO_PENDING) {
        HANDLE t = CreateThread(NULL, 0, cancel_later, NULL, 0, NULL);
        DWORD got = 7, t0 = GetTickCount();
        SetLastError(0);
        BOOL ok = GetOverlappedResult(h, &change, &got, TRUE);
        DWORD e = GetLastError(), took = GetTickCount() - t0;
        printf("cancel %d\n", cancelled);
        printf("change result %d error %lu bytes %lu before the thread ended %d\n", ok, e, got, took < 2000);
        WaitForSingleObject(t, INFINITE);
        CloseHandle(t);
    }
    SOCKET l = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in sa = {0};
    sa.sin_family = AF_INET;
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int n = sizeof sa;
    if (bind(l, (struct sockaddr *)&sa, sizeof sa) || getsockname(l, (struct sockaddr *)&sa, &n) || listen(l, 1)) {
        printf("skip no loopback\n");
        return 0;
    }
    b = socket(AF_INET, SOCK_STREAM, 0);
    printf("connect %d\n", connect(b, (struct sockaddr *)&sa, sizeof sa));
    a = accept(l, NULL, NULL);
    printf("accepted %d\n", a != INVALID_SOCKET);
    for (int round = 0; round < 2; round++) {
        char buf[16] = {0};
        WSABUF wb = {sizeof buf, buf};
        DWORD got = 0, flags = 0;
        OVERLAPPED ov = {0};
        int r = WSARecv(a, &wb, 1, &got, &flags, &ov, NULL);
        printf("recv %d pending %d\n", r, r == SOCKET_ERROR && WSAGetLastError() == WSA_IO_PENDING);
        CloseHandle(CreateThread(NULL, 0, later, NULL, 0, NULL));
        BOOL ok = GetOverlappedResult((HANDLE)a, &ov, &got, TRUE);
        printf("result %d bytes %lu data %.5s\n", ok, got, buf);
    }
    SetLastError(0);
    BOOL ok = SetFileCompletionNotificationModes((HANDLE)(ULONG_PTR)0x7ffc, 1);
    printf("modes on a garbage handle %d error %lu\n", ok, GetLastError());
    ok = SetFileCompletionNotificationModes((HANDLE)a, 1);
    printf("modes on the socket %d\n", ok);
    closesocket(a);
    closesocket(b);
    closesocket(l);
    return 0;
}
