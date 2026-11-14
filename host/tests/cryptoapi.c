/* libs: -ladvapi32
 * Public-key verification using an OpenSSL-produced signature, independent of
 * sake's signing code. Also exercise rejection paths and guest pointer sizes. */
#include <windows.h>
#include <wincrypt.h>
#include <stdio.h>
#include <string.h>
#include "cryptoapi-vectors.h"

/* (the bytes the vector's signature was made over: it predates the masu rename, and without
 * the private key it cannot be signed again) */
static const BYTE message[] = "ippai CryptoAPI signature regression";
static HCRYPTPROV provider;
static HCRYPTKEY key;
static int failed;
#define CHECK(name, expr) do { int ok = !!(expr); printf("%s: %s\n", name, ok ? "ok" : "FAIL"); failed |= !ok; } while (0)
static HCRYPTHASH hash(void) {
    HCRYPTHASH h = 0;
    if (!CryptCreateHash(provider, CALG_SHA1, 0, 0, &h)) return 0;
    if (!CryptHashData(h, message, sizeof(message) - 1, 0)) return 0;
    return h;
}
int main(void) {
    CHECK("acquire", CryptAcquireContextA(&provider, NULL, NULL, PROV_RSA_FULL, CRYPT_VERIFYCONTEXT));
    CHECK("import", CryptImportKey(provider, public_blob, sizeof(public_blob), 0, 0, &key));
    if (!provider || !key) return 1;
    DWORD n = 0;
    CHECK("export size", CryptExportKey(key, 0, PUBLICKEYBLOB, 0, NULL, &n) && n == sizeof(public_blob));
    BYTE out[sizeof(public_blob)];
    memset(out, 0xa5, sizeof(out));
    n = 1;
    BOOL result = CryptExportKey(key, 0, PUBLICKEYBLOB, 0, out, &n);
    CHECK("short export", !result && GetLastError() == ERROR_MORE_DATA && n == sizeof(public_blob) && out[0] == 0xa5);
    CHECK("export roundtrip", CryptExportKey(key, 0, PUBLICKEYBLOB, 0, out, &n) && !memcmp(out, public_blob, n));
    DWORD value = 0;
    n = sizeof(value);
    CHECK("algorithm", CryptGetKeyParam(key, KP_ALGID, (BYTE *)&value, &n, 0) && value == CALG_RSA_SIGN);
    n = sizeof(value);
    CHECK("key length", CryptGetKeyParam(key, KP_KEYLEN, (BYTE *)&value, &n, 0) && value == 1024);
    n = sizeof(value);
    CHECK("block length", CryptGetKeyParam(key, KP_BLOCKLEN, (BYTE *)&value, &n, 0) && value == 1024);
    HCRYPTHASH h = hash();
    CHECK("verify A", h && CryptVerifySignatureA(h, signature, sizeof(signature), key, NULL, 0));
    result = CryptHashData(h, message, 1, 0);
    CHECK("hash finalized", !result && GetLastError() == NTE_BAD_HASH_STATE);
    CryptDestroyHash(h);
    h = hash();
    CHECK("verify W", h && CryptVerifySignatureW(h, signature, sizeof(signature), key, NULL, 0));
    CryptDestroyHash(h);
    BYTE tampered[sizeof(signature)];
    memcpy(tampered, signature, sizeof(tampered));
    tampered[13] ^= 0x10;
    h = hash();
    result = CryptVerifySignatureA(h, tampered, sizeof(tampered), key, NULL, 0);
    CHECK("reject signature", !result && GetLastError() == NTE_BAD_SIGNATURE);
    CryptDestroyHash(h);
    h = hash();
    CryptHashData(h, message, 1, 0);
    result = CryptVerifySignatureA(h, signature, sizeof(signature), key, NULL, 0);
    CHECK("reject changed data", !result && GetLastError() == NTE_BAD_SIGNATURE);
    CryptDestroyHash(h);
    h = hash();
    result = CryptVerifySignatureA(h, signature, sizeof(signature) - 1, key, NULL, 0);
    CHECK("reject short signature", !result && GetLastError() == NTE_BAD_SIGNATURE);
    CryptDestroyHash(h);
    h = hash();
    result = CryptVerifySignatureA(h, signature, sizeof(signature), key, NULL, 0x80000000);
    CHECK("reject flags", !result && GetLastError() == NTE_BAD_FLAGS);
    CryptDestroyHash(h);
    HCRYPTKEY bad = 0;
    CHECK("reject truncated blob", !CryptImportKey(provider, public_blob, sizeof(public_blob) - 1, 0, 0, &bad));
    BYTE corrupt[sizeof(public_blob)];
    memcpy(corrupt, public_blob, sizeof(corrupt));
    corrupt[8] ^= 1;
    CHECK("reject blob magic", !CryptImportKey(provider, corrupt, sizeof(corrupt), 0, 0, &bad));
    CHECK("destroy", CryptDestroyKey(key));
    h = hash();
    CHECK("reject destroyed key", !CryptVerifySignatureA(h, signature, sizeof(signature), key, NULL, 0));
    CryptDestroyHash(h);
    CHECK("release", CryptReleaseContext(provider, 0));
    return failed;
}
