// libs: -lwininet
#include <windows.h>
#include <wininet.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static int errors;
#define CHECK(x) do { if (!(x)) { printf("FAIL line %d\n", __LINE__); errors++; } } while (0)
int main(void) {
    char out[256]; DWORD n = sizeof(out);
    CHECK(InternetCanonicalizeUrlA("https://example.com/a/../b c?keep=two words", out, &n, 0));
    CHECK(strcmp(out, "https://example.com/b%20c?keep=two words") == 0);
    CHECK(n == strlen(out));
    n = sizeof(out);
    CHECK(InternetCanonicalizeUrlA("https://example.com/a%20b", out, &n, ICU_DECODE | ICU_NO_ENCODE));
    CHECK(strcmp(out, "https://example.com/a b") == 0);
    n = sizeof(out);
    CHECK(InternetCanonicalizeUrlA("https://example.com/a%20b", out, &n, ICU_ENCODE_PERCENT));
    CHECK(strcmp(out, "https://example.com/a%2520b") == 0);
    n = 1; out[0] = 'X'; SetLastError(0);
    CHECK(!InternetCanonicalizeUrlA("https://example.com/", out, &n, 0));
    CHECK(GetLastError() == ERROR_INSUFFICIENT_BUFFER && n == 21 && out[0] == 'X');
    const char *url = "https://user:pass@example.com:8443/path?query#fragment";
    URL_COMPONENTSA a = {0}; a.dwStructSize = sizeof(a);
    a.dwSchemeLength = a.dwHostNameLength = a.dwUserNameLength = a.dwPasswordLength = a.dwUrlPathLength = a.dwExtraInfoLength = 1;
    CHECK(InternetCrackUrlA(url, 0, 0, &a));
    CHECK(a.nScheme == INTERNET_SCHEME_HTTPS && a.nPort == 8443);
    CHECK(a.lpszScheme == url && a.dwSchemeLength == 5);
    CHECK(a.dwHostNameLength == 11 && !memcmp(a.lpszHostName, "example.com", 11));
    CHECK(a.dwUserNameLength == 4 && !memcmp(a.lpszUserName, "user", 4));
    CHECK(a.dwPasswordLength == 4 && !memcmp(a.lpszPassword, "pass", 4));
    CHECK(a.dwUrlPathLength == 5 && !memcmp(a.lpszUrlPath, "/path", 5));
    CHECK(a.dwExtraInfoLength == 15 && !memcmp(a.lpszExtraInfo, "?query#fragment", 15));
    WCHAR host[32], path[32]; URL_COMPONENTSW w = {0}; w.dwStructSize = sizeof(w);
    w.lpszHostName = host; w.dwHostNameLength = 32; w.lpszUrlPath = path; w.dwUrlPathLength = 32;
    CHECK(InternetCrackUrlW(L"https://example.com/a%20b", 0, ICU_DECODE, &w));
    CHECK(w.nPort == 443 && !wcscmp(host, L"example.com") && !wcscmp(path, L"/a b"));
    printf("wininet url errors: %d\n", errors);
    return errors != 0;
}
