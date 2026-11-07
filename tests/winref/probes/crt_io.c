/* msvcrt's low-level I/O and what Python asks of the C runtime besides stdio: descriptors
 * (_open, _read, _write, _lseeki64, _chsize, _dup, _dup2, _setmode, _get_osfhandle,
 * _open_osfhandle, _fileno, _fdopen), _stat64 / _fstat64, _access, _unlink, the current
 * directory, the environment (_putenv, _wputenv, getenv, _wgetenv, _environ), multibyte
 * conversion in code page 932, strerror's messages, and the C99 math sake's CRT added. */
#include "../probe.h"
#include <io.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <direct.h>
#include <errno.h>
#include <wchar.h>
#include <locale.h>
#include <math.h>
#include <share.h>

/* the result and errno of `expr` (errno cleared first) */
#define PR(label, expr) do { errno = 0; long long r_ = (long long)(expr); int e_ = errno; printf("%s %lld errno %d\n", label, r_, e_); } while (0)

static void st(const char *label, int r, const struct _stat64 *s) {
    if (r != 0) {
        printf("%s -1 errno %d\n", label, errno);
        return;
    }
    printf("%s mode %#o nlink %d size %lld\n", label, s->st_mode, s->st_nlink, (long long)s->st_size);
}

int main(void) {
    char buf[300];
    struct _stat64 s;
    setlocale(LC_ALL, "");  /* (the ANSI code page: 932 from the manifest) */

    /* descriptors */
    int fd = _open("io_a.bin", _O_CREAT | _O_TRUNC | _O_RDWR | _O_BINARY, _S_IREAD | _S_IWRITE);
    printf("open %d\n", fd >= 3);
    printf("write %d\n", _write(fd, "hello, world\n", 13));
    printf("lseeki64 %lld\n", (long long)_lseeki64(fd, 0, SEEK_CUR));
    printf("lseek set %ld\n", _lseek(fd, 7, SEEK_SET));
    memset(buf, 0, sizeof buf);
    int n = _read(fd, buf, 100);
    printf("read %d '%s'\n", n, buf);
    printf("read at end %d\n", _read(fd, buf, 100));
    printf("tell %ld eof %d\n", _tell(fd), _eof(fd));
    printf("filelength %ld\n", _filelength(fd));
    printf("lseek end %ld\n", _lseek(fd, -6, SEEK_END));
    PR("lseek bad origin", _lseek(fd, 0, 7));
    PR("lseek negative", _lseek(fd, -100, SEEK_SET));
    int r = _chsize(fd, 20);
    printf("chsize %d length %ld\n", r, _filelength(fd));
    _lseek(fd, 13, SEEK_SET);
    memset(buf, 'x', 10);
    n = _read(fd, buf, 10);
    printf("read zeros %d %d %d\n", n, buf[0], buf[6]);
    r = _chsize(fd, 5);
    printf("chsize down %d length %ld\n", r, _filelength(fd));
    st("fstat64", _fstat64(fd, &s), &s);
    int d = _dup(fd);
    printf("dup %d\n", d > fd);
    _lseek(fd, 0, SEEK_SET);
    printf("dup shares position %ld\n", _tell(d));
    printf("dup2 %d\n", _dup2(fd, 40));
    r = _close(40);
    printf("close dup2 %d close dup %d\n", r, _close(d));
    r = _setmode(fd, _O_TEXT);
    printf("setmode %#x %#x\n", r, _setmode(fd, _O_BINARY));
    printf("get_osfhandle %d\n", _get_osfhandle(fd) != -1);
    printf("close %d\n", _close(fd));
    PR("close again", _close(fd));
    PR("read closed", _read(fd, buf, 1));
    PR("get_osfhandle closed", _get_osfhandle(fd));
    PR("write stdin", _write(0, "x", 1));

    /* opening */
    PR("open missing", _open("io_missing.bin", _O_RDONLY));
    PR("open excl", _open("io_a.bin", _O_CREAT | _O_EXCL | _O_WRONLY, _S_IWRITE));
    PR("open dir", _open(".", _O_RDONLY));
    fd = _open("io_a.bin", _O_WRONLY | _O_APPEND | _O_BINARY);
    _write(fd, "XY", 2);
    _lseek(fd, 0, SEEK_SET);
    _write(fd, "Z", 1);
    _close(fd);
    fd = _open("io_a.bin", _O_RDONLY | _O_BINARY);
    memset(buf, 0, sizeof buf);
    n = _read(fd, buf, 100);
    printf("append '%.*s' %d\n", n, buf, (int)strlen(buf));
    PR("write readonly", _write(fd, "q", 1));
    _close(fd);
    fd = _open("io_a.bin", _O_RDWR | _O_TRUNC | _O_BINARY);
    printf("trunc length %ld\n", _filelength(fd));
    _close(fd);
    int pfd = -2;
    r = _sopen_s(&pfd, "io_missing.bin", _O_RDONLY, _SH_DENYNO, 0);
    printf("sopen_s %d %d\n", r, pfd);

    /* a handle as a descriptor, a descriptor as a stream */
    HANDLE h = CreateFileA("io_b.txt", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    fd = _open_osfhandle((intptr_t)h, 0);
    printf("open_osfhandle %d same handle %d\n", fd >= 3, (HANDLE)_get_osfhandle(fd) == h);
    _write(fd, "abc\n", 4);
    FILE *f = _fdopen(fd, "r+b");
    printf("fdopen %d fileno %d\n", f != NULL, f && _fileno(f) == fd);
    fseek(f, 0, SEEK_SET);
    printf("fgetc %c\n", fgetc(f));
    fclose(f);
    PR("closed with the stream", _close(fd));
    printf("fileno std %d %d %d\n", _fileno(stdin), _fileno(stdout), _fileno(stderr));
    f = fopen("io_b.txt", "rb");
    printf("fopen fileno %d\n", _fileno(f) > 2);
    fclose(f);

    /* stat */
    st("stat file", _stat64("io_a.bin", &s), &s);
    st("stat dir", _stat64(".", &s), &s);
    st("stat dir slash", _stat64(".\\", &s), &s);
    st("stat file slash", _stat64("io_a.bin\\", &s), &s);
    st("stat missing", _stat64("io_missing.bin", &s), &s);
    st("stat wildcard", _stat64("io_*.bin", &s), &s);
    st("stat exe", _stat64("..\\..\\..\\..\\..\\nonexistent.exe", &s), &s);
#ifndef _WIN64  /* (x64's msvcrt has no _stat32) */
    struct _stat32 s32;
    r = _stat32("io_b.txt", &s32);
    printf("stat32 %d size %ld\n", r, (long)s32.st_size);
#endif
    struct _stat64i32 s6432;
    r = _wstat64i32(L"io_b.txt", &s6432);
    printf("wstat64i32 %d size %ld mode %#o\n", r, (long)s6432.st_size, s6432.st_mode);

    /* by name */
    printf("access %d %d\n", _access("io_a.bin", 0), _access("io_a.bin", 6));
    PR("access missing", _access("io_missing.bin", 0));
    PR("access bad mode", _access("io_a.bin", 1));
    _chmod("io_b.txt", _S_IREAD);
    PR("access readonly", _access("io_b.txt", 2));
    _chmod("io_b.txt", _S_IREAD | _S_IWRITE);
    printf("unlink %d\n", _unlink("io_a.bin"));
    PR("unlink again", _unlink("io_a.bin"));
    printf("wremove %d\n", _wremove(L"io_b.txt"));
    PR("unlink dir", _unlink("."));

    /* the current directory */
    char cwd[MAX_PATH], cwd2[MAX_PATH];
    _getcwd(cwd, sizeof cwd);
    _mkdir("io_dir");
    printf("chdir %d\n", _chdir("io_dir"));
    _getcwd(cwd2, sizeof cwd2);
    printf("getcwd moved %d ends %s\n", strlen(cwd2) == strlen(cwd) + 7, cwd2 + strlen(cwd2) - 6);
    wchar_t wcwd[MAX_PATH];
    printf("wgetcwd same %d\n", _wgetcwd(wcwd, MAX_PATH) != NULL && (int)wcslen(wcwd) == (int)strlen(cwd2));
    PR("getcwd small", _getcwd(buf, 3) != NULL);
    char *dyn = _getcwd(NULL, 0);
    printf("getcwd alloc %d\n", dyn && strcmp(dyn, cwd2) == 0);
    free(dyn);
    printf("chdir up %d\n", _chdir(".."));
    PR("chdir missing", _chdir("io_nodir"));
    _rmdir("io_dir");
    char full[MAX_PATH];
    printf("fullpath %d\n", _fullpath(full, "a\\..\\b.txt", sizeof full) && strlen(full) == strlen(cwd) + 6);

    /* environment */
    r = _putenv("IO_PROBE=one two");
    printf("putenv %d getenv %s\n", r, getenv("IO_PROBE"));
    wchar_t *wv = _wgetenv(L"IO_PROBE");
    printf("wgetenv %d\n", wv && wcscmp(wv, L"one two") == 0);
    r = _wputenv(L"IO_PROBE=three");
    printf("wputenv %d getenv %s\n", r, getenv("IO_PROBE"));
    r = _putenv_s("IO_PROBE", "four");
    printf("putenv_s %d getenv %s\n", r, getenv("IO_PROBE"));
    int seen = 0;
    for (char **e = _environ; *e; e++)
        if (strcmp(*e, "IO_PROBE=four") == 0) seen++;
    printf("environ has it %d\n", seen);
    r = _putenv("IO_PROBE=");
    printf("putenv empty %d getenv %p\n", r, (void *)getenv("IO_PROBE"));
    PR("putenv bad", _putenv("NOEQUALS"));
    /* the CRT's copy of the environment: kernel32's changes do not reach it, _putenv's reach both */
    SetEnvironmentVariableA("IO_K32", "k");
    printf("getenv after SetEnvironmentVariable %d\n", getenv("IO_K32") != NULL);
    r = _putenv("IO_CRT=c");
    printf("putenv reaches kernel32 %d %d\n", r, (int)GetEnvironmentVariableA("IO_CRT", buf, sizeof buf));
    size_t need = 0;
    r = getenv_s(&need, NULL, 0, "IO_NOT_SET_ANYWHERE");
    printf("getenv_s %d need %d\n", r, (int)need);

    /* multibyte (932) */
    const char *sj = "\x82\xa0" "a\x83\x41";  /* あaア */
    wchar_t w[8];
    n = (int)mbstowcs(w, sj, 8);
    printf("mbstowcs %d %04x %04x %04x\n", n, w[0], w[1], w[2]);
    printf("mbstowcs len %d\n", (int)mbstowcs(NULL, sj, 0));
    char back[16];
    memset(back, 0, sizeof back);
    n = (int)wcstombs(back, w, sizeof back);
    printf("wcstombs %d %02x%02x\n", n, (unsigned char)back[0], (unsigned char)back[1]);
    printf("wcstombs cut %d\n", (int)wcstombs(back, w, 1));
    mbstate_t ms;
    memset(&ms, 0, sizeof ms);
    wchar_t wc = 0;
    n = (int)mbrtowc(&wc, sj, 2, &ms);
    printf("mbrtowc %d %04x\n", n, wc);
    n = (int)mbrtowc(&wc, "z", 1, &ms);
    printf("mbrtowc ascii %d %04x\n", n, wc);
    printf("mbrtowc nul %d\n", (int)mbrtowc(&wc, "", 1, &ms));
    memset(&ms, 0, sizeof ms);
    n = (int)mbrlen(sj, 2, &ms);
    printf("mbrlen %d %d\n", n, (int)mbrlen("q", 1, &ms));
    n = mbtowc(&wc, sj + 3, 2);
    printf("mbtowc %d %04x\n", n, wc);
    memset(&ms, 0, sizeof ms);
    n = (int)wcrtomb(back, 0x3042, &ms);
    printf("wcrtomb %d %02x%02x\n", n, (unsigned char)back[0], (unsigned char)back[1]);
    printf("btowc %04x %04x %04x\n", btowc('A'), btowc(0x82), btowc(0xb1));
    printf("wctob %d %d %d\n", wctob(L'A'), wctob(0x3042), wctob(0xff71));

    /* messages */
    for (int e = 0; e < 44; e++) {
        strerror_s(buf, sizeof buf, e);
        printf("strerror %d %s\n", e, buf);
    }
    printf("strerror %s | %s\n", strerror(ENOENT), strerror(200));
    printf("wcserror %d\n", wcscmp(_wcserror(EACCES), L"Permission denied") == 0);
    r = strerror_s(buf, 5, 2);
    printf("strerror_s small %d '%s'\n", r, buf);

    /* C99 math */
    probe_ulp(1, "erf", erf(0.5));
    probe_ulp(1, "erfc", erfc(1.5));
    probe_ulp(0, "fma", fma(0.1, 10.0, -1.0));
    probe_ulp(0, "rint", rint(2.5));
    probe_ulp(0, "rint-", rint(-3.5));
    printf("lrint %ld %ld\n", lrint(2.5), lrint(3.5));
    probe_ulp(0, "nextafter", nextafter(1.0, 2.0));
    probe_ulp(0, "nearbyint", nearbyint(0.5));
    printf("isblank %d %d %d\n", isblank(' '), isblank('\t'), isblank('x'));
    printf("iswctype %d %d\n", iswctype(L'A', _UPPER) != 0, iswctype(L'1', _ALPHA) != 0);
    return 0;
}
