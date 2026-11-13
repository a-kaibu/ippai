/* libs: -lwinmm */
/* winmm's buffered mmio reads: mmioOpen with MMIO_ALLOCBUF (the default 8 KB buffer, or the
 * MMIOINFO's cchBuffer), mmioGetInfo, reading from pchNext and refilling with mmioAdvance, then
 * mmioSetInfo and mmioRead going on from where the buffer reading stopped. The DirectX SDK's
 * WaveReadFile (wavread.cpp) reads WAVE data this way (Pygmalion 体験版 calls mmioAdvance). */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>

static int failed;
#define CHECK(label, expr) do { int ok_ = !!(expr); printf("%s: %s\n", label, ok_ ? "ok" : "FAIL"); failed |= !ok_; } while (0)

#define DATA 20000
static BYTE file[44 + DATA];

static void make(void) {
    DWORD i, w;
    HANDLE h;
    memcpy(file, "RIFF", 4);
    *(DWORD *)(file + 4) = 36 + DATA;
    memcpy(file + 8, "WAVEfmt ", 8);
    *(DWORD *)(file + 16) = 16;
    *(WORD *)(file + 20) = 1;
    *(WORD *)(file + 22) = 1;
    *(DWORD *)(file + 24) = 22050;
    *(DWORD *)(file + 28) = 22050;
    *(WORD *)(file + 32) = 1;
    *(WORD *)(file + 34) = 8;
    memcpy(file + 36, "data", 4);
    *(DWORD *)(file + 40) = DATA;
    for (i = 0; i < DATA; i++) file[44 + i] = (BYTE)(i * 7 + (i >> 8));
    h = CreateFileA("mmiobuf.wav", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WriteFile(h, file, sizeof(file), &w, NULL);
    CloseHandle(h);
}

/* WaveReadFile: n bytes from the buffer, refilling it when pchNext reaches pchEndRead */
static UINT read_buffered(HMMIO h, MMIOINFO *mi, BYTE *dst, UINT n, int *advances) {
    UINT i;
    for (i = 0; i < n; i++) {
        if (mi->pchNext == mi->pchEndRead) {
            if (mmioAdvance(h, mi, MMIO_READ) != 0) return i;
            ++*advances;
            if (mi->pchNext == mi->pchEndRead) return i;
        }
        dst[i] = *(BYTE *)mi->pchNext++;
    }
    return i;
}

static void run(DWORD cch) {
    MMIOINFO open = {0}, mi;
    MMCKINFO riff = {0}, ck = {0};
    static BYTE got[DATA + 100];
    HMMIO h;
    UINT n, n2;
    int adv = 0;
    char label[80];
    open.cchBuffer = cch;
    h = mmioOpenA("mmiobuf.wav", &open, MMIO_READ | MMIO_ALLOCBUF);
    printf("buffer %lu: open %s\n", cch, h ? "ok" : "FAIL");
    if (!h) { failed = 1; return; }
    riff.fccType = mmioFOURCC('W', 'A', 'V', 'E');
    mmioDescend(h, &riff, NULL, MMIO_FINDRIFF);
    ck.ckid = mmioFOURCC('d', 'a', 't', 'a');
    mmioDescend(h, &ck, &riff, MMIO_FINDCHUNK);
    printf("  data chunk %lu bytes at %lu\n", ck.cksize, ck.dwDataOffset);
    mmioGetInfo(h, &mi, 0);
    /* (where the buffer starts and what it holds after mmioDescend is Windows' own business:
     * pchNext and lBufOffset + (pchNext - pchBuffer) are the position) */
    printf("  info: cchBuffer %ld, buffer %s, at %ld\n", mi.cchBuffer, mi.pchBuffer ? "set" : "NULL", mi.lBufOffset + (long)(mi.pchNext - mi.pchBuffer));
    n = read_buffered(h, &mi, got, 5000, &adv);
    printf("  read %u bytes; at %ld\n", n, mi.lBufOffset + (long)(mi.pchNext - mi.pchBuffer));
    sprintf(label, "  buffer %lu: the first 5000 bytes", cch);
    CHECK(label, n == 5000 && !memcmp(got, file + 44, 5000));
    mmioSetInfo(h, &mi, 0);
    n2 = mmioRead(h, (HPSTR)got + 5000, 3000);
    sprintf(label, "  buffer %lu: mmioRead after mmioSetInfo goes on (%u)", cch, n2);
    CHECK(label, n2 == 3000 && !memcmp(got + 5000, file + 44 + 5000, 3000));
    printf("  position %ld\n", mmioSeek(h, 0, SEEK_CUR));
    mmioGetInfo(h, &mi, 0);
    n = read_buffered(h, &mi, got + 8000, DATA + 100, &adv);
    sprintf(label, "  buffer %lu: to the end (%u bytes)", cch, n);
    CHECK(label, n == DATA - 8000 && !memcmp(got + 8000, file + 44 + 8000, DATA - 8000));
    printf("  at the end: advance %u, next == endread %d\n", mmioAdvance(h, &mi, MMIO_READ), mi.pchNext == mi.pchEndRead);
    mmioSetInfo(h, &mi, 0);
    printf("  position %ld\n", mmioSeek(h, 0, SEEK_CUR));
    mmioClose(h, 0);
}

int main(void) {
    make();
    run(0);
    run(1000);
    run(64);
    DeleteFileA("mmiobuf.wav");
    printf("%s\n", failed ? "FAILED" : "all ok");
    return failed;
}
