#pragma once
#include_next <stdlib.h>
extern "C" { int __cdecl at_quick_exit(void(__cdecl*)(void)); __declspec(noreturn) void __cdecl quick_exit(int); }
extern "C++" {
inline long __cdecl abs(long x) { return labs(x); }
inline long long __cdecl abs(long long x) { return llabs(x); }
}
