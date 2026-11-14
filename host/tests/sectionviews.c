/* Pagefile sections and file views (review b07): a section's memory goes once its last view is
   unmapped and its last handle closed, in either order (unmap then close leaked 1 MB a round);
   views of one section share their bytes; file views past the file fail with
   ERROR_ACCESS_DENIED; read-only views of a file show its bytes; snapshots close. */
#include <windows.h>
#include <tlhelp32.h>
#include <stdio.h>
#include <string.h>

static int round_trip(int close_first){
 HANDLE m=CreateFileMappingA(INVALID_HANDLE_VALUE,NULL,PAGE_READWRITE,0,1<<20,NULL);
 if(!m)return 1;
 unsigned char *v=MapViewOfFile(m,FILE_MAP_ALL_ACCESS,0,0,0);
 if(!v){CloseHandle(m);return 2;}
 memset(v,0x5a,1<<20);
 if(close_first){CloseHandle(m);if(v[12345]!=0x5a)return 3;UnmapViewOfFile(v);}
 else{UnmapViewOfFile(v);CloseHandle(m);}
 return 0;
}

int main(void){
 int r,fail=0;
 for(r=0;r<2500&&!fail;r++)fail=round_trip(0);
 printf("unmap then close: rounds %d failure %d\n",r,fail);
 fail=0;
 for(r=0;r<2500&&!fail;r++)fail=round_trip(1);
 printf("close then unmap: rounds %d failure %d\n",r,fail);

 /* two views of one section */
 HANDLE m=CreateFileMappingA(INVALID_HANDLE_VALUE,NULL,PAGE_READWRITE,0,0x20000,NULL);
 char *a=MapViewOfFile(m,FILE_MAP_ALL_ACCESS,0,0,0);
 char *b=MapViewOfFile(m,FILE_MAP_ALL_ACCESS,0,0x10000,0x10000);
 strcpy(a+0x10000,"shared");
 printf("second view sees the first's bytes: %d\n",b&&!strcmp(b,"shared"));
 UnmapViewOfFile(a);
 strcpy(b,"still");
 printf("a view outlives the other: %d\n",!strcmp(b,"still"));
 SetLastError(0);
 char *c=MapViewOfFile(m,FILE_MAP_ALL_ACCESS,0,0x10000,0x20000);
 DWORD e=GetLastError();
 printf("view past the section: %d error %lu\n",c!=NULL,e);
 UnmapViewOfFile(b);CloseHandle(m);

 /* a file's views */
 char path[MAX_PATH],dir[MAX_PATH];GetTempPathA(MAX_PATH,dir);GetTempFileNameA(dir,"sv",0,path);
 HANDLE f=CreateFileA(path,GENERIC_READ|GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 static char buf[0x30000];for(int i=0;i<(int)sizeof(buf);i++)buf[i]=(char)(i*7);
 DWORD w;WriteFile(f,buf,sizeof(buf),&w,NULL);
 HANDLE fm=CreateFileMappingA(f,NULL,PAGE_READONLY,0,0,NULL);
 char *v1=MapViewOfFile(fm,FILE_MAP_READ,0,0,0);
 char *v2=MapViewOfFile(fm,FILE_MAP_READ,0,0,0);
 char *v3=MapViewOfFile(fm,FILE_MAP_READ,0,0x20000,0x10000);
 printf("file views: %d %d %d\n",v1&&!memcmp(v1,buf,sizeof(buf)),v2&&!memcmp(v2,buf,sizeof(buf)),v3&&!memcmp(v3,buf+0x20000,0x10000));
 UnmapViewOfFile(v1);
 printf("the other view after one went: %d\n",!memcmp(v2,buf,sizeof(buf)));
 SetLastError(0);
 char *v4=MapViewOfFile(fm,FILE_MAP_READ,0,0x20000,0x20000);
 e=GetLastError();
 printf("file view past the end: %d error %lu\n",v4!=NULL,e);
 SetLastError(0);
 v4=MapViewOfFile(fm,FILE_MAP_READ,0,0x40000,0);
 e=GetLastError();
 printf("file view starting past the end: %d error %lu\n",v4!=NULL,e);
 UnmapViewOfFile(v2);UnmapViewOfFile(v3);CloseHandle(fm);CloseHandle(f);DeleteFileA(path);

 /* snapshots */
 int snaps=0;
 for(int i=0;i<3000;i++){HANDLE s=CreateToolhelp32Snapshot(TH32CS_SNAPMODULE,0);if(s==INVALID_HANDLE_VALUE)break;
  MODULEENTRY32 me;me.dwSize=sizeof(me);if(Module32First(s,&me))snaps++;CloseHandle(s);}
 printf("snapshots listed: %d\n",snaps);
 return 0;
}
