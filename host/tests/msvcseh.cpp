// _set_se_translator (MSVC ABI, /EHa: clang's -fasync-exceptions): the translator turns a
// structured exception (RaiseException, an access violation) into a C++ one that a catch by
// type takes, in the frame that raised it or in one further up; destructors on the way run;
// a translator that returns leaves the exception to catch (...); C++ throws do not call it;
// it is per thread. Prints only what any Windows gives. Built by build-msvc64.sh (x86-64,
// vcruntime140) and build-msvc32.sh (x86, msvcrt).
// eha
extern "C" {
typedef void *HANDLE;
typedef unsigned long DWORD;
struct _EXCEPTION_RECORD { DWORD ExceptionCode, ExceptionFlags; _EXCEPTION_RECORD *ExceptionRecord; void *ExceptionAddress; };
struct _EXCEPTION_POINTERS { _EXCEPTION_RECORD *ExceptionRecord; void *ContextRecord; };
typedef void (__cdecl *_se_translator_function)(unsigned int, _EXCEPTION_POINTERS *);
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) int __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) void __stdcall RaiseException(DWORD, DWORD, DWORD, const void *);
__declspec(dllimport) HANDLE __stdcall CreateThread(void *, DWORD, DWORD (__stdcall *)(void *), void *, DWORD, DWORD *);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE, DWORD);
__declspec(dllimport) int __stdcall GetExitCodeThread(HANDLE, DWORD *);
}
// (x86 msvcrt exports it as C++, vcruntime140 as C)
#ifdef _WIN64
extern "C" _se_translator_function __cdecl _set_se_translator(_se_translator_function);
#else
_se_translator_function __cdecl _set_se_translator(_se_translator_function);
#endif

// (type_info's vftable is no DLL's export to rely on: msvcrt.lib has it; nothing here calls
// through it)
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

struct SE { unsigned code; };
static int calls, dtors, pass, pointers_ok;
struct Guard { ~Guard() { dtors++; } };

static void __cdecl translate(unsigned code, _EXCEPTION_POINTERS *ep) {
    calls++;
    pointers_ok += ep && ep->ExceptionRecord && ep->ExceptionRecord->ExceptionCode == code && ep->ContextRecord;
    if (pass) return;
    throw SE{code};
}

__declspec(noinline) static void raise(DWORD code) { RaiseException(code, 0, 0, 0); }

// a frame with a try block that takes no SE: the translator runs here first
__declspec(noinline) static int inner(DWORD code) {
    Guard g;
    try {
        raise(code);
    } catch (int) {
        return -1;
    }
    return 0;
}

// no try block: not called here
__declspec(noinline) static void middle(DWORD code) {
    Guard g;
    raise(code);
}

// (a pointer the compiler cannot see is null; the store in a function of its own, called
// through a pointer: clang keeps no try state around a lone store or a call it knows cannot
// throw)
static volatile int *volatile nowhere;
static void poke_at(volatile int *p) { *p = 1; }
static void (*volatile poke)(volatile int *) = poke_at;
__declspec(noinline) static int av(volatile int *p) {
    try {
        poke(p);
    } catch (SE e) {
        return e.code == 0xc0000005;
    }
    return 0;
}

// every depth and catching frame: frames 1..depth have try blocks (taking int), the one at
// `at` takes SE
static int caught_at, caught_code;
__declspec(noinline) static void nest(int depth, int at, DWORD code) {
    Guard g;
    if (depth == 0) {
        raise(code);
    } else if (depth == at) {
        try { nest(depth - 1, at, code); } catch (SE e) { caught_at = depth; caught_code = e.code == code; }
    } else {
        try { nest(depth - 1, at, code); } catch (int) {}
    }
}

static DWORD __stdcall thread(void *) {
    int r = 0;
    try {
        raise(0xe0000005);
    } catch (SE) {
        r = 2;
    } catch (...) {
        r = 1;
    }
    return r;
}

int main() {
    // no translator: catch (...) takes the SE
    int r = 0;
    try { raise(0xe0000001); } catch (...) { r = 1; }
    num("no translator, catch all", r);
    num("first set returns", _set_se_translator(translate) == 0);

    calls = 0;
    r = 0;
    try { raise(0xe0000002); } catch (SE e) { r = e.code == 0xe0000002; }
    num("same frame catch SE", r);
    num("same frame calls", calls);
    num("pointers", pointers_ok == calls);

    calls = dtors = r = 0;
    try { inner(0xe0000003); } catch (SE e) { r = e.code == 0xe0000003; }
    num("outer frame catch SE", r);
    num("outer frame calls", calls);
    num("outer frame destructors", dtors);

    calls = dtors = r = 0;
    try { middle(0xe0000004); } catch (SE e) { r = e.code == 0xe0000004; }
    num("no try block between", r);
    num("no try block calls", calls);
    num("no try block destructors", dtors);

    calls = r = 0;
    pass = 1;
    try { raise(0xe0000006); } catch (SE) { r = 2; } catch (...) { r = 1; }
    pass = 0;
    num("translator returns, catch all", r);
    num("translator returns calls", calls);

    calls = r = 0;
    try { throw 5; } catch (int x) { r = x == 5; }
    num("C++ throw", r);
    num("C++ throw calls", calls);

    for (int depth = 1; depth <= 4; depth++) {
        for (int at = 1; at <= depth; at++) {
            calls = dtors = caught_at = caught_code = 0;
            nest(depth, at, 0xe0000100 + depth * 16 + at);
            char l[] = "nest depth 0 at 0:";
            l[11] += depth;
            l[16] += at;
            out(l);
            num(" caught at", caught_at);
            num("  code", caught_code);
            num("  calls", calls);
            num("  destructors", dtors);
        }
    }

    calls = 0;
    num("access violation", av(nowhere));
    num("access violation calls", calls);

    calls = 0;
    DWORD id;
    HANDLE h = CreateThread(0, 0, thread, 0, 0, &id);
    WaitForSingleObject(h, 0xffffffff);
    DWORD tr = 0;
    GetExitCodeThread(h, &tr);
    num("other thread, catch all", tr);
    num("other thread calls", calls);

    num("second set returns the first", _set_se_translator(0) == translate);
    calls = r = 0;
    try { raise(0xe0000007); } catch (SE) { r = 2; } catch (...) { r = 1; }
    num("removed, catch all", r);
    num("removed calls", calls);
    ExitProcess(0);
}
