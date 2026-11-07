// What a /MD program gets from the static parts of the VC++ runtime (msvcrt.lib): operator
// new/delete over malloc/free, _fltused, the pure-call handler.
#include <stdlib.h>
#include <new>
extern "C" int _fltused = 0;
extern "C" int __cdecl _purecall() { abort(); }
void* __cdecl operator new(size_t n) { if (void* p = malloc(n ? n : 1)) return p; throw std::bad_alloc(); }
void* __cdecl operator new[](size_t n) { return operator new(n); }
void* __cdecl operator new(size_t n, const std::nothrow_t&) noexcept { return malloc(n ? n : 1); }
void __cdecl operator delete(void* p) noexcept { free(p); }
void __cdecl operator delete[](void* p) noexcept { free(p); }
void __cdecl operator delete(void* p, size_t) noexcept { free(p); }
void __cdecl operator delete[](void* p, size_t) noexcept { free(p); }
