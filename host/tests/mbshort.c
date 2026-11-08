/* libs: -loleaut32 */
#include <windows.h>
#include <oleauto.h>
#include <stdio.h>
#include <wchar.h>
int main(void){
 WCHAR w[8];for(int i=0;i<8;i++)w[i]=0x5555;
 SetLastError(0);int n=MultiByteToWideChar(932,0,"token",-1,w,5);DWORD e=GetLastError();
 printf("short ascii: %d %lu %d %d\n",n,(unsigned long)e,wmemcmp(w,L"token",5)==0,w[5]==0x5555);
 w[5]=0;BSTR b=SysAllocString(w);printf("MFC BSTR: %u %d\n",SysStringLen(b),wcscmp(b,L"token")==0);SysFreeString(b);
 for(int i=0;i<8;i++)w[i]=0x5555;
 SetLastError(0);n=MultiByteToWideChar(932,0,"\x82\xa0Z",-1,w,2);e=GetLastError();
 printf("short dbcs: %d %lu %d %d\n",n,(unsigned long)e,w[0]==0x3042&&w[1]=='Z',w[2]==0x5555);
 for(int i=0;i<8;i++)w[i]=0x5555;
 n=MultiByteToWideChar(932,0,"ok",-1,w,8);printf("full: %d %d %d\n",n,wcscmp(w,L"ok")==0,w[3]==0x5555);
 return 0;
}
