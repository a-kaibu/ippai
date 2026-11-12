#include <windows.h>

#ifdef BUILD_FIXTURE
static volatile LONG active, overlap, attached, started, early, load_failed;
static HANDLE created;

static DWORD WINAPI worker(void *arg) {
    (void)arg;
    InterlockedExchange(&started, 1);
    return 0;
}

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance;
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH && reason != DLL_THREAD_ATTACH) return TRUE;
    if (InterlockedIncrement(&active) != 1) InterlockedIncrement(&overlap);
    /* Recursive acquisition on this thread must work while other callbacks wait. */
    HMODULE kernel = LoadLibraryA("kernel32.dll");
    if (!kernel) InterlockedIncrement(&load_failed);
    else FreeLibrary(kernel);
    if (reason == DLL_PROCESS_ATTACH) {
        created = CreateThread(NULL, 0, worker, NULL, 0, NULL);
        Sleep(50);
        early = started;
    } else {
        InterlockedIncrement(&attached);
        /* Widen the race without waiting for another thread inside DllMain. */
        Sleep(10);
    }
    InterlockedDecrement(&active);
    return TRUE;
}

__declspec(dllexport) DWORD WINAPI FinishCreated(void) {
    if (!created || WaitForSingleObject(created, 10000) != WAIT_OBJECT_0) return 0;
    CloseHandle(created);
    return !early && started;
}
__declspec(dllexport) DWORD WINAPI Overlaps(void) { return overlap; }
__declspec(dllexport) DWORD WINAPI Attaches(void) { return attached; }
__declspec(dllexport) DWORD WINAPI LoadFailures(void) { return load_failed; }
#else
#include <stdio.h>
#include <string.h>

static DWORD WINAPI worker(void *arg) { (void)arg; return 0; }
typedef DWORD (WINAPI *query)(void);

int main(void) {
    char path[MAX_PATH];
    GetModuleFileNameA(NULL, path, sizeof(path));
    char *name = strrchr(path, '\\');
    if (!name) return 1;
    strcpy(name + 1, "dllinit-fixture.dll");
    HMODULE dll = LoadLibraryA(path);
    if (!dll) { printf("load: FAIL\n"); return 1; }
    query finish = (query)GetProcAddress(dll, "FinishCreated");
    query overlaps = (query)GetProcAddress(dll, "Overlaps");
    query attaches = (query)GetProcAddress(dll, "Attaches");
    query failed = (query)GetProcAddress(dll, "LoadFailures");
    if (!finish || !overlaps || !attaches || !failed) return 2;
    printf("load: ok\n");
    DWORD deferred = finish();
    printf("created from DllMain: %s\n", deferred ? "deferred" : "FAIL");
    HANDLE threads[16];
    for (int i = 0; i < 16; i++) {
        threads[i] = CreateThread(NULL, 0, worker, NULL, CREATE_SUSPENDED, NULL);
        if (!threads[i]) return 3;
    }
    for (int i = 0; i < 16; i++) ResumeThread(threads[i]);
    if (WaitForMultipleObjects(16, threads, TRUE, 10000) != WAIT_OBJECT_0) return 4;
    for (int i = 0; i < 16; i++) CloseHandle(threads[i]);
    DWORD serial = overlaps() == 0, count = attaches(), nested = failed() == 0;
    printf("concurrent callbacks: %s\n", serial ? "serialized" : "FAIL");
    printf("thread attaches: %lu\n", count);
    printf("nested LoadLibrary: %s\n", nested ? "ok" : "FAIL");
    return deferred && serial && count == 17 && nested ? 0 : 5;
}
#endif
