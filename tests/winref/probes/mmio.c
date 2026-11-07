/* winmm mmio as games read and write WAVE/AVI-style RIFF files: mmioOpen on a memory buffer
 * and on a file, mmioDescend (RIFF/LIST types, FINDCHUNK, missing chunks, odd sizes and their
 * pad byte), mmioAscend, mmioRead/Seek past the ends, mmioCreateChunk + mmioWrite writing a
 * RIFF (sizes patched on ascend), mmioGetInfo/Advance buffered access, mmioStringToFOURCC,
 * and the error codes. Works in "wr-mmio".
 * libs: -lwinmm */
#include "../probe.h"
#include <mmsystem.h>

static void ck(const char *l, MMRESULT r, MMCKINFO *c) {
    printf("%s: %u", l, r);
    if (!r) printf(" id %.4s size %lu type %.4s offset %lu flags %lx", (char *)&c->ckid, c->cksize, c->fccType ? (char *)&c->fccType : "----", c->dwDataOffset, c->dwFlags);
    printf("\n");
}

/* RIFF WAVE: "fmt " (16), an odd "junk" chunk (3 bytes + pad), LIST INFO with INAM, "data" (8) */
static BYTE riff[] = {
    'R','I','F','F', 0,0,0,0, 'W','A','V','E', /* (size set in main) */
    'f','m','t',' ', 16,0,0,0, 1,0, 1,0, 0x22,0x56,0,0, 0x44,0xac,0,0, 2,0, 16,0,
    'j','u','n','k', 3,0,0,0, 'a','b','c', 0,
    'L','I','S','T', 14,0,0,0, 'I','N','F','O', 'I','N','A','M', 2,0,0,0, 'x',0,
    'd','a','t','a', 8,0,0,0, 1,2,3,4,5,6,7,8,
};

static void walk(HMMIO h) {
    MMCKINFO r = { 0 }, c = { 0 }, l = { 0 };
    ck("descend riff", mmioDescend(h, &r, NULL, 0), &r);
    ck("descend first", mmioDescend(h, &c, &r, 0), &c);
    char b[32];
    printf("read %ld", mmioRead(h, b, 4));
    printf(" pos %ld\n", mmioSeek(h, 0, SEEK_CUR));
    ck("ascend", mmioAscend(h, &c, 0), &c);
    printf("pos %ld\n", mmioSeek(h, 0, SEEK_CUR));
    ck("descend next", mmioDescend(h, &c, &r, 0), &c);
    ck("ascend odd", mmioAscend(h, &c, 0), &c);
    printf("pos after odd %ld\n", mmioSeek(h, 0, SEEK_CUR));
    l.fccType = mmioFOURCC('I', 'N', 'F', 'O');
    ck("find list", mmioDescend(h, &l, &r, MMIO_FINDLIST), &l);
    c.ckid = mmioFOURCC('I', 'N', 'A', 'M');
    ck("find in list", mmioDescend(h, &c, &l, MMIO_FINDCHUNK), &c);
    ck("ascend in", mmioAscend(h, &c, 0), &c);
    ck("ascend list", mmioAscend(h, &l, 0), &l);
    c.ckid = mmioFOURCC('d', 'a', 't', 'a');
    ck("find data", mmioDescend(h, &c, &r, MMIO_FINDCHUNK), &c);
    memset(b, 0, sizeof b);
    LONG n = mmioRead(h, b, 20);
    printf("read past %ld:", n);
    for (int i = 0; i < n; i++) printf(" %d", b[i]);
    printf(" again %ld\n", mmioRead(h, b, 4));
    printf("seek end %ld set %ld past %ld neg %ld\n", mmioSeek(h, 0, SEEK_END), mmioSeek(h, 12, SEEK_SET), mmioSeek(h, 1000, SEEK_SET), mmioSeek(h, -5, SEEK_SET));
    mmioSeek(h, 12, SEEK_SET);
    c.ckid = mmioFOURCC('n', 'o', 'n', 'e');
    ck("find missing", mmioDescend(h, &c, &r, MMIO_FINDCHUNK), &c);
    printf("pos after missing %ld\n", mmioSeek(h, 0, SEEK_CUR));
    mmioSeek(h, 0, SEEK_SET);
    r.fccType = mmioFOURCC('A', 'V', 'I', ' ');
    ck("find riff avi", mmioDescend(h, &r, NULL, MMIO_FINDRIFF), &r);
}

