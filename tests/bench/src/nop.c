/* Starts and ends: what starting a program costs (run.mjs times the page from load to here).
 * Build: i686-w64-mingw32-gcc -O2 nop.c -o nop.exe */
#include <stdio.h>

int main(void)
{
    printf("BENCH-END\n");
    fflush(stdout);
    return 0;
}
