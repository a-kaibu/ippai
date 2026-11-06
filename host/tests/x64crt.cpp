// The x64 C runtime as a VC++ 2015+ program calls it (ucrtbase's __stdio_common_*, vcruntime140's
// C++ exceptions and __C_specific_handler, msvcp140's exceptions): each check against what
// Windows gives. Prints "x64crt ok" when all pass. Build (clang, no MSVC headers or libraries):
//   host/tests/build-x64crt.sh OUT_DIR
extern "C" {
typedef unsigned long long size_t;
typedef char *va_list;
int __stdio_common_vsprintf(unsigned long long, char *, size_t, const char *, void *, va_list);
int __stdio_common_vsprintf_s(unsigned long long, char *, size_t, const char *, void *, va_list);
int __stdio_common_vsnprintf_s(unsigned long long, char *, size_t, size_t, const char *, void *, va_list);
int __stdio_common_vswprintf(unsigned long long, wchar_t *, size_t, const wchar_t *, void *, va_list);
int __stdio_common_vsscanf(unsigned long long, const char *, size_t, const char *, void *, va_list);
int __stdio_common_vfprintf(unsigned long long, void *, const char *, void *, va_list);
void *__acrt_iob_func(unsigned);
double strtod(const char *, char **);
float strtof(const char *, char **);
long strtol(const char *, char **, int);
unsigned long strtoul(const char *, char **, int);
long long _strtoi64(const char *, char **, int);
unsigned long long _strtoui64(const char *, char **, int);
long long _atoi64(const char *);
int atoi(const char *);
int *_errno(void);
int strcpy_s(char *, size_t, const char *);
int memcpy_s(void *, size_t, const void *, size_t);
int memmove_s(void *, size_t, const void *, size_t);
int strcat_s(char *, size_t, const char *);
int strncpy_s(char *, size_t, const char *, size_t);
int _i64toa_s(long long, char *, size_t, int);
char *_ui64toa(unsigned long long, char *, int);
int memcmp(const void *, const void *, size_t);
int strcmp(const char *, const char *);
size_t strlen(const char *);
void *memset(void *, int, size_t);
void *memcpy(void *, const void *, size_t);
void qsort(void *, size_t, size_t, int (*)(const void *, const void *));
long long _mktime64(void *);
void *_gmtime64(const long long *);
size_t strftime(char *, size_t, const char *, const void *);
double ldexp(double, int);
double frexp(double, int *);
double modf(double, double *);
float sinf(float);
double pow(double, double);
void *malloc(size_t);
void free(void *);
void _exit(int);
int __std_type_info_compare(const void *, const void *);
}
extern "C" __declspec(dllimport) void __stdcall ExitProcess(unsigned);
extern "C" __declspec(dllimport) void __stdcall RaiseException(unsigned, unsigned, unsigned, const void *);

static int printf_(const char *f, ...);
static int checks, fails;
static void check(bool ok, const char *what) {
  checks++;
  if (!ok) {
    fails++;
    printf_("FAIL %s\n", what);
  }
}
// printf through the UCRT core, as <stdio.h>'s inline printf does (options: legacy wide
// specifiers off, standard snprintf behaviour as snprintf's)
static int vsnp(char *b, size_t n, unsigned long long opts, const char *f, va_list va) { return __stdio_common_vsprintf(opts, b, n, f, 0, va); }
int sprintf_(char *b, size_t n, const char *f, ...) {
  va_list va;
  __builtin_va_start(va, f);
  int r = vsnp(b, n, 2, f, va);
  __builtin_va_end(va);
  return r;
}
static int printf_(const char *f, ...) {
  va_list va;
  __builtin_va_start(va, f);
  int r = __stdio_common_vfprintf(0, __acrt_iob_func(1), f, 0, va);
  __builtin_va_end(va);
  return r;
}
static int pf(unsigned long long opts, char *b, size_t n, const char *f, ...) {
  va_list va;
  __builtin_va_start(va, f);
  int r = vsnp(b, n, opts, f, va);
  __builtin_va_end(va);
  return r;
}
static int pf_s(char *b, size_t n, const char *f, ...) {
  va_list va;
  __builtin_va_start(va, f);
  int r = __stdio_common_vsprintf_s(0, b, n, f, 0, va);
  __builtin_va_end(va);
  return r;
}
static int pfn_s(char *b, size_t n, size_t count, const char *f, ...) {
  va_list va;
  __builtin_va_start(va, f);
  int r = __stdio_common_vsnprintf_s(0, b, n, count, f, 0, va);
  __builtin_va_end(va);
  return r;
}
static int sc(const char *in, const char *f, ...) {
  va_list va;
  __builtin_va_start(va, f);
  int r = __stdio_common_vsscanf(0, in, (size_t)-1, f, 0, va);
  __builtin_va_end(va);
  return r;
}
static bool is(const char *a, const char *b) { return strcmp(a, b) == 0; }

