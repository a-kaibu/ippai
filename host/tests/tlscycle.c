// Implicit TLS (a DLL's __declspec(thread) / __thread variables, its .tls directory) across
// LoadLibrary and FreeLibrary: a thread that already ran when the DLL loaded has its block
// too (Windows since Vista), each load starts from the DLL's initial values, and loading and
// unloading it over and over (KiriKiri's Plugins.link/unlink, a codec DLL per movie) keeps
// reusing one TLS index instead of running past the TEB's array (1024 x86, 512 x64).
// The fixture's TLS is native (clang's mingw target; mingw gcc would use emutls):
// tests/host-tests.sh builds it with clang and links it with gcc.
#include <windows.h>
#include <stdio.h>

#ifdef BUILD_FIXTURE
static __thread int counter = 5;
static __thread char buf[256] = "tls";
extern ULONG _tls_index;

__declspec(dllexport) int Next(void) { return counter++; }
__declspec(dllexport) int Buf(void) { return buf[0] + buf[255]; }
__declspec(dllexport) ULONG Index(void) { return _tls_index; }
#else

typedef int (*IntFn)(void);
typedef ULONG (*IndexFn)(void);

static HANDLE go, done;
static volatile IntFn worker_fn;
static volatile int worker_got;
static volatile LONG quit;

static DWORD WINAPI worker(void *arg) {
    (void)arg;
    for (;;) {
        WaitForSingleObject(go, INFINITE);
        if (quit) return 0;
        worker_got = worker_fn();
        SetEvent(done);
    }
}

// the worker (started before the DLL loaded) calls `f` once
static int on_worker(IntFn f) {
    worker_fn = f;
    SetEvent(go);
    WaitForSingleObject(done, INFINITE);
    return worker_got;
}

int main(void) {
    go = CreateEventA(NULL, FALSE, FALSE, NULL);
    done = CreateEventA(NULL, FALSE, FALSE, NULL);
    HANDLE th = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    // (the worker runs before the DLL is there)
    Sleep(20);

    HMODULE m = LoadLibraryA("tlscycle-fixture.dll");
    if (!m) {
        printf("load failed %lu\n", GetLastError());
        return 1;
    }
    IntFn next = (IntFn)GetProcAddress(m, "Next");
    IntFn buf = (IntFn)GetProcAddress(m, "Buf");
    IndexFn index = (IndexFn)GetProcAddress(m, "Index");
    int a = next(), b = next();
    int w1 = on_worker(next), w2 = on_worker(next);
    int wb = on_worker(buf);
    printf("loader thread %d %d, earlier thread %d %d, its buffer %d\n", a, b, w1, w2, wb);
    ULONG first = index();
    FreeLibrary(m);

    // load and unload many times (more than the TEB's array has slots)
    int cycles = 3000, fresh = 0, same_index = 0, worker_fresh = 0;
    for (int k = 0; k < cycles; k++) {
        m = LoadLibraryA("tlscycle-fixture.dll");
        if (!m) {
            printf("load %d failed %lu\n", k, GetLastError());
            return 1;
        }
        next = (IntFn)GetProcAddress(m, "Next");
        index = (IndexFn)GetProcAddress(m, "Index");
        fresh += next() == 5 && next() == 6;
        same_index += index() == first;
        if (k % 500 == 0) worker_fresh += on_worker(next) == 5;
        FreeLibrary(m);
    }
    printf("cycles %d: fresh values %d, same index %d, earlier thread fresh %d/%d\n", cycles, fresh, same_index,
           worker_fresh, (cycles + 499) / 500);

    // a thread started while the DLL is loaded gets its block as before
    m = LoadLibraryA("tlscycle-fixture.dll");
    next = (IntFn)GetProcAddress(m, "Next");
    quit = 1;
    SetEvent(go);
    WaitForSingleObject(th, INFINITE);
    quit = 0;
    th = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    int late = on_worker(next);
    quit = 1;
    SetEvent(go);
    WaitForSingleObject(th, INFINITE);
    printf("later thread %d\n", late);
    FreeLibrary(m);
    return 0;
}
#endif
