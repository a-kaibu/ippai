/* Code that changes after it ran (and got translated): a function in a buffer returning an
 * immediate, called many times, then rewritten. "protect": made writable with VirtualProtect
 * around the write (what the JIT notices); "rwx": a PAGE_EXECUTE_READWRITE buffer written in
 * place. Prints what the calls returned. */
#include <stdio.h>
#include <string.h>
#include <windows.h>

typedef unsigned (*fn)(unsigned);

static unsigned run(fn f, int n)
{
    unsigned s = 0;
    for (int i = 0; i < n; i++) s += f(i);
    return s;
}

static void make(unsigned char *p, unsigned k)
{
    /* mov eax, [esp+4]; add eax, k; ret */
    static const unsigned char code[] = { 0x8b, 0x44, 0x24, 0x04, 0x05, 0, 0, 0, 0, 0xc3 };
    memcpy(p, code, sizeof(code));
    memcpy(p + 5, &k, 4);
    FlushInstructionCache(GetCurrentProcess(), p, sizeof(code));
}

int main(int argc, char **argv)
{
    DWORD old;
    unsigned char *p = VirtualAlloc(NULL, 4096, MEM_COMMIT, PAGE_READWRITE);
    make(p, 1);
    VirtualProtect(p, 4096, PAGE_EXECUTE_READ, &old);
    unsigned a = run((fn)p, 100000);
    VirtualProtect(p, 4096, PAGE_READWRITE, &old);
    make(p, 1000);
    VirtualProtect(p, 4096, PAGE_EXECUTE_READ, &old);
    unsigned b = run((fn)p, 100000);
    printf("protect %u %u %s\n", a, b, b - a == 999u * 100000 ? "ok" : "STALE");
    if (argc > 1)
    {
        unsigned char *q = VirtualAlloc(NULL, 4096, MEM_COMMIT, PAGE_EXECUTE_READWRITE);
        make(q, 1);
        a = run((fn)q, 100000);
        make(q, 1000);
        b = run((fn)q, 100000);
        printf("rwx     %u %u %s\n", a, b, b - a == 999u * 100000 ? "ok" : "STALE");
    }
    return 0;
}
