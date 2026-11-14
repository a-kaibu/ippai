// MSVC C++ exceptions (x86) that leave a catch block: `throw;` and a new throw from inside a
// catch, caught further out, many times over and inside a callback the system calls
// (InitOnceExecuteOnce). Each object is destroyed exactly once, a rethrown object is the same
// one, and after a callback whose catch rethrew returns, the program goes on where it was.
// Prints only what any Windows gives. Built by build-msvc32.sh (x86, msvcrt).
extern "C" {
typedef void *HANDLE;
typedef unsigned long DWORD;
typedef int BOOL;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) int __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) BOOL __stdcall InitOnceExecuteOnce(void **, BOOL (__stdcall *)(void **, void *, void **), void *, void **);
}

extern "C" const void *const type_info_vftable[2] asm("??_7type_info@@6B@") = {0, 0};

static void out(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    WriteFile(GetStdHandle((DWORD)-11), s, n, &n, 0);
}
static void num(const char *label, int v) {
    char b[96];
    int i = 0;
    while (*label) b[i++] = *label++;
    b[i++] = ' ';
    if (v < 0) { b[i++] = '-'; v = -v; }
    char t[24];
    int k = 0;
    do { t[k++] = '0' + v % 10; v /= 10; } while (v);
    while (k) b[i++] = t[--k];
    b[i++] = '\n';
    b[i] = 0;
    out(b);
}

static int ctors, dtors, cleanups;
struct Obj {
    int id;
    Obj(int i) : id(i) { ctors++; }
    Obj(const Obj &o) : id(o.id) { ctors++; }
    ~Obj() { dtors++; }
};
struct Guard { ~Guard() { cleanups++; } };

__declspec(noinline) void thrower(int id) {
    Guard g;
    throw Obj(id);
}
// catch (...) { cleanup; throw; } two levels deep
__declspec(noinline) void level1(int id) {
    try { thrower(id); } catch (...) { cleanups++; throw; }
}
__declspec(noinline) void level2(int id) {
    Guard g;
    try { level1(id); } catch (Obj &) { cleanups++; throw; }
}
// a new object thrown from a catch block: the one it handled goes
__declspec(noinline) void replace(int id) {
    try { thrower(id); } catch (Obj &o) { throw Obj(o.id + 1000); }
}

static int chain_ok;
// each round: rethrows, replacements, rethrows caught again inside the catch (6 checks)
static void do_round(int k) {
    try { level2(k); } catch (Obj &o) { chain_ok += o.id == k; }
    try { replace(k); } catch (const Obj &o) { chain_ok += o.id == k + 1000; }
    try {
        thrower(k);
    } catch (Obj &o) {
        const Obj *inner = 0;
        try { throw; } catch (Obj &p) { inner = &p; }
        // (the same object, still alive)
        chain_ok += inner == &o && o.id == k;
    }
    try { level1(k); } catch (Obj o) { chain_ok += o.id == k; }
    // a throw from a catch block that a try around it in the same function catches
    try {
        try { thrower(k); } catch (Obj &) { throw Obj(k + 2000); }
    } catch (Obj &o) { chain_ok += o.id == k + 2000; }
    // catch blocks in catch blocks, the innermost rethrowing its own
    try {
        thrower(k);
    } catch (Obj &o) {
        try {
            thrower(k + 1);
        } catch (Obj &p) {
            try { throw; } catch (Obj &q) { chain_ok += &q == &p && o.id == k && p.id == k + 1; }
        }
    }
}

static int callbacks;
static BOOL __stdcall once(void **, void *param, void **) {
    int k = (int)(long)param;
    do_round(k);
    callbacks++;
    return 1;
}

int main() {
    const int N = 3000;
    chain_ok = 0;
    for (int k = 0; k < N; k++) do_round(k);
    num("rounds", N);
    num("caught right", chain_ok == 6 * N);
    num("objects balanced", ctors == dtors);
    num("cleanups per round", cleanups / N);

    // the same inside a callback the system calls (the host calls the guest)
    ctors = dtors = cleanups = chain_ok = 0;
    const int M = 500;
    int back = 0;
    for (int k = 0; k < M; k++) {
        void *init = 0;
        int before = callbacks;
        if (InitOnceExecuteOnce(&init, once, (void *)(long)k, 0) && callbacks == before + 1) back++;
    }
    num("callbacks returned", back == M);
    num("callbacks caught right", chain_ok == 6 * M);
    num("callbacks objects balanced", ctors == dtors);

    // and an exception thrown out of a catch block that nothing in the callback catches
    int outer = 0;
    try {
        try { thrower(7); } catch (Obj &) { throw; }
    } catch (Obj &o) { outer = o.id == 7; }
    num("after callbacks", outer);
    num("all balanced", ctors == dtors);
    ExitProcess(0);
}
