/* winmm mmioOpen on a file the program already opened (szFileName NULL, adwInfo[0] = the
 * handle), with no I/O procedure named or the DOS one ('DOS '), buffered (MMIO_ALLOCBUF) or not:
 * RGSS2 (RPG Maker VX, Game.rgss2a) reads its archive this way. Reads, seeks (SEEK_SET/CUR/END,
 * past the end), mmioDescend on a RIFF read through the handle, mmioWrite refused on a read-only
 * open, and the handle's own file still usable after mmioClose. Works in "wr-mmioh".
 * libs: -lwinmm */
#include "../probe.h"
#include <mmsystem.h>

/* an archive-like header (16 bytes), then data the test reads back */
static BYTE arc[64 + 4096];
static BYTE riff[] = {
    'R','I','F','F', 28,0,0,0, 'W','A','V','E',
    'f','m','t',' ', 4,0,0,0, 1,0, 1,0,
    'd','a','t','a', 4,0,0,0, 9,8,7,6,
};

static HANDLE make(const char *name, const BYTE *b, DWORD n) {
    HANDLE f = CreateFileA(name, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, b, n, &w, NULL);
    CloseHandle(f);
    return CreateFileA(name, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
}

static HMMIO open_on(HANDLE f, FOURCC fcc, DWORD flags) {
    MMIOINFO mi = { 0 };
    mi.fccIOProc = fcc;
    mi.adwInfo[0] = (DWORD)(UINT_PTR)f;
    HMMIO h = mmioOpenA(NULL, &mi, flags);
    printf("open fcc %.4s flags %lx: %s err %u\n", fcc ? (char *)&fcc : "0000", flags, h ? "ok" : "null", mi.wErrorRet);
    return h;
}

static void reads(HMMIO h) {
    BYTE b[16];
    LONG n = mmioRead(h, (HPSTR)b, 8);
    printf("read %ld: %.6s %02x %02x pos %ld\n", n, b, b[6], b[7], mmioSeek(h, 0, SEEK_CUR));
    printf("seek set 100 -> %ld\n", mmioSeek(h, 100, SEEK_SET));
    n = mmioRead(h, (HPSTR)b, 4);
    printf("read %ld: %02x %02x %02x %02x\n", n, b[0], b[1], b[2], b[3]);
    printf("seek cur 10 -> %ld\n", mmioSeek(h, 10, SEEK_CUR));
    n = mmioRead(h, (HPSTR)b, 2);
    printf("read %ld: %02x %02x\n", n, b[0], b[1]);
    printf("seek end -> %ld\n", mmioSeek(h, 0, SEEK_END));
    printf("seek end -4 -> %ld\n", mmioSeek(h, -4, SEEK_END));
    n = mmioRead(h, (HPSTR)b, 16);
    printf("read at end %ld: %02x %02x %02x %02x\n", n, b[0], b[1], b[2], b[3]);
    printf("read past end %ld\n", mmioRead(h, (HPSTR)b, 4));
    printf("write %ld\n", mmioWrite(h, "x", 1));
}

int main(void) {
    for (int i = 0; i < (int)sizeof arc; i++) arc[i] = (BYTE)(i * 7 + 3);
    memcpy(arc, "RGSSAD\0\1", 8);
    CreateDirectoryA("wr-mmioh", NULL);
    HANDLE f = make("wr-mmioh\\arc.bin", arc, sizeof arc);
    printf("file %s\n", f != INVALID_HANDLE_VALUE ? "ok" : "fail");

    FOURCC dos = mmioFOURCC('D', 'O', 'S', ' ');
    DWORD modes[] = { MMIO_READ | MMIO_ALLOCBUF, MMIO_READ };
    for (int m = 0; m < 2; m++) {
        for (int k = 0; k < 2; k++) {
            SetFilePointer(f, 0, NULL, FILE_BEGIN);
            HMMIO h = open_on(f, k ? 0 : dos, modes[m]);
            if (!h) continue;
            reads(h);
            printf("close %u\n", mmioClose(h, MMIO_FHOPEN));
            /* MMIO_FHOPEN: the program's handle stays open */
            DWORD got = 0;
            BYTE b[4] = { 0 };
            SetFilePointer(f, 8, NULL, FILE_BEGIN);
            BOOL ok = ReadFile(f, b, 4, &got, NULL);
            printf("handle after close: %d %lu %02x\n", ok, got, b[0]);
        }
    }
    CloseHandle(f);

    /* a RIFF file through its handle: mmioDescend walks it */
    HANDLE r = make("wr-mmioh\\a.wav", riff, sizeof riff);
    HMMIO h = open_on(r, dos, MMIO_READ | MMIO_ALLOCBUF);
    if (h) {
        MMCKINFO p = { 0 }, c = { 0 };
        MMRESULT e = mmioDescend(h, &p, NULL, 0);
        printf("descend riff %u %.4s %lu %.4s\n", e, (char *)&p.ckid, p.cksize, (char *)&p.fccType);
        c.ckid = mmioFOURCC('d', 'a', 't', 'a');
        e = mmioDescend(h, &c, &p, MMIO_FINDCHUNK);
        BYTE b[4] = { 0 };
        LONG n = mmioRead(h, (HPSTR)b, 4);
        printf("find data %u size %lu offset %lu read %ld %02x%02x%02x%02x\n", e, c.cksize, c.dwDataOffset, n, b[0], b[1], b[2], b[3]);
        mmioClose(h, MMIO_FHOPEN);
    }
    CloseHandle(r);
    DeleteFileA("wr-mmioh\\a.wav");
    DeleteFileA("wr-mmioh\\arc.bin");
    RemoveDirectoryA("wr-mmioh");
    return 0;
}
