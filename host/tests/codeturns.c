/* Code rewritten between a few versions, over and over, in one PAGE_EXECUTE_READWRITE buffer
 * with no protection change or cache flush: what RPG Maker's RGSS does with its sprite blend
 * loops (a plain one and a zooming one take turns many times a frame, #233). Each version is a
 * loop hot enough to be translated; the turns go well past the JIT's limit for code that keeps
 * changing (256), a third version shows up late, and a version differing from another only in
 * an immediate takes turns with it. Prints what each round of calls returned (the same on
 * Windows, with the JIT and without). */
#include <stdio.h>
#include <string.h>
#include <windows.h>

typedef unsigned (*fn)(const unsigned *, unsigned);

/* push esi; mov esi,[esp+8]; mov ecx,[esp+12]; xor eax,eax;
 * loop: <op> eax,[esi]; add esi,4; dec ecx; jnz loop; pop esi; ret */
static void make(unsigned char *p, int version, unsigned k)
{
    unsigned char *q = p;
    static const unsigned char head[] = { 0x56, 0x8b, 0x74, 0x24, 0x08, 0x8b, 0x4c, 0x24, 0x0c, 0x31, 0xc0 };
    memcpy(q, head, sizeof(head));
    q += sizeof(head);
    unsigned char *loop = q;
    switch (version)
    {
    case 0: /* add eax,[esi] */
        *q++ = 0x03, *q++ = 0x06;
        break;
    case 1: /* mov edx,[esi]; imul edx,edx,k; add eax,edx */
        *q++ = 0x8b, *q++ = 0x16;
        *q++ = 0x69, *q++ = 0xd2;
        memcpy(q, &k, 4), q += 4;
        *q++ = 0x01, *q++ = 0xd0;
        break;
    default: /* xor eax,[esi]; rol eax,3 */
        *q++ = 0x33, *q++ = 0x06;
        *q++ = 0xc1, *q++ = 0xc0, *q++ = 0x03;
        break;
    }
    *q++ = 0x83, *q++ = 0xc6, *q++ = 0x04; /* add esi,4 */
    *q++ = 0x49;                           /* dec ecx */
    *q++ = 0x75, *q = (unsigned char)(loop - (q + 1)), q++; /* jnz loop */
    *q++ = 0x5e, *q++ = 0xc3;              /* pop esi; ret */
    memset(q, 0xcc, 16);
}

static unsigned want(const unsigned *v, unsigned n, int version, unsigned k)
{
    unsigned a = 0;
    for (unsigned i = 0; i < n; i++)
    {
        if (version == 0) a += v[i];
        else if (version == 1) a += v[i] * k;
        else a = ((a ^ v[i]) << 3) | ((a ^ v[i]) >> 29);
    }
    return a;
}

int main(void)
{
    unsigned char *p = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    static unsigned v[640];
    for (unsigned i = 0; i < 640; i++) v[i] = i * 2654435761u;
    unsigned bad = 0, sum = 0;
    /* (two versions in turns; from turn 500 a third joins; versions 1 with k 3 and 5 alternate
     * in the last 200: only an immediate differs) */
    for (int turn = 0; turn < 900; turn++)
    {
        int version = turn < 500 ? turn & 1 : turn < 700 ? turn % 3 : 1;
        unsigned k = turn < 700 ? 3 : (turn & 1) ? 5 : 3;
        make(p, version, k);
        for (int call = 0; call < 8; call++)
        {
            unsigned n = 64 + (unsigned)(turn * 7 + call * 13) % 576;
            unsigned got = ((fn)p)(v, n);
            if (got != want(v, n, version, k)) bad++;
            sum += got;
        }
    }
    printf("codeturns %08x %s\n", sum, bad ? "STALE" : "ok");
    if (bad) printf("%u calls ran an old version\n", bad);
    return 0;
}
