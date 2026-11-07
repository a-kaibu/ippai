/* CryptoAPI as games use it to decrypt their data (c4c0005: RC4 keys derived from an MD5 or SHA-1
 * hash), the hashes themselves, RtlGenRandom (the VC++ CRT's rand_s, 533f281) and rpcrt4's UUID
 * strings (95e2c26). Everything printed is deterministic except that random bytes are random.
 * libs: -ladvapi32 -lrpcrt4 */
#include "../probe.h"
#include <wincrypt.h>
#include <rpc.h>

static void hex(const BYTE *p, DWORD n) {
    for (DWORD i = 0; i < n; i++) printf("%02x", p[i]);
}

static void hash(HCRYPTPROV prov, ALG_ID alg, const char *name, const void *data, DWORD n, int parts) {
    HCRYPTHASH h;
    if (!CryptCreateHash(prov, alg, 0, 0, &h)) {
        printf("%s: CryptCreateHash err %lu\n", name, GetLastError());
        return;
    }
    DWORD step = parts > 1 ? n / parts : n;
    for (DWORD at = 0; at < n; at += step) CryptHashData(h, (const BYTE *)data + at, at + step > n ? n - at : step, 0);
    if (n == 0) CryptHashData(h, (const BYTE *)"", 0, 0);
    DWORD size = 0, len = sizeof size;
    CryptGetHashParam(h, HP_HASHSIZE, (BYTE *)&size, &len, 0);
    BYTE out[64];
    DWORD m = 2;
    SetLastError(0xdead);
    BOOL r = CryptGetHashParam(h, HP_HASHVAL, out, &m, 0);
    printf("%s %lu bytes in %d: size %lu short buffer ret %d err %lu need %lu", name, n, parts, size, r != 0, r ? 0 : GetLastError(), m);
    m = sizeof out;
    r = CryptGetHashParam(h, HP_HASHVAL, out, &m, 0);
    printf(" ret %d ", r != 0);
    hex(out, m);
    /* after HP_HASHVAL the hash is finished: more data is refused */
    SetLastError(0xdead);
    r = CryptHashData(h, (const BYTE *)"x", 1, 0);
    printf(" more data ret %d err %lx\n", r != 0, r ? 0 : GetLastError());
    CryptDestroyHash(h);
}

static void rc4(HCRYPTPROV prov, ALG_ID halg, const char *pass, DWORD flags, const char *label) {
    HCRYPTHASH h;
    HCRYPTKEY k;
    CryptCreateHash(prov, halg, 0, 0, &h);
    CryptHashData(h, (const BYTE *)pass, (DWORD)strlen(pass), 0);
    SetLastError(0xdead);
    if (!CryptDeriveKey(prov, CALG_RC4, h, flags, &k)) {
        printf("rc4 %s: CryptDeriveKey err %lx\n", label, GetLastError());
        CryptDestroyHash(h);
        return;
    }
    DWORD bits = 0, len = sizeof bits;
    CryptGetKeyParam(k, KP_KEYLEN, (BYTE *)&bits, &len, 0);
    BYTE text[64] = "The quick brown fox jumps over the lazy dog";
    DWORD n = 43;
    BOOL r = CryptEncrypt(k, 0, FALSE, 0, text, &n, sizeof text);
    printf("rc4 %s: keylen %lu encrypt ret %d n %lu ", label, bits, r != 0, n);
    hex(text, n);
    /* the stream goes on across calls; Final restarts it */
    BYTE more[8] = "abcdefg";
    n = 7;
    CryptEncrypt(k, 0, TRUE, 0, more, &n, sizeof more);
    printf(" then ");
    hex(more, n);
    BYTE again[8] = "abcdefg";
    n = 7;
    CryptEncrypt(k, 0, TRUE, 0, again, &n, sizeof again);
    printf(" after final ");
    hex(again, n);
    printf("\n");
    /* decrypting with a fresh key gives the text back */
    HCRYPTKEY k2;
    CryptDeriveKey(prov, CALG_RC4, h, flags, &k2);
    n = 43;
    r = CryptDecrypt(k2, 0, TRUE, 0, text, &n);
    printf("rc4 %s: decrypt ret %d %.*s\n", label, r != 0, (int)n, text);
    CryptDestroyKey(k2);
    BOOL d1 = CryptDestroyKey(k);
    SetLastError(0xdead);
    BOOL d2 = CryptDestroyKey(k);
    printf("rc4 %s: destroy %d, again %d err %lx\n", label, d1 != 0, d2 != 0, d2 ? 0 : GetLastError());
    CryptDestroyHash(h);
}

