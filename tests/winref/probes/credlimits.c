/* CredWrite's limits (cred.rs): a CREDENTIAL with more than CRED_MAX_ATTRIBUTES attributes, a
 * blob over CRED_MAX_CREDENTIAL_BLOB_SIZE, an attribute value over CRED_MAX_VALUE_SIZE, and the
 * largest of each that is still written. Printed: CredWrite's result and last error, and the
 * sizes CredRead gives back. The counts are the guest's: sake must check them before reading.
 * libs: -ladvapi32 */
#include "../probe.h"
#include <wincred.h>

static BYTE blob[CRED_MAX_CREDENTIAL_BLOB_SIZE + 64];
static BYTE value[CRED_MAX_VALUE_SIZE + 64];
static CREDENTIAL_ATTRIBUTEW attrs[CRED_MAX_ATTRIBUTES + 4];

static void one(const char *label, DWORD blobsize, DWORD nattrs, DWORD valsize) {
    WCHAR target[] = L"sake-winref-credlimits";
    CREDENTIALW c = {0};
    c.Type = CRED_TYPE_GENERIC;
    c.TargetName = target;
    c.CredentialBlobSize = blobsize;
    c.CredentialBlob = blob;
    c.Persist = CRED_PERSIST_SESSION;
    c.AttributeCount = nattrs;
    c.Attributes = attrs;
    for (DWORD i = 0; i < CRED_MAX_ATTRIBUTES + 4; i++) {
        static WCHAR names[CRED_MAX_ATTRIBUTES + 4][16];
        wsprintfW(names[i], L"attr%u", i);
        attrs[i].Keyword = names[i];
        attrs[i].Flags = 0;
        attrs[i].ValueSize = i == 0 ? valsize : 4;
        attrs[i].Value = value;
    }
    SetLastError(0xdeadbeef);
    BOOL ok = CredWriteW(&c, 0);
    DWORD e = GetLastError();
    printf("%s: write %d error %lu", label, ok, ok ? 0 : e);
    PCREDENTIALW got = NULL;
    if (ok && CredReadW(target, CRED_TYPE_GENERIC, 0, &got)) {
        printf(" read blob %lu attrs %lu first value %lu", got->CredentialBlobSize, got->AttributeCount,
               got->AttributeCount ? got->Attributes[0].ValueSize : 0);
        CredFree(got);
    }
    printf("\n");
    CredDeleteW(target, CRED_TYPE_GENERIC, 0);
}

int main(void) {
    memset(blob, 0x5a, sizeof blob);
    memset(value, 0x33, sizeof value);
    one("plain", 16, 0, 0);
    one("largest blob", CRED_MAX_CREDENTIAL_BLOB_SIZE, 0, 0);
    one("blob one over", CRED_MAX_CREDENTIAL_BLOB_SIZE + 1, 0, 0);
    one("most attributes", 4, CRED_MAX_ATTRIBUTES, 4);
    one("attributes one over", 4, CRED_MAX_ATTRIBUTES + 1, 4);
    one("largest value", 4, 1, CRED_MAX_VALUE_SIZE);
    one("value one over", 4, 1, CRED_MAX_VALUE_SIZE + 1);
    /* (garbage counts: Windows checks them before it reads the arrays) */
    one("attributes 0x7fffffff", 4, 0x7fffffff, 4);
    one("blob 0xffffffff", 0xffffffff, 0, 0);
    return 0;
}
