// What a /MD program gets from the static parts of the VC++ runtime (msvcrt.lib, msvcprt.lib):
// operator new/delete over malloc/free, _fltused, the pure-call handler, printf (the UCRT's is
// an inline function over __stdio_common_vfprintf, no DLL exports it) and the STL's vector
// algorithms it uses, std::_Facet_Register (locale0_implib.cpp) and type_info's vftable (each
// module has its own: no DLL exports them either; checked against Microsoft's msvcp140.dll
// 14.51's exports, tests/winref host_msvcstream).
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <new>
extern "C" __declspec(dllimport) int __cdecl __stdio_common_vfprintf(unsigned long long, FILE*, const char*, _locale_t, va_list);
extern "C" int __cdecl printf(const char* f, ...) {
    va_list a;
    va_start(a, f);
    // (_CRT_INTERNAL_LOCAL_PRINTF_OPTIONS: legacy wide specifiers, as the UCRT's inline printf)
    int r = __stdio_common_vfprintf(4, __acrt_iob_func(1), f, nullptr, a);
    va_end(a);
    return r;
}
extern "C" const void* __stdcall __std_find_trivial_2(const void* first, const void* last, unsigned short v) noexcept {
    auto p = static_cast<const unsigned short*>(first);
    while (p != last && *p != v) ++p;
    return p;
}
extern "C" int _fltused = 0;
extern "C" int __cdecl _purecall() { abort(); }
void* __cdecl operator new(size_t n) { if (void* p = malloc(n ? n : 1)) return p; throw std::bad_alloc(); }
void* __cdecl operator new[](size_t n) { return operator new(n); }
void* __cdecl operator new(size_t n, const std::nothrow_t&) noexcept { return malloc(n ? n : 1); }
void __cdecl operator delete(void* p) noexcept { free(p); }
void __cdecl operator delete[](void* p) noexcept { free(p); }
void __cdecl operator delete(void* p, size_t) noexcept { free(p); }
void __cdecl operator delete[](void* p, size_t) noexcept { free(p); }

namespace std {
class _Facet_base;
// (msvcprt.lib's frees the facets at exit; a test program leaves them)
void __cdecl _Facet_Register(_Facet_base*) {}
}
// type_info's vftable: its one entry the scalar deleting destructor (never called: no RTTI
// object is deleted)
static void* __cdecl type_info_dtor(void* p, unsigned) { return p; }
extern "C" const void* const type_info_vftable[1] __asm__("??_7type_info@@6B@") = {(const void*)&type_info_dtor};
