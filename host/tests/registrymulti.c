/* libs: -ladvapi32
 * Multiple guest processes update a common persistent registry concurrently. */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
static const char path[]="Software\\sake-registry-concurrency";
static int worker(int id) {
 HKEY key; if(RegCreateKeyExA(HKEY_CURRENT_USER,path,0,NULL,0,KEY_ALL_ACCESS,NULL,&key,NULL))return 1;
 HANDLE gate=OpenEventA(SYNCHRONIZE,FALSE,"sake-registry-gate");
 char ready_name[40];snprintf(ready_name,sizeof(ready_name),"sake-registry-ready-%d",id);
 HANDLE ready=OpenEventA(EVENT_MODIFY_STATE,FALSE,ready_name);if(!ready)return 2;SetEvent(ready);CloseHandle(ready);
 if(!gate||WaitForSingleObject(gate,10000)!=WAIT_OBJECT_0)return 2;
 for(DWORD i=0;i<40;i++){char name[32];snprintf(name,sizeof(name),"p%d-%lu",id,i);
 if(RegSetValueExA(key,name,0,REG_DWORD,(BYTE *)&i,sizeof(i)))return 3;
 Sleep(1);}
 RegCloseKey(key);CloseHandle(gate);return 0;
}
int main(int argc,char **argv) {
 if(argc>1)return worker(atoi(argv[1]));
 HKEY key;RegDeleteKeyA(HKEY_CURRENT_USER,path);
 if(RegCreateKeyExA(HKEY_CURRENT_USER,path,0,NULL,0,KEY_ALL_ACCESS,NULL,&key,NULL))return 1;
 DWORD old=99;RegSetValueExA(key,"deleted",0,REG_DWORD,(BYTE *)&old,sizeof(old));
 HANDLE gate=CreateEventA(NULL,TRUE,FALSE,"sake-registry-gate");
 HANDLE ready[2]={CreateEventA(NULL,TRUE,FALSE,"sake-registry-ready-1"),CreateEventA(NULL,TRUE,FALSE,"sake-registry-ready-2")};
 PROCESS_INFORMATION pi[2]={{0}};int ok=1;char image[MAX_PATH];GetModuleFileNameA(NULL,image,sizeof(image));
 for(int i=0;i<2;i++){char cmd[MAX_PATH+32];snprintf(cmd,sizeof(cmd),"\"%s\" %d",image,i+1);STARTUPINFOA si={sizeof(si)};
 if(!CreateProcessA(NULL,cmd,NULL,NULL,FALSE,0,NULL,NULL,&si,&pi[i]))ok=0;}
 // Child initialization has loaded the old snapshot before the concurrent writes.
 if(WaitForMultipleObjects(2,ready,TRUE,10000)!=WAIT_OBJECT_0)ok=0;
 CloseHandle(ready[0]);CloseHandle(ready[1]);RegDeleteValueA(key,"deleted");SetEvent(gate);
 for(int i=0;i<2;i++){DWORD code=99;if(pi[i].hProcess){if(WaitForSingleObject(pi[i].hProcess,10000)==WAIT_OBJECT_0)GetExitCodeProcess(pi[i].hProcess,&code);CloseHandle(pi[i].hThread);CloseHandle(pi[i].hProcess);}if(code)ok=0;}
 printf("concurrent registry processes: %s\n",ok?"ok":"FAIL");
 for(int id=1;id<=2;id++)for(DWORD i=0;i<40;i++){char name[32];DWORD value=99,n=sizeof(value),type=0;snprintf(name,sizeof(name),"p%d-%lu",id,i);
 if(RegQueryValueExA(key,name,NULL,&type,(BYTE *)&value,&n)||type!=REG_DWORD||value!=i)ok=0;}
 printf("preserve independent registry writes: %s\n",ok?"ok":"FAIL");
 DWORD n=sizeof(old);int deleted=RegQueryValueExA(key,"deleted",NULL,NULL,(BYTE *)&old,&n)==ERROR_FILE_NOT_FOUND;
 printf("deleted registry value stays deleted: %s\n",deleted?"ok":"FAIL");
 RegCloseKey(key);RegDeleteKeyA(HKEY_CURRENT_USER,path);CloseHandle(gate);return !ok||!deleted;
}
