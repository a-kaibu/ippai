#include <windows.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
int main(void) {
    HMODULE crt=LoadLibraryA("ucrtbase.dll");
    void *(__cdecl *resize)(void *,size_t,size_t)=(void *)GetProcAddress(crt,"_recalloc");
    int *(__cdecl *error)(void)=(void *)GetProcAddress(crt,"_errno");
    CHECK("recalloc export",resize&&error);if(!resize||!error)return 1;
    unsigned char *p=resize(NULL,8,1);int zero=p!=NULL;
    if(p)for(int i=0;i<8;i++)zero&=p[i]==0;
    CHECK("initial zero",zero);if(!p)return 1;
    memset(p,0x53,8);p=resize(p,8,4);
    int preserve=p!=NULL;if(p)for(int i=0;i<8;i++)preserve&=p[i]==0x53;
    CHECK("preserve old bytes",preserve);if(!p)return 1;
    zero=1;for(int i=8;i<32;i++)zero&=p[i]==0;
    CHECK("zero extension",zero);
    void *bad=resize(p,SIZE_MAX,2);
    CHECK("overflow preserves allocation",bad==NULL&&*error()==12&&p[0]==0x53);
    p=resize(p,4,1);CHECK("shrink",p&&p[0]==0x53&&p[3]==0x53);
    CHECK("zero size frees",resize(p,0,1)==NULL);
    return failed;
}
