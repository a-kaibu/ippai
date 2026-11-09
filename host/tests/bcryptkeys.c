/* libs: -lbcrypt */
/* bcrypt's asymmetric keys as Inno Setup 6.7 checks its files' signatures with them:
 * ECDSA P-256 public key imported from a BCRYPT_ECCPUBLIC_BLOB and a signature made by
 * openssl verified (and refused when a byte of it or of the hash changes); a key pair
 * generated, finalized, exported and imported again, signing and verifying; a public blob
 * imported into the wrong algorithm; RSA 1024 generated with PKCS#1 v1.5 signatures; and
 * on x86 that each call pops its arguments (stdcall: ISCC, which keeps no frame, crashed when
 * one did not).
 * (sake: cng.rs BCrypt*; compared with Windows by tests/winref)
 */
#include <windows.h>
#include <bcrypt.h>
#include <stdio.h>
#include <string.h>

static const UCHAR X[32] = {0x13, 0xb9, 0xea, 0x3e, 0xb3, 0x10, 0x97, 0xf1, 0x3f, 0xf1, 0xd6, 0x1b, 0xf7, 0xcb, 0x6c, 0x51, 0x06, 0xa1, 0xec, 0xff, 0x71, 0xf1, 0x4d, 0x6b, 0xa1, 0x3a, 0x46, 0x27, 0xe6, 0x28, 0x79, 0xee};
static const UCHAR Y[32] = {0x1b, 0xa8, 0x95, 0xc3, 0x29, 0x25, 0xa0, 0x37, 0x9b, 0x35, 0x74, 0x00, 0xa2, 0x10, 0x4b, 0x9e, 0x5f, 0xbb, 0x6d, 0x67, 0x04, 0x70, 0x1c, 0xcc, 0xe6, 0x37, 0x5e, 0x33, 0xa3, 0x72, 0x45, 0xea};
static const UCHAR SIG[64] = {0xbd, 0xd6, 0xae, 0x3a, 0x18, 0xc8, 0xb5, 0x38, 0xf3, 0x7f, 0x78, 0x88, 0x0a, 0x66, 0x4c, 0x6c, 0x6f, 0x6f, 0x15, 0x3f, 0xf1, 0x9f, 0xb2, 0xa9, 0xa0, 0x6e, 0x8e, 0xb1, 0x49, 0x86, 0x23, 0xd7, 0xb4, 0xec, 0xb8, 0x4c, 0x8b, 0xff, 0x77, 0x10, 0xc7, 0x6e, 0x28, 0x90, 0x78, 0xc8, 0x4a, 0xe7, 0xc3, 0xd2, 0xdf, 0xb3, 0x4f, 0x90, 0xf0, 0x07, 0xbd, 0x66, 0x7d, 0x65, 0x65, 0x5d, 0xf7, 0x9e};
/* SHA-256 of "ippai bcrypt" */
static const UCHAR HASH[32] = {0x25, 0x6b, 0x15, 0x9e, 0xd5, 0x45, 0x31, 0x0a, 0x2c, 0x08, 0x77, 0xb6, 0xb3, 0x40, 0x4f, 0x98, 0xb6, 0x8e, 0x4c, 0x12, 0xca, 0xfc, 0x79, 0x2e, 0x19, 0x11, 0x35, 0xad, 0x98, 0x83, 0xe9, 0x60};

/* How far the stack moved across a call of f with n dword arguments: 0 when the callee pops
 * them as stdcall does (x86; elsewhere nothing to see). */
static void *g_f;
static int g_n;
static DWORD g_a[8];
static int pops(const char *name, void *f, int n, const DWORD *a) {
    int d = 0;
#ifdef __i386__
    g_f = f;
    g_n = n;
    memcpy(g_a, a, n * sizeof *a);
    __asm__ volatile(
        "mov %%esp, %%edi\n"
        "mov %1, %%ecx\n"
        "1: test %%ecx, %%ecx\n"
        "jz 2f\n"
        "dec %%ecx\n"
        "pushl %3(,%%ecx,4)\n"
        "jmp 1b\n"
        "2: call *%2\n"
        "mov %%esp, %0\n"
        "sub %%edi, %0\n"
        "mov %%edi, %%esp\n"
        : "=a"(d) : "m"(g_n), "m"(g_f), "m"(g_a) : "ecx", "edx", "edi", "memory");
#else
    (void)f, (void)n, (void)a;
#endif
    printf("%s pops its arguments: %s\n", name, d ? "no" : "yes");
    return d;
}