static void printf_checks() {
  char b[128];
  sprintf_(b, sizeof b, "%d %5.2f %e %g %x %s %c %lld", -42, 3.14159, 12345.678, 0.0001, 255u, "str", 'z', -9000000000LL);
  check(is(b, "-42  3.14 1.234568e+04 0.0001 ff str z -9000000000"), "printf basic");
  sprintf_(b, sizeof b, "%.0f %.0f %.0f %.1f", 0.5, 1.5, 2.5, 0.25);
  check(is(b, "0 2 2 0.2"), "printf round half even (UCRT)");
  sprintf_(b, sizeof b, "%f %f %f", 1.0 / 0.0, -1.0 / 0.0, 0.0 / 0.0);
  check(is(b, "inf -inf -nan(ind)") || is(b, "inf -inf nan"), "printf inf/nan");
  sprintf_(b, sizeof b, "%p", (void *)0x1234);
  check(is(b, "0000000000001234"), "printf %p");
  sprintf_(b, sizeof b, "%a", 1.0);
  check(is(b, "0x1.0000000000000p+0"), "printf %a");
  sprintf_(b, sizeof b, "%zu %Iu %I64d %hhd", (size_t)7, (size_t)8, 9LL, 300);
  check(is(b, "7 8 9 44"), "printf size modifiers");
  // snprintf (STANDARD_SNPRINTF): the length, a cut string with a NUL
  int r = pf(2, b, 4, "%s", "abcdef");
  check(r == 6 && is(b, "abc"), "snprintf truncation");
  // _vsnprintf (LEGACY_VSPRINTF_NULL_TERMINATION): exactly count: no NUL; more: -1
  memset(b, 'x', 8);
  r = pf(1, b, 3, "%s", "abc");
  check(r == 3 && b[3] == 'x', "_vsnprintf exact");
  r = pf(1, b, 3, "%s", "abcd");
  check(r == -1, "_vsnprintf over");
  // vsprintf_s: too small: an empty string, -1
  r = pf_s(b, 3, "%s", "abcd");
  check(r == -1 && b[0] == 0, "vsprintf_s too small");
  r = pfn_s(b, 8, (size_t)-1, "%s", "abcdefghij");
  check(r == -1 && is(b, "abcdefg"), "_vsnprintf_s _TRUNCATE");
  r = pfn_s(b, 8, 3, "%s", "abcdef");
  check(r == -1 && is(b, "abc"), "_vsnprintf_s count");
  r = pf(2, 0, 0, "%d", 12345);
  check(r == 5, "printf length only");
}

static void scanf_checks() {
  int i = 0, n = 0;
  double d = 0;
  char s[16] = {0};
  long long ll = 0;
  int r = sc("  42 3.5e2 word 0x1F", "%d %lf %15s %x", &i, &d, s, &n);
  check(r == 4 && i == 42 && d == 350.0 && is(s, "word") && n == 31, "sscanf basic");
  r = sc("-9000000000", "%lld", &ll);
  check(r == 1 && ll == -9000000000LL, "sscanf %lld");
  r = sc("", "%d", &i);
  check(r == -1, "sscanf EOF");
  r = sc("abc", "%d", &i);
  check(r == 0, "sscanf no match");
  r = sc("ab12", "%[a-z]%d", s, &i);
  check(r == 2 && is(s, "ab") && i == 12, "sscanf scan set");
}

static void conv_checks() {
  char *e;
  check(strtod("1e-310", &e) > 0 && *e == 0, "strtod subnormal");
  check(strtod("0x1.8p1", &e) == 3.0 && *e == 0, "strtod hex (UCRT)");
  check(strtod("  -inf", &e) == -1.0 / 0.0, "strtod inf");
  *_errno() = 0;
  check(strtod("1e400", &e) == 1.0 / 0.0 && *_errno() == 34, "strtod overflow ERANGE");
  check(strtof("3.4028236e38", &e) == 1.0f / 0.0f, "strtof overflow");
  *_errno() = 0;
  check(strtol("99999999999", &e, 10) == 0x7fffffffL && *_errno() == 34, "strtol ERANGE");
  check(strtoul("-1", &e, 10) == 0xffffffffUL, "strtoul -1");
  check(_strtoi64("-0x8000000000000000", &e, 16) == (long long)0x8000000000000000ULL, "_strtoi64 min");
  check(_strtoui64("18446744073709551615", &e, 10) == 18446744073709551615ULL, "_strtoui64 max");
  check(strtol("0x", &e, 16) == 0 && *e == 'x', "strtol 0x prefix alone");
  *_errno() = 0;
  check(strtod("4.9406564584124654e-324", &e) == 4.9406564584124654e-324 && *_errno() == 0, "strtod subnormal: no ERANGE");
  check(strtod("2.47e-324", &e) == 0 && *_errno() == 34, "strtod underflow to 0: ERANGE");
  check(_atoi64("123456789012") == 123456789012LL && atoi(" -7") == -7, "atoi");
  char b[32];
  check(_i64toa_s(-1234567890123LL, b, sizeof b, 10) == 0 && is(b, "-1234567890123"), "_i64toa_s");
  check(_i64toa_s(123, b, 3, 10) == 34 && b[0] == 0, "_i64toa_s ERANGE");
  check(is(_ui64toa(0xffffffffffffffffULL, b, 16), "ffffffffffffffff"), "_ui64toa");
  int ex;
  check(frexp(48.0, &ex) == 0.75 && ex == 6, "frexp");
  check(ldexp(1.0, -1074) > 0 && ldexp(1.0, 1024) == 1.0 / 0.0, "ldexp");
  double ip;
  check(modf(-2.5, &ip) == -0.5 && ip == -2.0, "modf");
  check(sinf(0.0f) == 0.0f && pow(2.0, 10.0) == 1024.0, "sinf/pow");
}

