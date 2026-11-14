/* int 2d (the debug service interrupt) with no debugger attached. Windows raises
 * STATUS_BREAKPOINT with the context just after the instruction and EAX (the service number) as
 * the first parameter; a debugger swallows it and resumes a byte further. Enigma Protector
 * (anemoi's SiglusEngine.exe) tells the two apart, so a skipped int 2d reads as "A debugger is
 * detected". Code from Wine's ntdll exception.c test_debug_service: it returns 1 when resumed
 * just after int 2d, 0 when a byte further. */
#include "../probe.h"

#ifdef _WIN64
static const BYTE code[] = {
    0x53, 0x57, 0x48, 0x89, 0xc8,
    0x48, 0xb9, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
    0x48, 0xba, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
    0x48, 0xbb, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
    0x48, 0xbf, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
    0xcd, 0x2d,
    0xeb, 0x0f, 0x1f, 0x00,
    0x48, 0x31, 0xc0, 0xeb, 0x0e,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x48, 0x31, 0xc0, 0x48, 0xff, 0xc0,
    0x5f, 0x5b, 0xc3,
};
#define AFTER 0x2f
#else
static const BYTE code[] = {
    0x53, 0x57, 0x8b, 0x44, 0x24, 0x0c,
    0xb9, 0x11, 0x11, 0x11, 0x11, 0xba, 0x22, 0x22, 0x22, 0x22,
    0xbb, 0x33, 0x33, 0x33, 0x33, 0xbf, 0x44, 0x44, 0x44, 0x44,
    0xcd, 0x2d,
    0xeb, 0x0f, 0x1f, 0x00,
    0x31, 0xc0, 0xeb, 0x0c,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x31, 0xc0, 0x40,
    0x5f, 0x5b, 0xc3,
};
#define AFTER 0x1c
#endif

static BYTE *mem;
static int count;
static DWORD code_seen, nparams;
static int at_after, addr_after, info_is_eax;

static LONG CALLBACK handler(EXCEPTION_POINTERS *e) {
    EXCEPTION_RECORD *r = e->ExceptionRecord;
    if (r->ExceptionCode != EXCEPTION_BREAKPOINT) return EXCEPTION_CONTINUE_SEARCH;
    count++;
    code_seen = r->ExceptionCode;
    nparams = r->NumberParameters;
#ifdef _WIN64
    at_after = e->ContextRecord->Rip == (DWORD_PTR)mem + AFTER;
    info_is_eax = r->ExceptionInformation[0] == e->ContextRecord->Rax;
#else
    at_after = e->ContextRecord->Eip == (DWORD_PTR)mem + AFTER;
    info_is_eax = r->ExceptionInformation[0] == e->ContextRecord->Eax;
#endif
    addr_after = r->ExceptionAddress == mem + AFTER;
    return EXCEPTION_CONTINUE_EXECUTION;
}

/* (an SEH frame's view: the handler of a __try-less program is the vectored one above; this
 * one is reached when the vectored handler declines) */
static int seh_count;
static LONG WINAPI unhandled(EXCEPTION_POINTERS *e) {
    if (e->ExceptionRecord->ExceptionCode != EXCEPTION_BREAKPOINT) return EXCEPTION_CONTINUE_SEARCH;
    seh_count++;
    return EXCEPTION_CONTINUE_EXECUTION;
}

int main(void) {
    mem = VirtualAlloc(NULL, 4096, MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    memcpy(mem, code, sizeof code);
    DWORD(CDECL * f)(DWORD_PTR) = (void *)mem;
    void *v = AddVectoredExceptionHandler(1, handler);
    /* BREAKPOINT_BREAK 0, BREAKPOINT_PRINT 1, BREAKPOINT_PROMPT 2, LOAD/UNLOAD SYMBOLS 3/4,
     * an invalid one 6 */
    static const DWORD services[] = { 0, 1, 2, 3, 4, 6 };
    for (int i = 0; i < 6; i++) {
        count = 0;
        code_seen = nparams = 0;
        at_after = addr_after = info_is_eax = 0;
        DWORD ret = f(services[i]);
        printf("service %lu: exceptions %d ret %lu code %08lx context after it %d address after it %d params %lu info0 is eax %d\n",
               services[i], count, ret, code_seen, at_after, addr_after, nparams, info_is_eax);
    }
    RemoveVectoredExceptionHandler(v);
    /* the last chance filter sees it too */
    SetUnhandledExceptionFilter(unhandled);
    DWORD ret = f(0);
    printf("unhandled filter: %d ret %lu\n", seh_count, ret);
    return 0;
}
