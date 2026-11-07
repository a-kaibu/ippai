#pragma once
#include_next <stdio.h>
#include <wchar.h>
#ifdef __cplusplus
extern "C"
#endif
__declspec(dllimport) int __cdecl _get_stream_buffer_pointers(FILE*, char***, char***, int**);
