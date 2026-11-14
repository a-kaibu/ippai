/* RDTSC as a game clock: 十六夜の花嫁 (μ-GameOperationSystem) counts TSC ticks over one second of
 * timeGetTime, keeps the low 32 bits as its rate, and from then on its time is TSC / (rate /
 * 1000) ms. On Windows the counter never goes back and runs at a fixed GHz rate; a counter that
 * stood still (sake's was 0) left the game at the same moment forever and its title never came.
 * Build: i686-w64-mingw32-gcc -O2 rdtsc.c -o rdtsc.exe -lwinmm (libs: -lwinmm) */
#include <stdio.h>
#include <windows.h>
#include <x86intrin.h>

int main(void)
{
    /* the game's calibration: the TSC over 1000 ms of timeGetTime (here 250 ms, times 4) */
    DWORD t0 = timeGetTime(), t1;
    while ((t1 = timeGetTime()) == t0) {}
    unsigned long long a = __rdtsc();
    while ((int)(timeGetTime() - (t1 + 250)) <= 0) {}
    unsigned long long b = __rdtsc();
    unsigned long long hz = (b - a) * 4;
    printf("ticks a second: %s\n", hz >= 500000000ull && hz <= 6000000000ull ? "0.5 to 6 GHz" : "out of range");
    printf("rate fits an int: %s\n", (unsigned)hz != 0 ? "yes" : "no");

    /* never back, from read to read */
    unsigned long long last = __rdtsc();
    int back = 0;
    for (int i = 0; i < 100000; i++) {
        unsigned long long now = __rdtsc();
        if (now < last) back++;
        last = now;
    }
    printf("went back: %d times\n", back);

    /* its game clock moves with timeGetTime: TSC / (rate / 1000) across 100 ms of Sleep */
    unsigned per_ms = (unsigned)(hz / 1000);
    unsigned long long c0 = __rdtsc() / per_ms;
    DWORD s0 = timeGetTime();
    Sleep(100);
    unsigned long long c1 = __rdtsc() / per_ms;
    DWORD s1 = timeGetTime();
    long long drift = (long long)(c1 - c0) - (long long)(s1 - s0);
    printf("game clock over 100 ms: %s\n", c1 > c0 && drift > -30 && drift < 30 ? "moves with timeGetTime" : "stands or runs off");
    return 0;
}
