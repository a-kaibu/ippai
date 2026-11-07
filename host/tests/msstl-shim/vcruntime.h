#pragma once
#include <sal.h>
#define __CLR_OR_THIS_CALL
#define __CLRCALL_OR_CDECL __cdecl
#define __CLRCALL_PURE_OR_CDECL __cdecl
#define __CRTDECL __cdecl
#include <stddef.h>
#include <stdint.h>
#ifndef _VCRTIMP
#define _VCRTIMP __declspec(dllimport)
#endif
#ifndef _CRTIMP2
#define _CRTIMP2 __declspec(dllimport)
#endif
#ifndef _CRT_BEGIN_C_HEADER
#define _CRT_BEGIN_C_HEADER extern "C" {
#define _CRT_END_C_HEADER }
#endif
#define _VCRT_ALLOCATOR __declspec(allocator)
#define _CRT_STRINGIZE_(x) #x
#define _CRT_STRINGIZE(x) _CRT_STRINGIZE_(x)
#define _CRT_WIDE_(s) L ## s
#define _CRT_WIDE(s) _CRT_WIDE_(s)
#define _CRT_CONCATENATE_(a, b) a ## b
#define _CRT_CONCATENATE(a, b)  _CRT_CONCATENATE_(a, b)
#ifndef _HAS_EXCEPTIONS
#define _HAS_EXCEPTIONS 1
#endif
#define _CRT_UNPARENTHESIZE_(...) __VA_ARGS__
#define _CRT_UNPARENTHESIZE(...)  _CRT_UNPARENTHESIZE_ __VA_ARGS__
#ifndef _VCRT_NOALIAS
#define _VCRT_NOALIAS __declspec(noalias)
#endif
#define _VCRT_RESTRICT __declspec(restrict)
#define _CRT_INSECURE_DEPRECATE(x)
#define _CRT_DEPRECATE_TEXT(x)
#define _CRT_SECURE_NO_WARNINGS 1
#define _NODISCARD [[nodiscard]]
#define _VCRUNTIME_DISABLED_WARNINGS
#define _CRT_ALIGN(x) __declspec(align(x))
#define _VCRUNTIME_EQUALITY_FN_ARGS
#define _HAS_NODISCARD 1
#define _CRT_GUARDOVERFLOW
#ifndef _HAS_CXX17
#if _MSVC_LANG > 201402L
#define _HAS_CXX17 1
#else
#define _HAS_CXX17 0
#endif
#endif
#ifndef _HAS_CXX20
#if _HAS_CXX17 && _MSVC_LANG > 201703L
#define _HAS_CXX20 1
#else
#define _HAS_CXX20 0
#endif
#endif
#ifndef _HAS_CXX23
#define _HAS_CXX23 0
#endif
#ifndef _HAS_CXX26
#define _HAS_CXX26 0
#endif
#define _CRT_SATELLITE_1
#define _CRT_SATELLITE_2
#define _CRT_SATELLITE_CODECVT_IDS
#define _CRT_SECURE_INVALID_PARAMETER(expr) __debugbreak()
#define _ACRTIMP __declspec(dllimport)
#define _Deref_post_opt_valid_
#define _CRT_SATELLITE_CODECVT_IDS_NOIMPORT
