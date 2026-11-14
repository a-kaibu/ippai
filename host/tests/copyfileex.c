/* CopyFileExW / CopyFileExA (Mono's File.Copy; Easy Save 3 copies its save to a backup with it
 * from the second start of 妹ちゃんとくらそう on, which stopped at a black screen while sake had
 * no CopyFileExW): a copy, COPY_FILE_FAIL_IF_EXISTS, a missing source, *pbCancel already TRUE,
 * and the progress routine: its reasons and sizes, PROGRESS_QUIET, PROGRESS_CANCEL (the copy
 * goes) and PROGRESS_STOP (ERROR_REQUEST_ABORTED). (sake: k32.rs copy_ex)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static WCHAR src[MAX_PATH], dst[MAX_PATH];
static int calls, answer;
static DWORD reasons[8];

static DWORD CALLBACK progress(LARGE_INTEGER total, LARGE_INTEGER done, LARGE_INTEGER stream, LARGE_INTEGER stream_done, DWORD n, DWORD reason, HANDLE a, HANDLE b, LPVOID data) {
    (void)a; (void)b;
    if (calls < 8) reasons[calls] = reason;
    calls++;
    printf("  progress: reason %lu total %lld done %lld stream %lld/%lld number %lu data %d\n", reason, total.QuadPart, done.QuadPart, stream_done.QuadPart, stream.QuadPart, n, data == (LPVOID)&answer);
    return answer;
}

static int there(const WCHAR *p) {
    return GetFileAttributesW(p) != INVALID_FILE_ATTRIBUTES;
}

static void content(void) {
    char b[32] = "";
    DWORD n = 0;
    HANDLE h = CreateFileW(dst, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    ReadFile(h, b, sizeof b - 1, &n, NULL);
    CloseHandle(h);
    printf("  dst \"%s\"\n", b);
}

int main(void) {
    WCHAR tmp[MAX_PATH];
    GetTempPathW(MAX_PATH, tmp);
    wcscpy(src, tmp); wcscat(src, L"copyfileex-src.tmp");
    wcscpy(dst, tmp); wcscat(dst, L"copyfileex-dst.tmp");
    DeleteFileW(dst);
    HANDLE h = CreateFileW(src, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WriteFile(h, "es3 save data", 13, &(DWORD){0}, NULL);
    CloseHandle(h);

    BOOL r = CopyFileExW(src, dst, NULL, NULL, NULL, 0);
    printf("copy: %d\n", r);
    content();
    r = CopyFileExW(src, dst, NULL, NULL, NULL, COPY_FILE_FAIL_IF_EXISTS);
    printf("fail if exists: %d, error %lu\n", r, r ? 0 : GetLastError());
    r = CopyFileExW(src, dst, NULL, NULL, NULL, 0);
    printf("overwrite: %d\n", r);
    WCHAR none[MAX_PATH];
    wcscpy(none, tmp); wcscat(none, L"copyfileex-none.tmp");
    r = CopyFileExW(none, dst, NULL, NULL, NULL, 0);
    printf("missing source: %d, error %lu\n", r, r ? 0 : GetLastError());

    DeleteFileW(dst);
    BOOL cancel = TRUE;
    r = CopyFileExW(src, dst, NULL, NULL, &cancel, 0);
    DWORD e = r ? 0 : GetLastError();
    printf("cancelled before: %d, error %lu, dst there %d\n", r, e, there(dst));

    calls = 0; answer = PROGRESS_CONTINUE;
    r = CopyFileExW(src, dst, progress, &answer, NULL, 0);
    printf("progress continue: %d, calls %d, first reason %lu, last reason %lu\n", r, calls, reasons[0], reasons[calls - 1]);
    content();

    DeleteFileW(dst);
    calls = 0; answer = PROGRESS_QUIET;
    r = CopyFileExW(src, dst, progress, &answer, NULL, 0);
    printf("progress quiet: %d, calls %d, dst there %d\n", r, calls, there(dst));

    DeleteFileW(dst);
    calls = 0; answer = PROGRESS_CANCEL;
    r = CopyFileExW(src, dst, progress, &answer, NULL, 0);
    e = r ? 0 : GetLastError();
    printf("progress cancel: %d, error %lu, calls %d, dst there %d\n", r, e, calls, there(dst));

    DeleteFileW(dst);
    calls = 0; answer = PROGRESS_STOP;
    r = CopyFileExW(src, dst, progress, &answer, NULL, 0);
    printf("progress stop: %d, error %lu, calls %d\n", r, r ? 0 : GetLastError(), calls);

    DeleteFileW(dst);
    char srca[MAX_PATH], dsta[MAX_PATH];
    WideCharToMultiByte(CP_ACP, 0, src, -1, srca, MAX_PATH, NULL, NULL);
    WideCharToMultiByte(CP_ACP, 0, dst, -1, dsta, MAX_PATH, NULL, NULL);
    r = CopyFileExA(srca, dsta, NULL, NULL, NULL, COPY_FILE_FAIL_IF_EXISTS);
    printf("ansi: %d\n", r);
    content();
    DeleteFileW(dst);
    DeleteFileW(src);
    return 0;
}
