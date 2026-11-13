// msvcp140's C-style threads support (x64, as std::mutex / condition_variable / thread call it):
// plain and recursive mutexes across threads, a condition variable handing 1000 items between
// two threads, a timed wait that times out, the thread id, the clocks, random_device,
// the error maps and _Throw_Cpp_error caught as a C++ exception, and vcruntime140's
// __intrinsic_setjmp / longjmp (no frame: a plain register restore), and the PPL's thread
// pool chores (_Schedule_chore / _Reschedule_chore / _Release_chore, as ppltasks call them). Prints only what any Windows
// gives. Built by build-msvc64.sh (clang, MSVC ABI, no SDK).
extern "C" {
typedef void *HANDLE;
typedef unsigned long DWORD;
__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD);
__declspec(dllimport) int __stdcall WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) HANDLE __stdcall CreateThread(void *, decltype(sizeof 0), DWORD(__stdcall *)(void *), void *, DWORD, DWORD *);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE, DWORD);
__declspec(dllimport) void __stdcall Sleep(DWORD);
__declspec(dllimport) int __stdcall CloseHandle(HANDLE);
long _InterlockedIncrement(volatile long *);
__declspec(dllimport) DWORD __stdcall GetCurrentThreadId(void);
__declspec(dllimport) int __stdcall QueryPerformanceFrequency(long long *);
__declspec(dllimport) void __stdcall GetSystemTimeAsFileTime(unsigned long long *);
__declspec(dllimport) void __stdcall ExitProcess(unsigned);
__declspec(dllimport) int __stdcall SleepConditionVariableSRW(void *, void *, DWORD, DWORD);
__declspec(dllimport) void __stdcall WakeAllConditionVariable(void *);
__declspec(dllimport) void __stdcall AcquireSRWLockExclusive(void *);
__declspec(dllimport) void __stdcall ReleaseSRWLockExclusive(void *);

struct xt { long long sec; long nsec; };
__declspec(dllimport) void _Mtx_init_in_situ(void *, int);
__declspec(dllimport) void _Mtx_destroy_in_situ(void *);
__declspec(dllimport) int _Mtx_lock(void *);
__declspec(dllimport) int _Mtx_trylock(void *);
__declspec(dllimport) int _Mtx_unlock(void *);
__declspec(dllimport) int _Mtx_current_owns(void *);
__declspec(dllimport) void _Cnd_init_in_situ(void *);
__declspec(dllimport) void _Cnd_destroy_in_situ(void *);
__declspec(dllimport) int _Cnd_wait(void *, void *);
__declspec(dllimport) int _Cnd_timedwait(void *, void *, const xt *);
__declspec(dllimport) int _Cnd_signal(void *);
__declspec(dllimport) int _Cnd_broadcast(void *);
__declspec(dllimport) unsigned _Thrd_id(void);
__declspec(dllimport) long long _Query_perf_frequency(void);
__declspec(dllimport) long long _Query_perf_counter(void);
__declspec(dllimport) long long _Xtime_get_ticks(void);
}
__declspec(dllimport) unsigned __cdecl _Random_device_imp() asm("?_Random_device@std@@YAIXZ");
#ifdef _WIN64
__declspec(dllimport) const char *__cdecl _Syserror_map_imp(int) asm("?_Syserror_map@std@@YAPEBDH@Z");
#else
__declspec(dllimport) const char *__cdecl _Syserror_map_imp(int) asm("?_Syserror_map@std@@YAPBDH@Z");
#endif
__declspec(dllimport) int __cdecl _Winerror_map_imp(int) asm("?_Winerror_map@std@@YAHH@Z");
__declspec(dllimport) void __cdecl _Throw_Cpp_error_imp(int) asm("?_Throw_Cpp_error@std@@YAXH@Z");