int main(void) {
    riff[4] = sizeof riff - 8;
    printf("fourcc %08lx %08lx %08lx\n", mmioStringToFOURCCA("wave", 0), mmioStringToFOURCCA("wave", MMIO_TOUPPER), mmioStringToFOURCCA("ab", 0));

    /* a memory file */
    MMIOINFO mi = { 0 };
    mi.fccIOProc = FOURCC_MEM;
    mi.pchBuffer = (char *)riff;
    mi.cchBuffer = sizeof riff;
    HMMIO h = mmioOpenA(NULL, &mi, MMIO_READ);
    printf("open mem %d err %u\n", h != NULL, mi.wErrorRet);
    walk(h);
    MMIOINFO gi;
    mmioSeek(h, 4, SEEK_SET);
    printf("getinfo %u", mmioGetInfo(h, &gi, 0));
    printf(" next %ld end %ld buf %ld disk %ld\n", (long)(gi.pchNext - gi.pchBuffer), (long)(gi.pchEndRead - gi.pchBuffer), gi.cchBuffer, gi.lDiskOffset);
    printf("close %u\n", mmioClose(h, 0));

    /* the same bytes in a file */
    CreateDirectoryA("wr-mmio", NULL);
    HANDLE f = CreateFileA("wr-mmio\\a.wav", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD w;
    WriteFile(f, riff, sizeof riff, &w, NULL);
    CloseHandle(f);
    h = mmioOpenA("wr-mmio\\a.wav", NULL, MMIO_READ);
    printf("open file %d\n", h != NULL);
    walk(h);
    mmioClose(h, 0);
    h = mmioOpenA("wr-mmio\\a.wav", NULL, MMIO_READ | MMIO_ALLOCBUF);
    printf("open buffered %d\n", h != NULL);
    walk(h);
    mmioClose(h, 0);
    MMIOINFO ei = { 0 };
    h = mmioOpenA("wr-mmio\\none.wav", &ei, MMIO_READ);
    printf("open missing %d err %u\n", h != NULL, ei.wErrorRet);
    printf("exist %d\n", mmioOpenA("wr-mmio\\a.wav", NULL, MMIO_EXIST) != NULL);

    /* writing a RIFF: sizes come from ascend */
    h = mmioOpenA("wr-mmio\\out.wav", NULL, MMIO_CREATE | MMIO_WRITE);
    MMCKINFO r = { 0 }, c = { 0 };
    r.fccType = mmioFOURCC('W', 'A', 'V', 'E');
    ck("create riff", mmioCreateChunk(h, &r, MMIO_CREATERIFF), &r);
    c.ckid = mmioFOURCC('f', 'm', 't', ' ');
    ck("create fmt", mmioCreateChunk(h, &c, 0), &c);
    printf("write %ld\n", mmioWrite(h, (char *)riff + 20, 16));
    ck("ascend fmt", mmioAscend(h, &c, 0), &c);
    c.ckid = mmioFOURCC('o', 'd', 'd', ' ');
    c.cksize = 0;
    mmioCreateChunk(h, &c, 0);
    printf("write odd %ld\n", mmioWrite(h, "xyz", 3));
    ck("ascend odd", mmioAscend(h, &c, 0), &c);
    ck("ascend riff", mmioAscend(h, &r, 0), &r);
    printf("close %u\n", mmioClose(h, 0));
    f = CreateFileA("wr-mmio\\out.wav", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    BYTE out[128];
    DWORD n = 0;
    ReadFile(f, out, sizeof out, &n, NULL);
    CloseHandle(f);
    printf("written %lu:", n);
    for (DWORD i = 0; i < n; i++) printf(" %02x", out[i]);
    printf("\n");
    return 0;
}
