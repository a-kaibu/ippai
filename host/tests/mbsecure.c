#include <windows.h>
#include <stdio.h>
#include <string.h>
static int failed;
/* (ucrtbase's invalid-parameter handler returns, so its checks answer EINVAL instead of
   ending the process; stdout unbuffered: Windows ended this probe with nothing printed) */
static void __cdecl ignore_invalid(const wchar_t *e, const wchar_t *f, const wchar_t *file, unsigned line, UINT_PTR r) {
    (void)e; (void)f; (void)file; (void)line; (void)r;
}
typedef void (__cdecl *invalid_handler)(const wchar_t *, const wchar_t *, const wchar_t *, unsigned, UINT_PTR);
static void returning_invalid_parameters(HMODULE ucrt) {
    invalid_handler (__cdecl *set)(invalid_handler) = (void *)GetProcAddress(ucrt, "_set_invalid_parameter_handler");
    if (set) set(ignore_invalid);
    setvbuf(stdout, NULL, _IONBF, 0);
}
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
int main(void){HMODULE m=LoadLibraryA("ucrtbase.dll");if(!m)return 1;returning_invalid_parameters(m);int (__cdecl *copy)(unsigned char *,size_t,const unsigned char *,size_t)=(void *)GetProcAddress(m,"_mbsnbcpy_s");if(!copy)return 1;
 /* (the split character only exists in a DBCS code page: a Windows runner's ANSI code page is 1252) */
 int (__cdecl *setmbcp)(int)=(void *)GetProcAddress(m,"_setmbcp");if(!setmbcp||setmbcp(932))return 1;unsigned char out[8],source[]={0x82,0xa0,'X',0};memset(out,0xa5,sizeof(out));
 CHECK("secure multibyte ASCII",copy(out,sizeof(out),(unsigned char *)"abc",3)==0&&!strcmp((char *)out,"abc")&&out[4]==0xa5);
 CHECK("complete multibyte character",copy(out,sizeof(out),source,2)==0&&out[0]==0x82&&out[1]==0xa0&&out[2]==0);
 {int r=copy(out,sizeof(out),source,1);printf("split multibyte character: %d, out %02x\n",r,out[0]);}
 CHECK("secure multibyte overflow",copy(out,2,(unsigned char *)"abc",3)!=0&&out[0]==0);
 CHECK("secure multibyte truncation",copy(out,3,source,(size_t)-1)==80&&out[0]==0x82&&out[1]==0xa0&&out[2]==0);
 CHECK("zero count accepts null source",copy(out,sizeof(out),NULL,0)==0&&out[0]==0);
 CHECK("secure multibyte null rejected",copy(out,sizeof(out),NULL,1)==22&&out[0]==0);return failed;}
