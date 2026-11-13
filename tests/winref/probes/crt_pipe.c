/* The C runtime's _pipe, as programs use it for a pipe inside one process (PULLTOP's AdvHD
 * trial "ピュアソングガーデン！" calls ucrtbase's _pipe at start and terminates when it fails):
 * the descriptors it gives, _read / _write through them in pieces, a short read, _eof-free EOF
 * after the writer closes (_read 0), EPIPE writing after the reader closes, _get_osfhandle giving
 * pipe handles (PeekNamedPipe, ReadFile, GetFileType), _dup keeping an end open, _fstat64's
 * _S_IFIFO and size and _O_NOINHERIT and the handles' inherit flags. */
#include "../probe.h"
#include <io.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <errno.h>

#define PR(label, expr) do { errno = 0; long long r_ = (long long)(expr); int e_ = errno; printf("%s %lld errno %d\n", label, r_, e_); } while (0)

static void inherit(const char *label, int fd) {
    DWORD f = 0xdead;
    BOOL r = GetHandleInformation((HANDLE)_get_osfhandle(fd), &f);
    printf("%s %d inherit %lu\n", label, r, f & HANDLE_FLAG_INHERIT);
}

int main(void) {
    int fds[2] = { -7, -7 };
    char buf[64];
    struct _stat64 st;

    PR("pipe", _pipe(fds, 256, _O_BINARY));
    printf("fds distinct %d above std %d\n", fds[0] != fds[1], fds[0] > 2 && fds[1] > 2);
    PR("write abc", _write(fds[1], "abcdef", 6));
    memset(buf, 0, sizeof buf);
    PR("read 4", _read(fds[0], buf, 4));
    printf("got '%s'\n", buf);
    PR("fstat", _fstat64(fds[0], &st));
    printf("fifo %d size %lld\n", (st.st_mode & _S_IFMT) == _S_IFIFO, (long long)st.st_size);
    memset(buf, 0, sizeof buf);
    PR("read rest", _read(fds[0], buf, sizeof buf));
    printf("got '%s'\n", buf);
    PR("read on write end", _read(fds[1], buf, 1));
    PR("write on read end", _write(fds[0], "x", 1));

    HANDLE hr = (HANDLE)_get_osfhandle(fds[0]);
    printf("type %lu\n", GetFileType(hr));
    PR("write xyz", _write(fds[1], "xyz", 3));
    DWORD n = 0, avail = 0;
    BOOL ok = PeekNamedPipe(hr, NULL, 0, NULL, &avail, NULL);
    printf("peek %d avail %lu\n", ok, avail);
    memset(buf, 0, sizeof buf);
    ok = ReadFile(hr, buf, 2, &n, NULL);
    printf("ReadFile %d n %lu '%s'\n", ok, n, buf);
    inherit("inherit r", fds[0]);
    inherit("inherit w", fds[1]);

    /* a duplicate keeps the write end open; EOF only after both are closed */
    int w2 = _dup(fds[1]);
    printf("dup ok %d\n", w2 > 2);
    PR("close w", _close(fds[1]));
    PR("write dup", _write(w2, "Q", 1));
    memset(buf, 0, sizeof buf);
    PR("read after close", _read(fds[0], buf, sizeof buf));
    printf("got '%s'\n", buf);
    PR("close dup", _close(w2));
    PR("read eof", _read(fds[0], buf, sizeof buf));
    PR("close r", _close(fds[0]));

    /* the reader gone: EPIPE */
    PR("pipe noinherit", _pipe(fds, 0, _O_BINARY | _O_NOINHERIT));
    inherit("noinherit r", fds[0]);
    inherit("noinherit w", fds[1]);
    PR("close r2", _close(fds[0]));
    PR("write broken", _write(fds[1], "z", 1));
    PR("close w2", _close(fds[1]));

    PR("pipe text", _pipe(fds, 64, _O_TEXT));
    PR("close t0", _close(fds[0]));
    PR("close t1", _close(fds[1]));
    return 0;
}
