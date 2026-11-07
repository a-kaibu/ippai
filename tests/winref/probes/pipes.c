/* Pipes inside one process, as programs use them to talk to their helpers (the ソフト電池
 * runtime's \\.\pipe\softdenchi.* and the like): anonymous pipes (CreatePipe, sizes, EOF after the
 * writer closes), named pipes in byte and message mode (CreateNamedPipe arguments and errors,
 * a client CreateFile before ConnectNamedPipe, ERROR_PIPE_CONNECTED, PeekNamedPipe counts,
 * ERROR_MORE_DATA on a short message read, SetNamedPipeHandleState, GetNamedPipeInfo,
 * TransactNamedPipe, WaitNamedPipe, all instances busy, DisconnectNamedPipe, the broken pipe
 * errors), and overlapped ConnectNamedPipe. Single-threaded: nothing depends on scheduling. */
#include "../probe.h"

#define E(expr) do { SetLastError(0xdead); long long r_ = (long long)(expr); printf("%-78s = %lld err %lu\n", #expr, r_, GetLastError()); } while (0)
#define P(name) "\\\\.\\pipe\\ippai_probe_" name

static char buf[256];
static DWORD n, avail, left, msg;

static void peek(HANDLE h) {
    n = avail = left = 0xdead;
    memset(buf, 0, sizeof buf);
    SetLastError(0xdead);
    BOOL r = PeekNamedPipe(h, buf, 4, &n, &avail, &left);
    printf("peek %d err %lu read %lu avail %lu left %lu data '%.4s'\n", r, GetLastError(), n, avail, left, buf);
}
static void rd(HANDLE h, DWORD len) {
    n = 0xdead;
    memset(buf, 0, sizeof buf);
    SetLastError(0xdead);
    BOOL r = ReadFile(h, buf, len, &n, NULL);
    printf("read(%lu) %d err %lu got %lu '%s'\n", len, r, GetLastError(), n, buf);
}
static void wr(HANDLE h, const char *s) {
    n = 0xdead;
    SetLastError(0xdead);
    BOOL r = WriteFile(h, s, (DWORD)strlen(s), &n, NULL);
    printf("write '%s' %d err %lu wrote %lu\n", s, r, GetLastError(), n);
}
static HANDLE client(const char *name, DWORD access) {
    SetLastError(0xdead);
    HANDLE h = CreateFileA(name, access, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("client %s valid %d err %lu\n", name + 9, h != INVALID_HANDLE_VALUE, GetLastError());
    return h;
}

int main(void) {
    /* anonymous */
    HANDLE r, w;
    E(CreatePipe(&r, &w, NULL, 0));
    wr(w, "abc");
    peek(r);
    rd(r, 2);
    rd(r, 10);
    E(GetFileType(r));
    E(GetFileSize(r, NULL));
    wr(r, "x"); /* the read end */
    rd(w, 1);   /* the write end */
    wr(w, "");
    peek(r);
    E(CloseHandle(w));
    rd(r, 4); /* writer gone: ERROR_BROKEN_PIPE */
    peek(r);
    E(CloseHandle(r));
    E(CreatePipe(&r, &w, NULL, 0));
    E(CloseHandle(r));
    wr(w, "q"); /* reader gone */
    E(CloseHandle(w));

    /* named, byte mode */
    E(CreateNamedPipeA("\\\\.\\pipe\\", PIPE_ACCESS_DUPLEX, 0, 1, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE);
    E(CreateNamedPipeA("C:\\notapipe", PIPE_ACCESS_DUPLEX, 0, 1, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE);
    E(CreateNamedPipeA(P("x"), PIPE_ACCESS_DUPLEX, 0, 0, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE);
    E(CreateNamedPipeA(P("x"), 0, 0, 1, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE);
    client(P("none"), GENERIC_READ | GENERIC_WRITE);
    E(WaitNamedPipeA(P("none"), 0));
    HANDLE s = CreateNamedPipeA(P("byte"), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 512, 512, 0, NULL);
    E(s != INVALID_HANDLE_VALUE);
    E(GetFileType(s));
    E(CreateNamedPipeA(P("byte"), PIPE_ACCESS_DUPLEX, 0, 1, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE); /* 1 instance only */
    E(WaitNamedPipeA(P("byte"), 0));
    peek(s); /* not connected yet */
    HANDLE c = client(P("byte"), GENERIC_READ | GENERIC_WRITE);
    client(P("byte"), GENERIC_READ | GENERIC_WRITE); /* busy */
    E(WaitNamedPipeA(P("byte"), 0));
    E(ConnectNamedPipe(s, NULL)); /* already connected */
    wr(c, "hello");
    wr(c, "world");
    peek(s);
    rd(s, 3);
    rd(s, 100);
    wr(s, "reply");
    rd(c, 100);
    DWORD state = 0xdead, inst = 0xdead;
    E(GetNamedPipeHandleStateA(s, &state, &inst, NULL, NULL, NULL, 0));
    printf("state %#lx instances %lu\n", state, inst);
    DWORD flags = 0xdead, out = 0xdead, in = 0xdead, max = 0xdead;
    E(GetNamedPipeInfo(s, &flags, &out, &in, &max));
    printf("server info flags %#lx out %lu in %lu max %lu\n", flags, out, in, max);
    E(GetNamedPipeInfo(c, &flags, &out, &in, &max));
    printf("client info flags %#lx out %lu in %lu max %lu\n", flags, out, in, max);
    DWORD mode = PIPE_READMODE_MESSAGE;
    E(SetNamedPipeHandleState(c, &mode, NULL, NULL)); /* byte pipe: no message reads */
    E(CloseHandle(c));
    rd(s, 10);
    peek(s);
    wr(s, "late");
    E(ConnectNamedPipe(s, NULL)); /* still not disconnected */
    E(DisconnectNamedPipe(s));
    E(DisconnectNamedPipe(s));
    rd(s, 10);
    E(WaitNamedPipeA(P("byte"), 0));
    c = client(P("byte"), GENERIC_READ | GENERIC_WRITE); /* listening again after disconnect? */
    if (c != INVALID_HANDLE_VALUE) E(CloseHandle(c));
    E(CloseHandle(s));
    client(P("byte"), GENERIC_READ | GENERIC_WRITE); /* gone */

    /* named, message mode, two instances */
    HANDLE m1 = CreateNamedPipeA(P("msg"), PIPE_ACCESS_DUPLEX, PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE, 2, 512, 512, 0, NULL);
    HANDLE m2 = CreateNamedPipeA(P("msg"), PIPE_ACCESS_DUPLEX, PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE, 2, 512, 512, 0, NULL);
    E(m1 != INVALID_HANDLE_VALUE && m2 != INVALID_HANDLE_VALUE);
    E(CreateNamedPipeA(P("msg"), PIPE_ACCESS_DUPLEX, PIPE_TYPE_MESSAGE, 2, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE);
    E(CreateNamedPipeA(P("msg"), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, 512, 512, 0, NULL) != INVALID_HANDLE_VALUE);
    HANDLE c1 = client(P("msg"), GENERIC_READ | GENERIC_WRITE);
    HANDLE c2 = client(P("msg"), GENERIC_READ | GENERIC_WRITE);
    client(P("msg"), GENERIC_READ | GENERIC_WRITE);
    wr(c1, "first message");
    wr(c1, "second");
    wr(c2, "to two");
    peek(m1);
    rd(m1, 5); /* ERROR_MORE_DATA */
    peek(m1);
    rd(m1, 100);
    rd(m1, 100);
    rd(m2, 100);
    peek(c1);
    E(GetNamedPipeHandleStateA(c1, &state, &inst, NULL, NULL, NULL, 0));
    printf("client state %#lx instances %lu\n", state, inst);
    mode = PIPE_READMODE_MESSAGE;
    E(SetNamedPipeHandleState(c1, &mode, NULL, NULL));
    E(GetNamedPipeHandleStateA(c1, &state, &inst, NULL, NULL, NULL, 0));
    printf("client state %#lx\n", state);
    wr(m1, "ab");
    wr(m1, "cd");
    rd(c1, 1);
    rd(c1, 10);
    rd(c1, 10);
    mode = PIPE_READMODE_BYTE;
    E(SetNamedPipeHandleState(m2, &mode, NULL, NULL));
    wr(c2, "xy");
    wr(c2, "zw");
    rd(m2, 10); /* byte reads of a message pipe: across messages? */
    /* TransactNamedPipe: c1 is in message read mode */
    wr(m1, "pending"); /* a message waiting: the transaction fails */
    n = 0xdead;
    E(TransactNamedPipe(c1, "q", 1, buf, sizeof buf, &n, NULL));
    rd(c1, 100);
    E(CloseHandle(c1));
    E(CloseHandle(c2));
    rd(m1, 10);
    wr(m1, "x");
    E(CloseHandle(m1));
    E(CloseHandle(m2));

    /* one-way pipes and access */
    HANDLE in1 = CreateNamedPipeA(P("in"), PIPE_ACCESS_INBOUND, 0, 1, 512, 512, 0, NULL); /* (0: a write waits for the reader) */
    E(in1 != INVALID_HANDLE_VALUE);
    client(P("in"), GENERIC_READ | GENERIC_WRITE); /* the client may only write */
    HANDLE cw = client(P("in"), GENERIC_WRITE);
    wr(cw, "up");
    rd(in1, 10);
    wr(in1, "down");
    E(CloseHandle(cw));
    E(CloseHandle(in1));

    /* overlapped ConnectNamedPipe */
    HANDLE ov = CreateNamedPipeA(P("ov"), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED, 0, 1, 512, 512, 0, NULL);
    OVERLAPPED o = { 0 };
    o.hEvent = CreateEventA(NULL, TRUE, FALSE, NULL);
    E(ConnectNamedPipe(ov, &o));
    E(WaitForSingleObject(o.hEvent, 0));
    HANDLE oc = client(P("ov"), GENERIC_READ | GENERIC_WRITE);
    E(WaitForSingleObject(o.hEvent, 0));
    DWORD got = 0xdead;
    E(GetOverlappedResult(ov, &o, &got, FALSE));
    wr(oc, "ovl");
    ResetEvent(o.hEvent);
    memset(buf, 0, sizeof buf);
    E(ReadFile(ov, buf, 100, NULL, &o));
    E(GetOverlappedResult(ov, &o, &got, TRUE));
    printf("overlapped read %lu '%s'\n", got, buf);
    ResetEvent(o.hEvent);
    E(ReadFile(ov, buf, 100, NULL, &o)); /* nothing there: pending */
    E(CancelIo(ov));
    E(GetOverlappedResult(ov, &o, &got, TRUE));
    E(CloseHandle(oc));
    E(CloseHandle(ov));
    return 0;
}
