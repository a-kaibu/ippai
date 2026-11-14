/* A window procedure that sends itself a message without end, 256 KiB of stack a level
   (review b07): the process ends with STATUS_STACK_OVERFLOW, as on Windows, instead of the
   host writing callback frames below the thread's stack. Run as a child; the parent prints
   its exit code. */
#include <windows.h>
#include <malloc.h>
#include <stdio.h>
#include <string.h>

static volatile int depth;
static LRESULT CALLBACK proc(HWND h,UINT m,WPARAM w,LPARAM l){
 if(m==WM_USER){
  volatile char *p=_alloca(262144);
  memset((char *)p,depth,262144);
  depth++;
  return SendMessageA(h,WM_USER,0,0)+p[100];
 }
 return DefWindowProcA(h,m,w,l);
}

int main(int argc,char **argv){
 if(argc>1&&!strcmp(argv[1],"child")){
  WNDCLASSA wc={0};wc.lpfnWndProc=proc;wc.lpszClassName="deep";RegisterClassA(&wc);
  HWND h=CreateWindowA("deep","",0,0,0,10,10,NULL,NULL,NULL,NULL);
  SendMessageA(h,WM_USER,0,0);
  return 0;
 }
 char exe[MAX_PATH],cmd[MAX_PATH+16];GetModuleFileNameA(NULL,exe,MAX_PATH);
 wsprintfA(cmd,"\"%s\" child",exe);
 /* (its output, a crash report among it, goes nowhere) */
 SECURITY_ATTRIBUTES sa={sizeof(sa),NULL,TRUE};
 HANDLE nul=CreateFileA("NUL",GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,&sa,OPEN_EXISTING,0,NULL);
 STARTUPINFOA si={sizeof(si)};PROCESS_INFORMATION pi;
 si.dwFlags=STARTF_USESTDHANDLES;si.hStdInput=nul;si.hStdOutput=nul;si.hStdError=nul;
 if(!CreateProcessA(NULL,cmd,NULL,NULL,TRUE,0,NULL,NULL,&si,&pi)){printf("no child %lu\n",GetLastError());return 1;}
 WaitForSingleObject(pi.hProcess,60000);
 DWORD code=0;GetExitCodeProcess(pi.hProcess,&code);
 printf("child exit %08lx\n",code);
 return 0;
}
