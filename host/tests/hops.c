// Calls among many small functions: each its own JIT region, so nearly every call and return
// is a jump between regions (the hops of crates/koji/src/jit/long.rs Gen::chain, and kura's
// merge_hot64, which compiles a region again with the regions it jumps to most). The functions use different
// registers (few, many with callee-saved ones live, eight arguments with some on the stack,
// SSE), so a region that loads and stores only the registers it names must leave the others
// as the region before it left them. Prints checksums that must not depend on the JIT
// (tests/host-tests.sh: JIT, interpreter, x64, x64 merging at the first hot jump). The
// expected output is what the same source prints built with gcc for Linux x86-64 and run natively.
#include <stdio.h>
#include <stdint.h>
#include <string.h>

typedef uint64_t u64;
typedef uint32_t u32;

#define NI __attribute__((noinline))

// few registers
#define A(i) NI static u64 a##i(u64 x) { return (x ^ (u64)(i) * 0x9e3779b97f4a7c15ull) + (x >> ((i) % 13 + 1)); }
// many live values: callee-saved registers (rbx, rbp, rsi, rdi, r12-r15 on x64)
#define B(i) NI static u64 b##i(u64 x) { \
  u64 p = x * 3 + (i), q = x ^ 0x5555, r = x >> 3, s = x << 5, t = x * 7, u = ~x, v = x + 99, w = x ^ (i); \
  for (int k = 0; k < 3; k++) { p += q * r; q ^= s + t; r += u ^ v; s -= w; t ^= p >> 7; u += q << 1; v ^= r; w += s; } \
  return p ^ q ^ r ^ s ^ t ^ u ^ v ^ w; }
// eight arguments (x64: four in registers, four on the stack)
#define C(i) NI static u64 c##i(u64 a, u64 b, u64 c, u64 d, u64 e, u64 f, u64 g, u64 h) { \
  return a * (i + 1) + (b ^ c) - (d >> 2) + e * f + (g ^ (h << 3)); }
// SSE
#define D(i) NI static u64 d##i(u64 x) { double f = (double)(x & 0xfffff) * (1.0 + (i) / 64.0); \
  return x ^ (u64)(int64_t)(f * 0.75 + (i)); }

#define R8(M, a) M(a##0) M(a##1) M(a##2) M(a##3) M(a##4) M(a##5) M(a##6) M(a##7)
#define R64(M) R8(M, 1) R8(M, 2) R8(M, 3) R8(M, 4) R8(M, 5) R8(M, 6) R8(M, 7) R8(M, 8)
R64(A)
R64(B)
R64(C)
R64(D)

#define P(f, i) f##i,
#define T8(f, a) P(f, a##0) P(f, a##1) P(f, a##2) P(f, a##3) P(f, a##4) P(f, a##5) P(f, a##6) P(f, a##7)
#define T64(f) T8(f, 1) T8(f, 2) T8(f, 3) T8(f, 4) T8(f, 5) T8(f, 6) T8(f, 7) T8(f, 8)
static u64 (*const ta[64])(u64) = { T64(a) };
static u64 (*const tb[64])(u64) = { T64(b) };
static u64 (*const tc[64])(u64, u64, u64, u64, u64, u64, u64, u64) = { T64(c) };
static u64 (*const td[64])(u64) = { T64(d) };

// recursion through the tables (returns that go back into regions deep in the stack)
NI static u64 deep(u64 x, int n) {
  if (n == 0) return x;
  u64 y = ta[x & 63](x) + deep(tb[(x >> 6) & 63](x), n - 1);
  return y ^ td[(x >> 12) & 63](y);
}

static u64 sink;

int main(void) {
  u64 x = 1;
  for (u32 i = 0; i < 200000; i++) x = ta[(x ^ i) & 63](x) + 1;
  printf("a %08x%08x\n", (u32)(x >> 32), (u32)x);
  for (u32 i = 0; i < 100000; i++) x = tb[(x ^ i) & 63](x);
  printf("b %08x%08x\n", (u32)(x >> 32), (u32)x);
  for (u32 i = 0; i < 100000; i++) x = tc[(x ^ i) & 63](x, x >> 1, x >> 2, x + i, i, x * 3, x ^ 77, i * 5);
  printf("c %08x%08x\n", (u32)(x >> 32), (u32)x);
  for (u32 i = 0; i < 100000; i++) x = td[(x ^ i) & 63](x);
  printf("d %08x%08x\n", (u32)(x >> 32), (u32)x);
  // all four kinds in turn
  for (u32 i = 0; i < 200000; i++) {
    switch ((x ^ i) & 3) {
    case 0: x = ta[(x >> 2) & 63](x); break;
    case 1: x = tb[(x >> 2) & 63](x); break;
    case 2: x = tc[(x >> 2) & 63](x, i, x, i, x, i, x, i); break;
    default: x = td[(x >> 2) & 63](x); break;
    }
  }
  printf("mix %08x%08x\n", (u32)(x >> 32), (u32)x);
  u64 s = 0;
  for (u32 i = 0; i < 2000; i++) s += deep(x + i, 12);
  printf("deep %08x%08x\n", (u32)(s >> 32), (u32)s);
  sink = s;
  return 0;
}
