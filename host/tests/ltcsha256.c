/* KiriKiri sigcheck.dll's sha256_compress (libtomcrypt built without optimization: koji's
 * Hle::Sha256 does it in wasm) run from executable memory the way the plugin runs it: a
 * sha256_process/sha256_done around it hashing whole buffers, called hot, and from two threads
 * at once (the plugin checks the exe and data.xp3 on two). Every digest is checked against a
 * plain C SHA-256; prints the digests and "ok" or "BAD". (x86 machine code: 32-bit only.) */
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include "ltcsha256_code.h"

/* libtomcrypt's struct sha256_state (ulong64 length; ulong32 state[8], curlen; unsigned char buf[64]) */
struct md {
    unsigned long long length;
    unsigned state[8], curlen;
    unsigned char buf[64];
};
typedef int (*compress_fn)(struct md *, const unsigned char *);
static compress_fn compress;

static const unsigned IV[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};

/* libtomcrypt's sha256_process and sha256_done, around the guest function */
static int process(struct md *md, const unsigned char *in, unsigned long n)
{
    int bad = 0;
    while (n > 0) {
        if (md->curlen == 0 && n >= 64) {
            bad |= compress(md, in);
            md->length += 512;
            in += 64;
            n -= 64;
        } else {
            unsigned k = 64 - md->curlen < n ? 64 - md->curlen : n;
            memcpy(md->buf + md->curlen, in, k);
            md->curlen += k;
            in += k;
            n -= k;
            if (md->curlen == 64) {
                bad |= compress(md, md->buf);
                md->length += 512;
                md->curlen = 0;
            }
        }
    }
    return bad;
}

static int done(struct md *md, unsigned char out[32])
{
    int bad = 0;
    md->length += md->curlen * 8ull;
    md->buf[md->curlen++] = 0x80;
    if (md->curlen > 56) {
        while (md->curlen < 64) md->buf[md->curlen++] = 0;
        bad |= compress(md, md->buf);
        md->curlen = 0;
    }
    while (md->curlen < 56) md->buf[md->curlen++] = 0;
    for (int i = 0; i < 8; i++) md->buf[56 + i] = (unsigned char)(md->length >> (56 - 8 * i));
    bad |= compress(md, md->buf);
    for (int i = 0; i < 8; i++)
        for (int j = 0; j < 4; j++) out[4 * i + j] = (unsigned char)(md->state[i] >> (24 - 8 * j));
    return bad;
}

/* the reference */
#define ROR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))
static const unsigned K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};
static int ref_compress(struct md *md, const unsigned char *b)
{
    unsigned w[64], v[8];
    for (int i = 0; i < 16; i++) w[i] = (unsigned)b[4 * i] << 24 | b[4 * i + 1] << 16 | b[4 * i + 2] << 8 | b[4 * i + 3];
    for (int i = 16; i < 64; i++)
        w[i] = (ROR(w[i - 2], 17) ^ ROR(w[i - 2], 19) ^ (w[i - 2] >> 10)) + w[i - 7] + (ROR(w[i - 15], 7) ^ ROR(w[i - 15], 18) ^ (w[i - 15] >> 3)) + w[i - 16];
    memcpy(v, md->state, sizeof v);
    for (int i = 0; i < 64; i++) {
        unsigned t1 = v[7] + (ROR(v[4], 6) ^ ROR(v[4], 11) ^ ROR(v[4], 25)) + ((v[4] & v[5]) ^ (~v[4] & v[6])) + K[i] + w[i];
        unsigned t2 = (ROR(v[0], 2) ^ ROR(v[0], 13) ^ ROR(v[0], 22)) + ((v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]));
        memmove(v + 1, v, 7 * sizeof *v);
        v[4] += t1;
        v[0] = t1 + t2;
    }
    for (int i = 0; i < 8; i++) md->state[i] += v[i];
    return 0;
}

static void digest(const unsigned char *in, unsigned long n, compress_fn f, unsigned char out[32], int *bad)
{
    struct md md = {0};
    memcpy(md.state, IV, sizeof IV);
    compress = f;
    *bad |= process(&md, in, n);
    *bad |= done(&md, out);
}

static unsigned char *data;
#define DATA (1 << 20)
static compress_fn guest;
static char lines[2][64];

/* hashes buffers of every length class (whole blocks, partial, unaligned starts) `rounds` times */
static DWORD WINAPI hasher(void *arg)
{
    int id = (int)(INT_PTR)arg, bad = 0, wrong = 0;
    unsigned sum = 0;
    for (int r = 0; r < 24; r++) {
        unsigned long n = (r * 40503u + id * 977u) % DATA, at = (r * 7u + id) % 61;
        unsigned char a[32], b[32];
        struct md m1 = {0}, m2 = {0};
        memcpy(m1.state, IV, sizeof IV);
        memcpy(m2.state, IV, sizeof IV);
        /* (each thread its own pointer: compress is shared, so the guest one is called directly) */
        for (unsigned long k = 0; k < n / 64; k++) {
            bad |= guest(&m1, data + at + 64 * k);
            ref_compress(&m2, data + at + 64 * k);
        }
        memcpy(a, m1.state, 32);
        memcpy(b, m2.state, 32);
        wrong += memcmp(a, b, 32) != 0;
        sum = sum * 31 + m1.state[0] + m1.state[7];
    }
    snprintf(lines[id], sizeof lines[id], "thread %d: %08x %s\n", id, sum, bad || wrong ? "BAD" : "ok");
    return 0;
}

int main(void)
{
    guest = VirtualAlloc(NULL, sizeof ltc_sha256_compress, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    memcpy((void *)guest, ltc_sha256_compress, sizeof ltc_sha256_compress);
    data = malloc(DATA + 64);
    unsigned x = 12345;
    for (int i = 0; i < DATA + 64; i++) data[i] = (unsigned char)((x = x * 1103515245 + 12345) >> 16);

    unsigned char a[32], b[32];
    int bad = 0;
    digest((const unsigned char *)"abc", 3, guest, a, &bad);
    printf("abc:");
    for (int i = 0; i < 32; i++) printf("%02x", a[i]);
    printf(" %s\n", bad ? "BAD" : "ok");
    /* hot: every length 0..200 and then the whole buffer, guest against reference */
    int wrong = 0;
    for (int n = 0; n <= 200; n++) {
        digest(data + (n & 7), n, guest, a, &bad);
        digest(data + (n & 7), n, ref_compress, b, &bad);
        wrong += memcmp(a, b, 32) != 0;
    }
    printf("lengths 0..200: %s\n", wrong || bad ? "BAD" : "ok");
    digest(data, DATA, guest, a, &bad);
    digest(data, DATA, ref_compress, b, &bad);
    printf("1 MiB:");
    for (int i = 0; i < 32; i++) printf("%02x", a[i]);
    printf(" %s\n", memcmp(a, b, 32) || bad ? "BAD" : "ok");
    HANDLE t[2];
    for (int i = 0; i < 2; i++) t[i] = CreateThread(NULL, 0, hasher, (void *)(INT_PTR)i, CREATE_SUSPENDED, NULL);
    for (int i = 0; i < 2; i++) ResumeThread(t[i]);
    WaitForMultipleObjects(2, t, TRUE, INFINITE);
    printf("%s%s", lines[0], lines[1]);
    return 0;
}
