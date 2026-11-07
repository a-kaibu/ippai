/* ReadProcessMemory / WriteProcessMemory on the program's own process: the counts (SIZE_T, 8
 * bytes on x64), partial copies into a reserved page, NULL, zero sizes, read-only and code pages,
 * and code patched while it runs (Delphi programs hook their own functions this way: sake's
 * translated code has to see the new bytes, 0556349). */
#include "../probe.h"

/* a function in the image's code section that returns a constant (patched below) */
__asm__(".text\n.p2align 4\npatch_target:\n movl $1, %eax\n ret\n .fill 16, 1, 0x90\n");
int target(void) __asm__("patch_target");

typedef int (*fn)(void);
static int __attribute__((noinline)) run(fn f, int n) {
    int s = 0;
    for (int i = 0; i < n; i++) s += f();
    return s;
}

static void counts(HANDLE p, const char *who) {
    static char src[64] = "abcdefghijklmnopqrstuvwxyz", dst[64];
    SIZE_T n = (SIZE_T)-1;
    SetLastError(0xdead);
    BOOL r = ReadProcessMemory(p, src, dst, 26, &n);
    printf("%s read 26: ret %d count %llu same %d\n", who, r != 0, (unsigned long long)n, memcmp(src, dst, 26) == 0);
    n = (SIZE_T)-1;
    r = ReadProcessMemory(p, src, dst, 0, &n);
    printf("%s read 0: ret %d err %lu count %llu\n", who, r != 0, r ? 0 : GetLastError(), (unsigned long long)n);
    r = ReadProcessMemory(p, src, dst, 4, NULL);
    printf("%s read without count: ret %d\n", who, r != 0);
    n = (SIZE_T)-1;
    SetLastError(0xdead);
    r = ReadProcessMemory(p, NULL, dst, 4, &n);
    printf("%s read NULL: ret %d err %lu count %llu\n", who, r != 0, r ? 0 : GetLastError(), (unsigned long long)n);
    n = (SIZE_T)-1;
    SetLastError(0xdead);
    r = WriteProcessMemory(p, NULL, src, 4, &n);
    printf("%s write NULL: ret %d err %lu count %llu\n", who, r != 0, r ? 0 : GetLastError(), (unsigned long long)n);
    n = (SIZE_T)-1;
    r = WriteProcessMemory(p, dst, "XYZ", 3, &n);
    printf("%s write 3: ret %d count %llu text %.5s\n", who, r != 0, (unsigned long long)n, dst);
}

