// A game's reads of its archive, timed (tests/chunkbench/run.mjs): data.bin (incompressible,
// as a game's packed archives are) read as a VN reads one: its head at the start, then scattered
// reads (a scene's images and voices: 64 KiB at random places), then one long sequential read
// (a movie). Prints CHUNKREAD lines the harness parses.
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>

static LARGE_INTEGER freq;
static double now_ms(void) {
  LARGE_INTEGER t;
  QueryPerformanceCounter(&t);
  return (double)t.QuadPart * 1000.0 / (double)freq.QuadPart;
}
static int cmp(const void *a, const void *b) {
  double x = *(const double *)a, y = *(const double *)b;
  return x < y ? -1 : x > y;
}
static unsigned seed = 12345;
static unsigned rnd(void) { return seed = seed * 1103515245u + 12345u; }

int main(void) {
  QueryPerformanceFrequency(&freq);
  static char buf[1 << 20];
  double t0 = now_ms();
  HANDLE f = CreateFileA("data.bin", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
  if (f == INVALID_HANDLE_VALUE) { printf("CHUNKREAD error open\n"); return 1; }
  LARGE_INTEGER size;
  GetFileSizeEx(f, &size);
  DWORD got;
  unsigned sum = 0;
  // the start: the archive's head (its index) and the title's files
  for (int i = 0; i < 4; i++) {
    ReadFile(f, buf, 1 << 19, &got, NULL);
    sum += (unsigned char)buf[got / 2];
  }
  double t1 = now_ms();
  printf("CHUNKREAD start_ms=%.1f\n", t1 - t0);
  // scattered reads: 200 of 64 KiB, or as n.txt says ("<reads> <bytes each>", at most 1000
  // of 1 MiB: run.mjs's NET game)
  int N = 200, len = 65536;
  FILE *nf = fopen("n.txt", "r");
  if (nf) {
    if (fscanf(nf, "%d %d", &N, &len) != 2 || N < 1 || N > 1000 || len < 1 || len > (int)sizeof buf) N = 200, len = 65536;
    fclose(nf);
  }
  static double lat[1000];
  for (int i = 0; i < N; i++) {
    LONGLONG at = ((LONGLONG)rnd() << 8 ^ rnd()) % (size.QuadPart - len);
    LARGE_INTEGER p; p.QuadPart = at;
    double a = now_ms();
    SetFilePointerEx(f, p, NULL, FILE_BEGIN);
    ReadFile(f, buf, len, &got, NULL);
    lat[i] = now_ms() - a;
    sum += (unsigned char)buf[100];
  }
  double t2 = now_ms();
  qsort(lat, N, sizeof lat[0], cmp);
  printf("CHUNKREAD random_total_ms=%.1f p50=%.2f p95=%.2f max=%.1f\n", t2 - t1, lat[N / 2], lat[N * 95 / 100], lat[N - 1]);
  // a long read (a movie): 32 MiB from the middle
  LARGE_INTEGER p; p.QuadPart = size.QuadPart / 2;
  SetFilePointerEx(f, p, NULL, FILE_BEGIN);
  for (int i = 0; i < 32; i++) {
    ReadFile(f, buf, 1 << 20, &got, NULL);
    sum += (unsigned char)buf[7];
  }
  double t3 = now_ms();
  printf("CHUNKREAD seq_ms=%.1f\n", t3 - t2);
  printf("CHUNKREAD done total_ms=%.1f sum=%u\n", t3 - t0, sum);
  CloseHandle(f);
  return 0;
}
