/* arch: 32
 * The upper half of a 32-bit process (review 2026-11-14 A-1): addresses from 2 GB up are not the
 * program's (sake keeps its own heap and statics there), so writes there are access violations
 * with the address, and APIs given a buffer there fail (ReadFile: ERROR_NOACCESS) instead of
 * writing it. (Reads there fault on Windows too; sake lets them through, see kura cpu.rs.) */
#include "../probe.h"

void f_wr(void *p);
__asm__(".text\n.globl _f_wr\n_f_wr:\n mov 4(%esp), %ecx\n movl $9, (%ecx)\n ret\n");

static DWORD code, kind, at;
static int hits;

static LONG CALLBACK handler(EXCEPTION_POINTERS *e) {
    EXCEPTION_RECORD *r = e->ExceptionRecord;
    CONTEXT *c = e->ContextRecord;
    code = r->ExceptionCode;
    kind = r->NumberParameters >= 2 ? r->ExceptionInformation[0] : 99;
    at = r->NumberParameters >= 2 ? (DWORD)r->ExceptionInformation[1] : 0;
    hits++;
    /* back to the caller of the function under test */
    c->Eip = *(DWORD *)c->Esp;
    c->Esp += 4;
    return EXCEPTION_CONTINUE_EXECUTION;
}

static void one(const char *what, void (*f)(void *), DWORD a) {
    code = kind = at = 0;
    hits = 0;
    f((void *)a);
    printf("%s %08lx: hits %d code %08lx kind %lu at %08lx\n", what, a, hits, code, kind, at);
}

int main(void) {
    AddVectoredExceptionHandler(1, handler);
    static const DWORD addrs[] = { 0x80000000, 0x80001000, 0x80400000, 0x90000000, 0xc0000000, 0xf0000000 };
    for (int i = 0; i < 6; i++) {
        one("write", f_wr, addrs[i]);
    }
    /* ReadFile into the upper half */
    char path[MAX_PATH];
    GetModuleFileNameA(NULL, path, sizeof path);
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    DWORD got = 77;
    SetLastError(0);
    BOOL ok = ReadFile(h, (void *)0x90000000, 16, &got, NULL);
    printf("ReadFile to 90000000: %d error %lu got %lu\n", ok, GetLastError(), got);
    SetLastError(0);
    ok = ReadFile(h, NULL, 16, &got, NULL);
    printf("ReadFile to NULL: %d error %lu\n", ok, GetLastError());
    char small[4];
    ok = ReadFile(h, small, 0, &got, NULL);
    printf("ReadFile of 0 bytes: %d got %lu\n", ok, got);
    CloseHandle(h);
    return 0;
}