int main(void) {
    BCRYPT_ALG_HANDLE ec, ecdh, rsa;
    NTSTATUS st = BCryptOpenAlgorithmProvider(&ec, BCRYPT_ECDSA_P256_ALGORITHM, NULL, 0);
    printf("open ECDSA_P256: %lx\n", st);
    BCryptOpenAlgorithmProvider(&ecdh, BCRYPT_ECDH_P256_ALGORITHM, NULL, 0);
    BCryptOpenAlgorithmProvider(&rsa, BCRYPT_RSA_ALGORITHM, NULL, 0);

    UCHAR blob[sizeof(BCRYPT_ECCKEY_BLOB) + 64];
    BCRYPT_ECCKEY_BLOB *hdr = (BCRYPT_ECCKEY_BLOB *)blob;
    hdr->dwMagic = BCRYPT_ECDSA_PUBLIC_P256_MAGIC;
    hdr->cbKey = 32;
    memcpy(blob + sizeof *hdr, X, 32);
    memcpy(blob + sizeof *hdr + 32, Y, 32);
    BCRYPT_KEY_HANDLE pub;
    printf("import public: %lx\n", BCryptImportKeyPair(ec, NULL, BCRYPT_ECCPUBLIC_BLOB, &pub, blob, sizeof blob, 0));
    printf("verify openssl's signature: %lx\n", BCryptVerifySignature(pub, NULL, (PUCHAR)HASH, 32, (PUCHAR)SIG, 64, 0));
    UCHAR bad[64];
    memcpy(bad, SIG, 64);
    bad[40] ^= 1;
    printf("verify, signature changed: %lx\n", BCryptVerifySignature(pub, NULL, (PUCHAR)HASH, 32, bad, 64, 0));
    UCHAR h2[32];
    memcpy(h2, HASH, 32);
    h2[0] ^= 0x80;
    printf("verify, hash changed: %lx\n", BCryptVerifySignature(pub, NULL, h2, 32, (PUCHAR)SIG, 64, 0));
    printf("verify, short signature: %lx\n", BCryptVerifySignature(pub, NULL, (PUCHAR)HASH, 32, (PUCHAR)SIG, 63, 0));
    BCRYPT_KEY_HANDLE wrong;
    printf("import into ECDH_P256: %lx\n", BCryptImportKeyPair(ecdh, NULL, BCRYPT_ECCPUBLIC_BLOB, &wrong, blob, sizeof blob, 0));
    blob[sizeof *hdr + 5] ^= 1;
    printf("import, point off the curve: %lx\n", BCryptImportKeyPair(ec, NULL, BCRYPT_ECCPUBLIC_BLOB, &wrong, blob, sizeof blob, 0));
    UCHAR sig[64];
    ULONG n = 0;
    printf("sign with a public key fails: %d\n", BCryptSignHash(pub, NULL, (PUCHAR)HASH, 32, sig, sizeof sig, &n, 0) != 0);
    st = BCryptDestroyKey(pub);
    printf("destroy: %lx, again: %lx\n", st, BCryptDestroyKey(pub));

    BCRYPT_KEY_HANDLE pair, back;
    printf("generate 384 on P-256: %lx\n", BCryptGenerateKeyPair(ec, &pair, 384, 0));
    printf("generate: %lx\n", BCryptGenerateKeyPair(ec, &pair, 256, 0));
    printf("export before finalizing fails: %d\n", BCryptExportKey(pair, NULL, BCRYPT_ECCPUBLIC_BLOB, NULL, 0, &n, 0) != 0);
    st = BCryptFinalizeKeyPair(pair, 0);
    printf("finalize: %lx, again: %d\n", st, BCryptFinalizeKeyPair(pair, 0) != 0);
    st = BCryptExportKey(pair, NULL, BCRYPT_ECCPUBLIC_BLOB, NULL, 0, &n, 0);
    printf("public blob size: %lx %lu\n", st, n);
    UCHAR pb[256];
    st = BCryptExportKey(pair, NULL, BCRYPT_ECCPUBLIC_BLOB, pb, 10, &n, 0);
    printf("export, small buffer: %lx\n", st);
    st = BCryptExportKey(pair, NULL, BCRYPT_ECCPUBLIC_BLOB, pb, sizeof pb, &n, 0);
    printf("export: %lx %lu magic %08lx\n", st, n, ((BCRYPT_ECCKEY_BLOB *)pb)->dwMagic);
    st = BCryptExportKey(pair, NULL, BCRYPT_ECCPRIVATE_BLOB, NULL, 0, &n, 0);
    printf("private blob size: %lx %lu\n", st, n);
    st = BCryptSignHash(pair, NULL, (PUCHAR)HASH, 32, NULL, 0, &n, 0);
    printf("sign, size: %lx %lu\n", st, n);
    st = BCryptSignHash(pair, NULL, (PUCHAR)HASH, 32, sig, sizeof sig, &n, 0);
    printf("sign: %lx %lu\n", st, n);
    printf("import exported: %lx\n", BCryptImportKeyPair(ec, NULL, BCRYPT_ECCPUBLIC_BLOB, &back, pb, 8 + 64, 0));
    printf("verify with it: %lx\n", BCryptVerifySignature(back, NULL, (PUCHAR)HASH, 32, sig, 64, 0));
    BCryptDestroyKey(back);
    BCryptDestroyKey(pair);

    BCRYPT_KEY_HANDLE rk;
    st = BCryptGenerateKeyPair(rsa, &rk, 1024, 0);
    printf("RSA generate: %lx, finalize: %lx\n", st, BCryptFinalizeKeyPair(rk, 0));
    BCRYPT_PKCS1_PADDING_INFO pad = {BCRYPT_SHA256_ALGORITHM};
    UCHAR rs[128];
    st = BCryptSignHash(rk, &pad, (PUCHAR)HASH, 32, rs, sizeof rs, &n, BCRYPT_PAD_PKCS1);
    printf("RSA sign: %lx %lu\n", st, n);
    printf("RSA verify: %lx\n", BCryptVerifySignature(rk, &pad, (PUCHAR)HASH, 32, rs, n, BCRYPT_PAD_PKCS1));
    rs[7] ^= 1;
    printf("RSA verify, changed: %lx\n", BCryptVerifySignature(rk, &pad, (PUCHAR)HASH, 32, rs, n, BCRYPT_PAD_PKCS1));
    st = BCryptExportKey(rk, NULL, BCRYPT_RSAPUBLIC_BLOB, NULL, 0, &n, 0);
    printf("RSA public blob: %lx %lu\n", st, n);
    BCryptDestroyKey(rk);

    BCRYPT_KEY_HANDLE k2 = 0, k3 = 0;
    DWORD a1[] = {(DWORD)(DWORD_PTR)ec, (DWORD)(DWORD_PTR)&k2, 256, 0};
    pops("BCryptGenerateKeyPair", BCryptGenerateKeyPair, 4, a1);
    DWORD a2[] = {(DWORD)(DWORD_PTR)k2, 0};
    pops("BCryptFinalizeKeyPair", BCryptFinalizeKeyPair, 2, a2);
    DWORD a3[] = {(DWORD)(DWORD_PTR)k2, 0, (DWORD)(DWORD_PTR)HASH, 32, (DWORD)(DWORD_PTR)sig, sizeof sig, (DWORD)(DWORD_PTR)&n, 0};
    pops("BCryptSignHash", BCryptSignHash, 8, a3);
    DWORD a4[] = {(DWORD)(DWORD_PTR)k2, 0, (DWORD)(DWORD_PTR)HASH, 32, (DWORD)(DWORD_PTR)sig, 64, 0};
    pops("BCryptVerifySignature", BCryptVerifySignature, 7, a4);
    DWORD a5[] = {(DWORD)(DWORD_PTR)ec, 0, (DWORD)(DWORD_PTR)BCRYPT_ECCPUBLIC_BLOB, (DWORD)(DWORD_PTR)&k3, (DWORD)(DWORD_PTR)blob, sizeof blob, 0};
    pops("BCryptImportKeyPair", BCryptImportKeyPair, 7, a5);
    DWORD a6[] = {(DWORD)(DWORD_PTR)k2, 0, (DWORD)(DWORD_PTR)BCRYPT_ECCPUBLIC_BLOB, 0, 0, (DWORD)(DWORD_PTR)&n, 0};
    pops("BCryptExportKey", BCryptExportKey, 7, a6);
#ifdef __i386__
    BCryptDestroyKey(k3);
    BCryptDestroyKey(k2);
#endif
    printf("close: %lx\n", BCryptCloseAlgorithmProvider(ec, 0));
    BCryptCloseAlgorithmProvider(ecdh, 0);
    BCryptCloseAlgorithmProvider(rsa, 0);
    return 0;
}
