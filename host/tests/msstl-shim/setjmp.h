#pragma once
typedef struct __declspec(align(16)) _SETJMP_FLOAT128 { unsigned __int64 Part[2]; } SETJMP_FLOAT128;
typedef SETJMP_FLOAT128 _JBTYPE;
#define _JBLEN 16
typedef _JBTYPE jmp_buf[_JBLEN];
#define _JMP_BUF_DEFINED
#ifdef __cplusplus
extern "C"
#endif
__declspec(noreturn) void __cdecl longjmp(jmp_buf, int);
