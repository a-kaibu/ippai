// MSVC C++ exceptions (x86): catch by type, by base class reference and by value, catch(...),
// rethrow, destructors. Prints "ok" when right. Build (clang, no MSVC headers or libraries):
//   printf 'LIBRARY msvcrt.dll\nEXPORTS\n_CxxThrowException@8\n__CxxFrameHandler3\nprintf\n' > m.def
//   printf 'LIBRARY kernel32.dll\nEXPORTS\nExitProcess@4\n' > k.def
//   llvm-dlltool -m i386 -d m.def -l m.lib -k; llvm-dlltool -m i386 -d k.def -l k.lib -k
//   clang++ --target=i686-pc-windows-msvc -O1 -fexceptions -fcxx-exceptions -c cxxtest.cpp
//   lld-link /subsystem:console /entry:main /nodefaultlib /out:cxxtest.exe cxxtest.o m.lib k.lib
extern "C" int printf(const char *, ...);
extern "C" __declspec(dllimport) void __stdcall ExitProcess(unsigned);
static int dtors, copies;
struct Guard { int *p; ~Guard() { (*p)++; } };
struct Base { int v; Base(int x) : v(x) {} Base(const Base &o) : v(o.v) { copies++; } virtual ~Base() {} };
struct Derived : Base { Derived(int x) : Base(x) {} };
__declspec(noinline) void thrower(int k) {
  int local = 0; Guard g{&dtors};
  if (k == 1) throw 42;
  if (k == 2) throw Derived(7);
  if (k == 3) throw "str";
}
__declspec(noinline) int rethrow() {
  try { thrower(1); } catch (int) { throw; }
  return 0;
}
int main() {
  int ok = 0;
  try { thrower(1); } catch (int x) { ok += x == 42; }
  try { thrower(2); } catch (const Base &b) { ok += b.v == 7; }
  try { thrower(2); } catch (Base b) { ok += b.v == 7; }
  try { thrower(3); } catch (int) { } catch (...) { ok++; }
  try { rethrow(); } catch (int x) { ok += x == 42; }
  try { thrower(0); ok++; } catch (...) {}
  printf("cxx %d/6 dtors %d copies %d %s\n", ok, dtors, copies, ok == 6 && dtors == 6 ? "ok" : "FAIL");
  ExitProcess(0);
}
void operator delete(void *) noexcept {}
extern "C" const void *type_info_vtable[2] __asm__("??_7type_info@@6B@") = {0, 0};