int main(void) {
    HCRYPTPROV prov;
    BOOL r = CryptAcquireContextA(&prov, NULL, NULL, PROV_RSA_FULL, CRYPT_VERIFYCONTEXT);
    printf("CryptAcquireContext PROV_RSA_FULL verify: %d\n", r != 0);
    if (!r) return 1;
    const char *fox = "The quick brown fox jumps over the lazy dog";
    static BYTE big[100000];
    for (DWORD i = 0; i < sizeof big; i++) big[i] = (BYTE)(i * 7 + (i >> 8));
    hash(prov, CALG_MD5, "md5", "", 0, 1);
    hash(prov, CALG_MD5, "md5", fox, 43, 1);
    hash(prov, CALG_MD5, "md5", fox, 43, 5);
    hash(prov, CALG_MD5, "md5", big, sizeof big, 7);
    hash(prov, CALG_SHA1, "sha1", "", 0, 1);
    hash(prov, CALG_SHA1, "sha1", fox, 43, 1);
    hash(prov, CALG_SHA1, "sha1", big, sizeof big, 3);
    rc4(prov, CALG_MD5, "password", 0, "md5 default");
    rc4(prov, CALG_MD5, "password", 128 << 16, "md5 128");
    rc4(prov, CALG_MD5, "password", 40 << 16, "md5 40");
    rc4(prov, CALG_MD5, "password", (128 << 16) | CRYPT_NO_SALT, "md5 128 nosalt");
    rc4(prov, CALG_MD5, "password", (40 << 16) | CRYPT_NO_SALT, "md5 40 nosalt");
    rc4(prov, CALG_SHA1, "\x82\xb1\x82\xf1\x82\xc9\x82\xbf\x82\xcd", 128 << 16, "sha1 128");
    CryptReleaseContext(prov, 0);

    HCRYPTPROV aes;
    r = CryptAcquireContextA(&aes, NULL, NULL, PROV_RSA_AES, CRYPT_VERIFYCONTEXT);
    printf("CryptAcquireContext PROV_RSA_AES verify: %d\n", r != 0);
    if (r) {
        hash(aes, CALG_SHA_256, "sha256", fox, 43, 1);
        hash(aes, CALG_SHA_256, "sha256", big, sizeof big, 2);
        CryptReleaseContext(aes, 0);
    }

    /* RtlGenRandom (advapi32!SystemFunction036) */
    BOOLEAN(WINAPI * gen)(PVOID, ULONG) = (void *)GetProcAddress(LoadLibraryA("advapi32.dll"), "SystemFunction036");
    printf("SystemFunction036 found %d\n", gen != NULL);
    if (gen) {
        BYTE a[64] = { 0 }, b[64] = { 0 };
        BOOLEAN ra = gen(a, 64), rb = gen(b, 64), rz = gen(a, 0);
        int nz = 0;
        for (int i = 0; i < 64; i++) nz += a[i] != 0;
        printf("RtlGenRandom: %d %d zero length %d differ %d nonzero bytes>32 %d\n", ra, rb, rz, memcmp(a, b, 64) != 0, nz > 32);
    }
    unsigned int rs1 = 0, rs2 = 0;
    errno_t (*rand_s_fn)(unsigned int *) = (void *)GetProcAddress(LoadLibraryA("msvcrt.dll"), "rand_s");
    if (rand_s_fn) {
        int e1 = rand_s_fn(&rs1), e2 = rand_s_fn(&rs2);
        printf("rand_s: %d %d differ %d\n", e1, e2, rs1 != rs2);
    }

    /* UUID strings */
    const char *in[] = { "6B29FC40-CA47-1067-B31D-00DD010662DA", "6b29fc40-ca47-1067-b31d-00dd010662da", "{6B29FC40-CA47-1067-B31D-00DD010662DA}",
                         "6B29FC40-CA47-1067-B31D-00DD010662D", "6B29FC40CA47-1067-B31D-00DD010662DA", "6B29FC40-CA47-1067-B31D-00DD010662DAX",
                         "G B29FC40-CA47-1067-B31D-00DD010662DA", "", "00000000-0000-0000-0000-000000000000" };
    for (int i = 0; i < 9; i++) {
        UUID u;
        memset(&u, 0xcd, sizeof u);
        RPC_STATUS st = UuidFromStringA((RPC_CSTR)in[i], &u);
        printf("UuidFromString \"%s\": %ld ", in[i], st);
        hex((BYTE *)&u, 16);
        if (st == 0) {
            RPC_CSTR s;
            UuidToStringA(&u, &s);
            printf(" back %s", s);
            RpcStringFreeA(&s);
        }
        printf("\n");
    }
    {
        UUID u;
        memset(&u, 0xcd, sizeof u);
        printf("UuidFromString NULL: %ld ", UuidFromStringA(NULL, &u));
        hex((BYTE *)&u, 16);
        printf("\n");
        RPC_WSTR w;
        UuidFromStringA((RPC_CSTR)in[0], &u);
        UuidToStringW(&u, &w);
        printf("UuidToStringW %ls\n", (wchar_t *)w);
        RpcStringFreeW(&w);
        UUID a, b;
        RPC_STATUS sa = UuidCreate(&a);
        RPC_STATUS sb = UuidCreate(&b);
        RPC_STATUS cmpst;
        printf("UuidCreate: %ld %ld version %d variant %d differ %d compare %d\n", sa, sb, a.Data3 >> 12, a.Data4[0] >> 6, memcmp(&a, &b, 16) != 0,
               UuidCompare(&a, &a, &cmpst));
        UUID nil;
        UuidCreateNil(&nil);
        printf("UuidIsNil nil %d created %d\n", UuidIsNil(&nil, &cmpst), UuidIsNil(&a, &cmpst));
        printf("UuidEqual %d %d\n", UuidEqual(&a, &a, &cmpst), UuidEqual(&a, &b, &cmpst));
    }
    return 0;
}
