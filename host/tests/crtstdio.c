#define __USE_MINGW_ANSI_STDIO 1
#include <stdio.h>
#include <windows.h>
int main(void) {
    typedef FILE *(__cdecl *Iob)(void);
    Iob iob = (Iob)GetProcAddress(GetModuleHandleA("msvcrt.dll"), "__p__iob");
    if (!iob || !iob()) return 1;
    FILE *streams = iob();
    printf("iob stable %d\n", streams == iob());
    printf("ansi integer %d hex %x string %s\n", 123, 0xabc, "ok");
    fprintf(stdout, "stdout %d\n", 42);
    return 0;
}
