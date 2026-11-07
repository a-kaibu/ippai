#pragma once
#include <vcruntime.h>
extern "C++" {
typedef void (__cdecl* terminate_handler)();
typedef void (__cdecl* unexpected_handler)();
__declspec(noreturn) void __cdecl terminate() noexcept;
__declspec(noreturn) void __cdecl unexpected();
terminate_handler __cdecl set_terminate(terminate_handler) noexcept;
terminate_handler __cdecl _get_terminate();
unexpected_handler __cdecl set_unexpected(unexpected_handler) noexcept;
unexpected_handler __cdecl _get_unexpected();
}
extern "C" int __cdecl __uncaught_exception();
extern "C" int __cdecl __uncaught_exceptions();
