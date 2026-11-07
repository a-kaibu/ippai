/* crypt32's CryptBinaryToString / CryptStringToBinary (cryptstr.rs): base64 with and without the
 * PEM lines, spaced and raw hex, the line-break flags, the length rules (NULL buffer, too small a
 * buffer for A and W) and the lenient decoding (white space, short groups, padding, garbage
 * before a header, separators in hex; spaced hex as CryptBinaryToString writes it).
 * libs: -lcrypt32 */
#include "../probe.h"
#include <wincrypt.h>

static void show(const char *s, DWORD n) {
    putchar('"');
    for (DWORD i = 0; i < n; i++) {
        unsigned char c = s[i];
        if (c == '\r') printf("\\r");
        else if (c == '\n') printf("\\n");
        else if (c < 32 || c > 126) printf("\\x%02x", c);
        else putchar(c);
    }
    putchar('"');
}

static void enc(const BYTE *d, DWORD n, DWORD fmt) {
    DWORD len = 0;
    BOOL r = CryptBinaryToStringA(d, n, fmt, NULL, &len);
    printf("encode %lu bytes fmt %08lx: ret %d need %lu", n, fmt, r, len);
    char buf[2048];
    DWORD got = sizeof buf;
    if (r && CryptBinaryToStringA(d, n, fmt, buf, &got)) {
        printf(" wrote %lu ", got);
        show(buf, got);
    }
    WCHAR wb[2048];
    DWORD wl = 0, wg = 2048;
    BOOL wr = CryptBinaryToStringW(d, n, fmt, NULL, &wl);
    int same = wr && CryptBinaryToStringW(d, n, fmt, wb, &wg) && wg == got;
    for (DWORD i = 0; same && i < got; i++) same = wb[i] == (BYTE)buf[i];
    printf(" W need %lu same %d\n", wl, same);
}

static void small(const BYTE *d, DWORD n, DWORD fmt, DWORD cap) {
    char buf[64];
    memset(buf, 0xcc, sizeof buf);
    DWORD len = cap;
    SetLastError(0xdeadbeef);
    BOOL r = CryptBinaryToStringA(d, n, fmt, buf, &len);
    printf("A cap %lu fmt %08lx: ret %d err %lu len %lu bytes", cap, fmt, r, GetLastError(), len);
    for (DWORD i = 0; i < cap + 1 && i < 12; i++) printf(" %02x", (BYTE)buf[i]);
    WCHAR wb[64];
    memset(wb, 0xcc, sizeof wb);
    len = cap;
    SetLastError(0xdeadbeef);
    r = CryptBinaryToStringW(d, n, fmt, wb, &len);
    printf(" | W ret %d err %lu len %lu first %04x\n", r, GetLastError(), len, wb[0]);
}

static void dec(const char *s, DWORD n, DWORD fmt) {
    BYTE buf[64];
    memset(buf, 0xcc, sizeof buf);
    DWORD len = sizeof buf, skip = 0xdead, used = 0xdead;
    SetLastError(0xdeadbeef);
    BOOL r = CryptStringToBinaryA(s, n, fmt, buf, &len, &skip, &used);
    printf("decode ");
    show(s, n ? n : (DWORD)strlen(s));
    printf(" fmt %lu: ret %d", fmt, r);
    if (r) {
        printf(" len %lu skip %lu used %lu bytes ", len, skip, used);
        for (DWORD i = 0; i < len; i++) printf("%02x", buf[i]);
    } else {
        printf(" err %lu len %lu first %02x skip %lx used %lx", GetLastError(), len, buf[0], skip, used);
    }
    DWORD nl = 0xdead;
    SetLastError(0xdeadbeef);
    r = CryptStringToBinaryA(s, n, fmt, NULL, &nl, NULL, NULL);
    printf(" | NULL buffer ret %d len %lu\n", r, nl);
}

int main(void) {
    BYTE d[300];
    for (int i = 0; i < 300; i++) d[i] = (BYTE)(i * 7 + 1);
    const DWORD fl[] = {0, CRYPT_STRING_NOCR, CRYPT_STRING_NOCRLF};
    const DWORD fm[] = {CRYPT_STRING_BASE64, CRYPT_STRING_BASE64HEADER, CRYPT_STRING_BASE64REQUESTHEADER,
                        CRYPT_STRING_BASE64X509CRLHEADER, CRYPT_STRING_HEX, CRYPT_STRING_HEXRAW, CRYPT_STRING_BINARY};
    const DWORD sizes[] = {0, 1, 2, 3, 8, 16, 17, 47, 48, 49, 100};
    for (int f = 0; f < 7; f++)
        for (int l = 0; l < 3; l++)
            for (int k = 0; k < 11; k++) enc(d, sizes[k], fm[f] | fl[l]);
    for (DWORD cap = 0; cap < 8; cap++) {
        small(d, 3, CRYPT_STRING_BASE64, cap);
        small(d, 3, CRYPT_STRING_HEX, cap);
        small(d, 3, CRYPT_STRING_HEXRAW, cap);
    }
    printf("NULL data: %d err %lu\n", CryptBinaryToStringA(NULL, 0, 0, NULL, &(DWORD){0}), GetLastError());

    const char *b64[] = {"V", "VV", "VVV", "V=", "VV=", "V==", "V===", "VV==", "V========", "VV    VV", "V\nVVV",
                         "V===VVVV", "VVVV=VVVV", "VV-V", "AA==\r\n", "garbage\r\nAA==\r\n", "garbage\r\nAAAA\r\n", ""};
    for (int i = 0; i < 18; i++) {
        dec(b64[i], 0, CRYPT_STRING_BASE64);
        dec(b64[i], 0, CRYPT_STRING_BASE64_ANY);
    }
    dec("VVVVVVVV", 1, CRYPT_STRING_BASE64);
    const char *hdr[] = {"-----BEGIN-----VVVV-----END-----", "-----BEGIN -----VVVV-----END -----",
                         "junk-----BEGIN foo-----V-----END foo-----", "-----BEGIN X509 CRL-----\r\nAA==\r\n",
                         "x\r\n-----BEGIN CERTIFICATE-----\r\nAQI=\r\n-----END CERTIFICATE-----\r\n"};
    for (int i = 0; i < 5; i++)
        for (DWORD f = 0; f <= CRYPT_STRING_ANY; f++)
            if (f != CRYPT_STRING_HEX && f != CRYPT_STRING_HEXASCII) dec(hdr[i], 0, f);
    const char *hex[] = {"", "-", "0", "000", "0102", " aE\t\n\r\n", "01-02", "aa,,-BB---ff,-,", "1-2", "0102q",
                         "01 02", "00 01 02 03 04 05 06 07  08 09\r\n", "0a0b\r\n"};
    for (int i = 0; i < 13; i++) {
        dec(hex[i], 0, CRYPT_STRING_HEX);
        dec(hex[i], 0, CRYPT_STRING_HEXRAW);
        dec(hex[i], 0, CRYPT_STRING_HEX_ANY);
    }
    dec("01\0 02", 4, CRYPT_STRING_HEX);
    dec("AA==", 0, CRYPT_STRING_BASE64 | CRYPT_STRING_NOCR);
    return 0;
}
