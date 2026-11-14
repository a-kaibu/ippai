/* Unity 6's way of getting aligned regions (its allocators, 64 MB on sake: unity6.rs): reserve
 * twice the block less a page, round the base up to the block, use the whole block. Every
 * reservation that succeeds must hold its whole aligned block: committing the block's last page
 * works and that page takes a write. Run until it fails (sake's 2 GB fill up; Windows' 128 TB
 * don't within the count). A commit that runs past its reservation's end fails with
 * ERROR_INVALID_ADDRESS and leaves the pages reserved (sake gave お仕置きLesson 体験版 a cut
 * block at the top of a full address space and took commits past it, and the player wrote over
 * the TEBs, stacks and DLL images above). (sake: mem.rs reserve_aligned, k32.rs alloc_r) */
#include <windows.h>
#include <stdio.h>

#define BLOCK ((SIZE_T)64 << 20)
#define COUNT 48

int main(void)
{
    char *got[COUNT];
    int n = 0, whole = 1;
    for (; n < COUNT; n++) {
        char *r = VirtualAlloc(NULL, 2 * BLOCK - 0x1000, MEM_RESERVE, PAGE_READWRITE);
        if (!r)
            break;
        got[n] = r;
        char *b = (char *)(((ULONG_PTR)r + BLOCK - 1) & ~(BLOCK - 1));
        char *last = VirtualAlloc(b + BLOCK - 0x1000, 0x1000, MEM_COMMIT, PAGE_READWRITE);
        MEMORY_BASIC_INFORMATION m;
        VirtualQuery(b + BLOCK - 1, &m, sizeof m);
        if (last != b + BLOCK - 0x1000 || m.AllocationBase != r || m.State != MEM_COMMIT)
            whole = 0;
        else
            last[0xfff] = 1;
    }
    printf("reservations: at least 8 %d, every aligned block whole %d\n", n >= 8, whole);
    for (int i = 0; i < n; i++)
        VirtualFree(got[i], 0, MEM_RELEASE);

    /* a commit past the end of its reservation */
    char *a = VirtualAlloc(NULL, 0x10000, MEM_RESERVE, PAGE_NOACCESS);
    SetLastError(0xdead);
    char *c = VirtualAlloc(a + 0xf000, 0x2000, MEM_COMMIT, PAGE_READWRITE);
    DWORD err = GetLastError();
    MEMORY_BASIC_INFORMATION m;
    VirtualQuery(a + 0xf000, &m, sizeof m);
    printf("commit past the end: %d error %lu, last page still reserved %d\n", c != NULL, c ? 0 : (unsigned long)err, m.State == MEM_RESERVE);
    c = VirtualAlloc(a + 0xf000, 0x1000, MEM_COMMIT, PAGE_READWRITE);
    printf("commit of the last page: %d\n", c == a + 0xf000);
    c = VirtualAlloc(a + 0xfffc, 4, MEM_COMMIT, PAGE_READWRITE);
    printf("commit of its last 4 bytes: %d\n", c == a + 0xf000);
    VirtualFree(a, 0, MEM_RELEASE);
    return 0;
}
