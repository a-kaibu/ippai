/*
 * What a Wine PE DLL compiled to wasm needs besides its own sources (instead of winecrt0's
 * x86 parts): the entry point vnhost points the x86 DLL's entry at, and stubs for x86-only
 * helpers. See docs/wasm-dlls.md.
 */
#include <stdarg.h>
#include "windef.h"
#include "winbase.h"

#ifdef VNWASM_DLLMAIN
BOOL WINAPI DllMain( HINSTANCE inst, DWORD reason, void *reserved );
#endif

BOOL WINAPI __vnwasm_entry( HINSTANCE inst, DWORD reason, void *reserved )
{
#ifdef VNWASM_DLLMAIN
    return DllMain( inst, reason, reserved );
#else
    if (reason == DLL_PROCESS_ATTACH) DisableThreadLibraryCalls( inst );
    return TRUE;
#endif
}

/* setjmp/longjmp (libpng's error path): wasm code can't be unwound into */
typedef int jmp_buf_t[16];
int __cdecl __intrinsic_setjmp( jmp_buf_t buf ) { return 0; }
int __cdecl _setjmp( jmp_buf_t buf ) { return 0; }
void __cdecl longjmp( jmp_buf_t buf, int val ) { __builtin_trap(); }