static void out(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    WriteFile(GetStdHandle((DWORD)-11), s, n, &n, 0);
}
// (where the program is: a watchdog prints it when a step hangs, as one did on Windows for
// 300 s after "kernel wait woken by _Cnd_broadcast", issue #96; nothing when none does)
static const char *volatile stage = "start";
static void out(const char *s);
static DWORD __stdcall watchdog(void *) {
    Sleep(60000);
    out("watchdog: stuck at ");
    out(stage);
    out("\n");
    ExitProcess(3);
    return 0;
}
static void num(const char *label, long long v) {
    char b[64];
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

alignas(8) static char mtx[80], rmtx[80], pmtx[80], cnd[72];
static volatile int queue, taken, done_producing;
static int other_result;

static DWORD __stdcall try_other(void *) {
    other_result = _Mtx_trylock(mtx);
    if (other_result == 0) _Mtx_unlock(mtx);
    return 0;
}
static DWORD __stdcall consumer(void *) {
    for (;;) {
        _Mtx_lock(mtx);
        while (queue == 0 && !done_producing) _Cnd_wait(cnd, mtx);
        if (queue == 0 && done_producing) { _Mtx_unlock(mtx); return 0; }
        queue--;
        taken++;
        _Cnd_broadcast(cnd);
        _Mtx_unlock(mtx);
    }
}

// The STL's headers wait and wake on the Windows objects inside msvcp140's: the
// CONDITION_VARIABLE at +8 of a _Cnd_internal_imp_t, the SRW lock at +16 of a
// _Mtx_internal_imp_t (AdvHD's std::condition_variable::wait_for calls
// SleepConditionVariableSRW on them and the PPL's chore wakes it with _Cnd_broadcast).
alignas(8) static char xmtx[80], xcnd[72];
// (the SRW lock and the CONDITION_VARIABLE inside them: after _Type (x64: padded) and
// critical_section's vtable)
#define XSRW (xmtx + 2 * sizeof(void *))
#define XCV (xcnd + sizeof(void *))
static volatile int xflag;
static int x_kernel_woke;
// (the waiter holds the SRW lock itself, as the inlined wait does: _Mtx_lock / _Mtx_unlock around
// it would leave the _Mtx's owner and count to a thread that no longer has it, and on Windows the
// next _Mtx_lock never returned)
static DWORD __stdcall kernel_waiter(void *) {
    AcquireSRWLockExclusive(XSRW);
    int r = 1;
    while (!xflag && r) r = SleepConditionVariableSRW(XCV, XSRW, 10000, 0);
    x_kernel_woke = r != 0 && xflag;
    ReleaseSRWLockExclusive(XSRW);
    return 0;
}
static DWORD __stdcall kernel_waker(void *) {
    Sleep(50);
    _Mtx_lock(xmtx);
    xflag = 2;
    WakeAllConditionVariable(XCV);
    _Mtx_unlock(xmtx);
    return 0;
}
static void mixed_test() {
    _Mtx_init_in_situ(xmtx, 2);
    _Cnd_init_in_situ(xcnd);
    stage = "mixed: SleepConditionVariableSRW woken by _Cnd_broadcast";
    HANDLE t = CreateThread(0, 0, kernel_waiter, 0, 0, 0);
    Sleep(50);
    _Mtx_lock(xmtx);
    xflag = 1;
    _Cnd_broadcast(xcnd);
    _Mtx_unlock(xmtx);
    WaitForSingleObject(t, 0xffffffff);
    num("kernel wait woken by _Cnd_broadcast", x_kernel_woke);
    xflag = 0;
    stage = "mixed: _Cnd_timedwait woken by WakeAllConditionVariable";
    t = CreateThread(0, 0, kernel_waker, 0, 0, 0);
    unsigned long long ft = 0;
    GetSystemTimeAsFileTime(&ft);
    long long secs = (long long)((ft - 116444736000000000ULL) / 10000000) + 10;
    xt until = {secs, 0};
    _Mtx_lock(xmtx);
    int r = 0;
    while (xflag != 2 && r == 0) r = _Cnd_timedwait(xcnd, xmtx, &until);
    stage = "mixed: _Cnd_timedwait returned";
    num("_Cnd_timedwait woken by WakeAllConditionVariable", r == 0 && xflag == 2);
    _Mtx_unlock(xmtx);
    stage = "mixed: waiting for the waker thread";
    WaitForSingleObject(t, 0xffffffff);
    stage = "mixed: done";
}

extern "C" {
#ifdef _WIN64
__declspec(dllimport) int __intrinsic_setjmp(unsigned long long *, void *);
#define SETJMP(b) __intrinsic_setjmp(b, 0)
#else
__declspec(dllimport) int _setjmp3(unsigned long long *, int, ...);
#define SETJMP(b) _setjmp3(b, 0)
#endif
__declspec(dllimport) void longjmp(unsigned long long *, int);
}
namespace Concurrency { namespace details {
struct _Threadpool_chore {
    void *_M_work;
    void (*_M_callback)(void *);
    void *_M_data;
};
__declspec(dllimport) int __cdecl _Schedule_chore(_Threadpool_chore *);
__declspec(dllimport) int __cdecl _Reschedule_chore(const _Threadpool_chore *);
__declspec(dllimport) void __cdecl _Release_chore(_Threadpool_chore *);
} }
static volatile long chore_runs;
static void chore_cb(void *data) {
    _InterlockedIncrement(&chore_runs);
    *(volatile int *)data = 7;
}
static void chore_test() {
    using namespace Concurrency::details;
    int data = 0;
    _Threadpool_chore ch{nullptr, chore_cb, &data};
    num("schedule chore", _Schedule_chore(&ch));
    num("chore has work", ch._M_work != nullptr);
    for (int i = 0; i < 5000 && chore_runs < 1; i++) Sleep(1);
    num("reschedule chore", _Reschedule_chore(&ch));
    for (int i = 0; i < 5000 && chore_runs < 2; i++) Sleep(1);
    num("chore runs", chore_runs);
    num("chore data", data);
    _Release_chore(&ch);
    num("released work", ch._M_work == nullptr);
}
alignas(16) static unsigned long long jb[32];
static volatile int hops;
__declspec(noinline) static void jump_back(int v) {
    hops = hops + 1;
    longjmp(jb, v);
}
static void setjmp_test() {
    volatile int local = 5;
    int r = SETJMP(jb);
    if (r < 3) {
        local = local + 1;
        jump_back(r == 0 ? 0 : r + 1);
    }
    num("setjmp result", r);
    num("setjmp hops", hops);
    num("setjmp local", local);
}
int main() {
    CloseHandle(CreateThread(0, 0, watchdog, 0, 0, 0));
    // (as std::mutex and std::recursive_mutex make them: _Mtx_try, | _Mtx_recursive. A _Mtx_plain
    // one is locked whatever is asked, a try too: Windows' msvcp140 waits there for ever)
    _Mtx_init_in_situ(mtx, 2);
    _Mtx_init_in_situ(rmtx, 2 | 0x100);
    _Mtx_init_in_situ(pmtx, 1);
    _Cnd_init_in_situ(cnd);
    num("lock", _Mtx_lock(mtx));
    num("owns", _Mtx_current_owns(mtx));
    // the thread that holds it, again: busy (not recursive)
    num("self try", _Mtx_trylock(mtx));
    num("self lock", _Mtx_lock(mtx));
    // a plain one counts again (as a recursive one) whatever is asked
    num("plain", _Mtx_lock(pmtx) + _Mtx_lock(pmtx) + _Mtx_trylock(pmtx));
    _Mtx_unlock(pmtx);
    _Mtx_unlock(pmtx);
    num("plain owns", _Mtx_current_owns(pmtx));
    _Mtx_unlock(pmtx);
    num("plain released", _Mtx_current_owns(pmtx));
    HANDLE t = CreateThread(0, 0, try_other, 0, 0, 0);
    WaitForSingleObject(t, 0xffffffff);
    num("trylock held elsewhere", other_result);
    num("unlock", _Mtx_unlock(mtx));
    num("owns after", _Mtx_current_owns(mtx));
    t = CreateThread(0, 0, try_other, 0, 0, 0);
    WaitForSingleObject(t, 0xffffffff);
    num("trylock free", other_result);
    num("recursive", _Mtx_lock(rmtx) + _Mtx_lock(rmtx) + _Mtx_trylock(rmtx));
    _Mtx_unlock(rmtx);
    _Mtx_unlock(rmtx);
    num("recursive owns", _Mtx_current_owns(rmtx));
    _Mtx_unlock(rmtx);
    num("recursive released", _Mtx_current_owns(rmtx));

    t = CreateThread(0, 0, consumer, 0, 0, 0);
    for (int i = 0; i < 1000; i++) {
        _Mtx_lock(mtx);
        while (queue >= 4) _Cnd_wait(cnd, mtx);
        queue++;
        _Cnd_signal(cnd);
        _Mtx_unlock(mtx);
    }
    _Mtx_lock(mtx);
    done_producing = 1;
    _Cnd_broadcast(cnd);
    _Mtx_unlock(mtx);
    WaitForSingleObject(t, 0xffffffff);
    num("taken", taken);
    xt past = {1, 0};
    _Mtx_lock(mtx);
    num("timedwait past", _Cnd_timedwait(cnd, mtx, &past));
    num("owns after wait", _Mtx_current_owns(mtx));
    _Mtx_unlock(mtx);
    _Cnd_destroy_in_situ(cnd);
    _Mtx_destroy_in_situ(mtx);

    num("thread id", _Thrd_id() == GetCurrentThreadId());
    long long f = 0;
    QueryPerformanceFrequency(&f);
    num("frequency", _Query_perf_frequency() == f);
    long long a = _Query_perf_counter(), b = _Query_perf_counter();
    num("counter", b >= a);
    unsigned long long ft = 0;
    GetSystemTimeAsFileTime(&ft);
    long long d = _Xtime_get_ticks() - (long long)(ft - 116444736000000000ULL);
    num("ticks", d >= 0 && d < 10000000);
    num("random", _Random_device_imp() != _Random_device_imp() || _Random_device_imp() != _Random_device_imp());
    out(_Syserror_map_imp(22));
    out("\n");
    num("winerror 5", _Winerror_map_imp(5));
    int caught = 0;
    try {
        _Throw_Cpp_error_imp(5); // _RESOURCE_DEADLOCK_WOULD_OCCUR
    } catch (...) {
        caught = 1;
    }
    num("caught", caught);
    stage = "setjmp";
    setjmp_test();
    stage = "chore";
    chore_test();
    mixed_test();
    ExitProcess(0);
}
