// libs: -lwininet
/* HttpQueryInfo's header indices (wininet.h HTTP_QUERY_*) on a request's own headers, which
 * need no connection: InternetConnect and HttpOpenRequest send nothing. */
#include <windows.h>
#include <wininet.h>
#include <stdio.h>
#include <string.h>
static int errors;
#define CHECK(x) do { if (!(x)) { printf("FAIL line %d\n", __LINE__); errors++; } } while (0)
static void query(HINTERNET r, DWORD info, const char *name) {
    char out[512]; DWORD n = sizeof out, i = 0;
    if (name) strcpy(out, name);
    SetLastError(0);
    BOOL ok = HttpQueryInfoA(r, info | HTTP_QUERY_FLAG_REQUEST_HEADERS, out, &n, &i);
    if (ok) printf("%#lx: [%s] %lu next %lu\n", info, out, n, i);
    else printf("%#lx: error %lu\n", info, GetLastError());
}
int main(void) {
    HINTERNET s = InternetOpenA("probe-agent/1.0", INTERNET_OPEN_TYPE_DIRECT, NULL, NULL, 0);
    CHECK(s != NULL);
    HINTERNET c = InternetConnectA(s, "example.com", INTERNET_DEFAULT_HTTPS_PORT, NULL, NULL, INTERNET_SERVICE_HTTP, 0, 0);
    CHECK(c != NULL);
    const char *types[] = { "text/plain", NULL };
    HINTERNET r = HttpOpenRequestA(c, "POST", "/path", NULL, NULL, types, INTERNET_FLAG_SECURE, 0);
    CHECK(r != NULL);
    CHECK(HttpAddRequestHeadersA(r, "X-Probe: one\r\nReferer: https://example.com/r\r\nContent-Type: text/plain\r\n", (DWORD)-1, HTTP_ADDREQ_FLAG_ADD));
    query(r, HTTP_QUERY_REQUEST_METHOD, NULL);
    query(r, HTTP_QUERY_ACCEPT, NULL);
    query(r, HTTP_QUERY_REFERER, NULL);
    query(r, HTTP_QUERY_CONTENT_TYPE, NULL);
    query(r, HTTP_QUERY_CUSTOM, "X-Probe");
    query(r, HTTP_QUERY_CUSTOM, "X-Absent");
    query(r, HTTP_QUERY_LOCATION, NULL);
    query(r, HTTP_QUERY_AUTHORIZATION, NULL);
    /* (no response yet) */
    char out[64]; DWORD n = sizeof out;
    SetLastError(0);
    CHECK(!HttpQueryInfoA(r, HTTP_QUERY_STATUS_CODE, out, &n, NULL));
    printf("status before send: error %lu\n", GetLastError());
    printf("indices: %d %d %d %d %d %d\n", HTTP_QUERY_RAW_HEADERS, HTTP_QUERY_CONNECTION, HTTP_QUERY_AUTHORIZATION,
        HTTP_QUERY_LOCATION, HTTP_QUERY_USER_AGENT, HTTP_QUERY_SET_COOKIE);
    InternetCloseHandle(r); InternetCloseHandle(c); InternetCloseHandle(s);
    printf("wininet query errors: %d\n", errors);
    return errors != 0;
}