int main(void) {
    HANDLE self = GetCurrentProcess();
    counts(self, "pseudo");
    HANDLE h = OpenProcess(PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    printf("OpenProcess self: %d\n", h != NULL);
    if (h) counts(h, "handle");

    /* a committed page followed by a reserved one */
    char *two = VirtualAlloc(NULL, 0x2000, MEM_RESERVE, PAGE_NOACCESS);
    VirtualAlloc(two, 0x1000, MEM_COMMIT, PAGE_READWRITE);
    memset(two, 'q', 0x1000);
    static char out[0x2000];
    SIZE_T n = (SIZE_T)-1;
    SetLastError(0xdead);
    BOOL r = ReadProcessMemory(self, two + 0xff0, out, 0x20, &n);
    printf("read across into reserved: ret %d err %lu count %llu\n", r != 0, r ? 0 : GetLastError(), (unsigned long long)n);
    n = (SIZE_T)-1;
    SetLastError(0xdead);
    r = ReadProcessMemory(self, two + 0x1000, out, 0x10, &n);
    printf("read reserved: ret %d err %lu count %llu\n", r != 0, r ? 0 : GetLastError(), (unsigned long long)n);
    n = (SIZE_T)-1;
    SetLastError(0xdead);
    r = WriteProcessMemory(self, two + 0xff8, "0123456789abcdef", 16, &n);
    printf("write across into reserved: ret %d err %lu count %llu first %c\n", r != 0, r ? 0 : GetLastError(), (unsigned long long)n, two[0xff8]);
    /* into another buffer that ends in the reserved page */
    n = (SIZE_T)-1;
    SetLastError(0xdead);
    r = ReadProcessMemory(self, out, two + 0xff8, 16, &n);
    printf("read into reserved: ret %d err %lu count %llu\n", r != 0, r ? 0 : GetLastError(), (unsigned long long)n);

    /* a read-only page and a no-access page */
    char *ro = VirtualAlloc(NULL, 0x1000, MEM_COMMIT, PAGE_READWRITE);
    strcpy(ro, "readonly");
    DWORD old;
    VirtualProtect(ro, 0x1000, PAGE_READONLY, &old);
    SetLastError(0xdead);
    r = WriteProcessMemory(self, ro, "WRITTEN", 7, &n);
    MEMORY_BASIC_INFORMATION mbi;
    VirtualQuery(ro, &mbi, sizeof mbi);
    printf("write read-only page: ret %d err %lu text %.8s protect after %lx\n", r != 0, r ? 0 : GetLastError(), ro, mbi.Protect);
    VirtualProtect(ro, 0x1000, PAGE_NOACCESS, &old);
    SetLastError(0xdead);
    r = ReadProcessMemory(self, ro, out, 4, &n);
    printf("read no-access page: ret %d err %lu\n", r != 0, r ? 0 : GetLastError());
    SetLastError(0xdead);
    r = WriteProcessMemory(self, ro, "x", 1, &n);
    VirtualQuery(ro, &mbi, sizeof mbi);
    printf("write no-access page: ret %d err %lu protect after %lx\n", r != 0, r ? 0 : GetLastError(), mbi.Protect);

    /* code: in allocated executable memory, run hot, patched, run again */
    unsigned char *code = VirtualAlloc(NULL, 0x1000, MEM_COMMIT, PAGE_EXECUTE_READWRITE);
    unsigned char body[] = { 0xb8, 7, 0, 0, 0, 0xc3 };
    memcpy(code, body, sizeof body);
    FlushInstructionCache(self, code, sizeof body);
    printf("heap code before: %d\n", run((fn)code, 100000));
    int v = 9;
    r = WriteProcessMemory(self, code + 1, &v, 4, &n);
    printf("heap code patched: ret %d %d\n", r != 0, run((fn)code, 100000));
    /* the same bytes written by the program itself, then FlushInstructionCache */
    code[1] = 11;
    FlushInstructionCache(self, code, 6);
    printf("heap code stored: %d\n", run((fn)code, 100000));
    /* read-execute code (the image's own section) */
    VirtualQuery((void *)target, &mbi, sizeof mbi);
    printf("image code protect %lx\n", mbi.Protect);
    printf("image code before: %d\n", run(target, 100000));
    v = 3;
    SetLastError(0xdead);
    r = WriteProcessMemory(self, (char *)target + 1, &v, 4, &n);
    unsigned long er = GetLastError();
    VirtualQuery((void *)target, &mbi, sizeof mbi);
    printf("image code patched: ret %d err %lu count %llu protect after %lx\n", r != 0, r ? 0 : er, (unsigned long long)n, mbi.Protect);
    printf("image code after: %d\n", run(target, 100000));
    /* a jump written over the start: mov eax,5 ; ret placed in the heap page, target jumps to it */
    unsigned char *hook = code + 0x100;
    unsigned char five[] = { 0xb8, 5, 0, 0, 0, 0xc3 };
    memcpy(hook, five, sizeof five);
#ifdef _WIN64
    unsigned char jmp[12] = { 0x48, 0xb8 };
    unsigned long long a = (unsigned long long)hook;
    memcpy(jmp + 2, &a, 8);
    jmp[10] = 0xff, jmp[11] = 0xe0;
#else
    int rel = (int)(hook - ((unsigned char *)target + 5));
    unsigned char jmp[5] = { 0xe9 };
    memcpy(jmp + 1, &rel, 4);
#endif
    r = WriteProcessMemory(self, (void *)target, jmp, sizeof jmp, &n);
    printf("image code hooked: ret %d %d\n", r != 0, run(target, 100000));
    return 0;
}