static void sec_checks() {
  char b[8];
  check(strcpy_s(b, sizeof b, "1234567") == 0 && is(b, "1234567"), "strcpy_s fits");
  check(strcpy_s(b, sizeof b, "12345678") == 34 && b[0] == 0, "strcpy_s ERANGE");
  strcpy_s(b, sizeof b, "abc");
  check(strcat_s(b, sizeof b, "defg") == 0 && is(b, "abcdefg"), "strcat_s");
  check(strncpy_s(b, sizeof b, "abcdefghij", (size_t)-1) == 80 && is(b, "abcdefg"), "strncpy_s _TRUNCATE");
  check(strncpy_s(b, sizeof b, "abcdefghij", 3) == 0 && is(b, "abc"), "strncpy_s count");
  char m[4] = {1, 2, 3, 4};
  check(memmove_s(m, 4, "wxyz!", 5) == 34 && m[0] == 1 && m[3] == 4, "memmove_s ERANGE keeps");
  check(memcpy_s(m, 4, "wxyz!", 5) == 34 && m[0] == 0 && m[3] == 0, "memcpy_s ERANGE zeroes");
  check(memcpy_s(m, 4, "ab", 2) == 0 && m[0] == 'a' && m[1] == 'b' && m[2] == 0, "memcpy_s");
}

static int cmp(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }
static void misc_checks() {
  int v[6] = {5, 3, 9, 1, 7, 3};
  qsort(v, 6, sizeof(int), cmp);
  check(v[0] == 1 && v[1] == 3 && v[2] == 3 && v[5] == 9, "qsort");
  int tm[9] = {0, 0, 0, 1, 0, 70, 0, 0, 0};
  long long t = 0;
  const int *g = (const int *)_gmtime64(&t);
  check(g && g[5] == 70 && g[3] == 1 && g[6] == 4, "_gmtime64");
  char b[64];
  check(strftime(b, sizeof b, "%Y-%m-%d %H:%M:%S %a", g) == 23 && is(b, "1970-01-01 00:00:00 Thu"), "strftime");
  (void)tm;
}

// ---- C++ exceptions (clang emits __CxxFrameHandler3) and SEH (__C_specific_handler) ----
static int dtors, copies;
struct Guard { int *p; ~Guard() { (*p)++; } };
struct Base { int v; Base(int x) : v(x) {} Base(const Base &o) : v(o.v) { copies++; } virtual ~Base() {} };
struct Derived : Base { Derived(int x) : Base(x) {} };
__declspec(noinline) void thrower(int k) {
  Guard g{&dtors};
  if (k == 1) throw 42;
  if (k == 2) throw Derived(7);
  if (k == 3) throw "str";
}
__declspec(noinline) int rethrow() {
  try { thrower(1); } catch (int) { throw; }
  return 0;
}
__declspec(noinline) int nested() {
  int r = 0;
  try {
    thrower(1);
  } catch (int) {
    try { thrower(2); } catch (const Base &b) { r = b.v; }
  }
  return r;
}
static void cxx_checks() {
  int ok = 0;
  try { thrower(1); } catch (int x) { ok += x == 42; }
  try { thrower(2); } catch (const Base &b) { ok += b.v == 7; }
  try { thrower(2); } catch (Base b) { ok += b.v == 7; }
  try { thrower(3); } catch (int) { } catch (...) { ok++; }
  try { rethrow(); } catch (int x) { ok += x == 42; }
  try { thrower(0); ok++; } catch (...) {}
  ok += nested() == 7;
  check(ok == 7, "C++ catches");
  check(dtors == 8, "C++ destructors on unwind");
}
static int filter_calls;
static int filter(unsigned code) { filter_calls++; return code == 0xE0000001 ? 1 : 0; }
static void seh_checks() {
  int hit = 0, fin = 0;
  __try {
    __try {
      RaiseException(0xE0000001, 0, 0, 0);
    } __finally {
      fin++;
    }
  } __except (filter(0xE0000001)) {
    hit++;
  }
  check(hit == 1 && fin == 1 && filter_calls == 1, "SEH __try/__except/__finally");
}

int main() {
  printf_checks();
  scanf_checks();
  conv_checks();
  sec_checks();
  misc_checks();
  cxx_checks();
  seh_checks();
  printf_("x64crt %d/%d %s\n", checks - fails, checks, fails ? "FAIL" : "ok");
  ExitProcess(fails != 0);
}
void operator delete(void *p) noexcept { free(p); }
void operator delete(void *p, size_t) noexcept { free(p); }
extern "C" int _fltused = 0;
extern "C" const void *type_info_vtable[2] __asm__("??_7type_info@@6B@") = {0, 0};
