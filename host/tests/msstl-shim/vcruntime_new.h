#pragma once
#include <vcruntime.h>
namespace std {
enum class align_val_t : size_t {};
struct nothrow_t { explicit nothrow_t() = default; };
extern nothrow_t const nothrow;
}
[[nodiscard]] void* __cdecl operator new(size_t);
[[nodiscard]] void* __cdecl operator new(size_t, std::nothrow_t const&) noexcept;
[[nodiscard]] void* __cdecl operator new[](size_t);
[[nodiscard]] void* __cdecl operator new[](size_t, std::nothrow_t const&) noexcept;
void __cdecl operator delete(void*) noexcept;
void __cdecl operator delete(void*, std::nothrow_t const&) noexcept;
void __cdecl operator delete[](void*) noexcept;
void __cdecl operator delete[](void*, std::nothrow_t const&) noexcept;
void __cdecl operator delete(void*, size_t) noexcept;
void __cdecl operator delete[](void*, size_t) noexcept;
[[nodiscard]] void* __cdecl operator new(size_t, std::align_val_t);
[[nodiscard]] void* __cdecl operator new[](size_t, std::align_val_t);
void __cdecl operator delete(void*, std::align_val_t) noexcept;
void __cdecl operator delete[](void*, std::align_val_t) noexcept;
void __cdecl operator delete(void*, size_t, std::align_val_t) noexcept;
void __cdecl operator delete[](void*, size_t, std::align_val_t) noexcept;
#ifndef __PLACEMENT_NEW_INLINE
#define __PLACEMENT_NEW_INLINE
inline void* __cdecl operator new(size_t, void* _Where) noexcept { return _Where; }
inline void __cdecl operator delete(void*, void*) noexcept {}
#endif
#ifndef __PLACEMENT_VEC_NEW_INLINE
#define __PLACEMENT_VEC_NEW_INLINE
inline void* __cdecl operator new[](size_t, void* _Where) noexcept { return _Where; }
inline void __cdecl operator delete[](void*, void*) noexcept {}
#endif
