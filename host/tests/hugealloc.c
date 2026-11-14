/* Sizes near 4 GB (review b07): reservations, commits and heap blocks fail with an error
   instead of rounding to an empty region the program then takes for 4 GB (the error code is
   not compared: Windows gives ERROR_NOT_ENOUGH_MEMORY or ERROR_INVALID_PARAMETER by size). */
#include <windows.h>
#include <stdio.h>

int main(void){
 SIZE_T sizes[]={0xfffff001u,0xffffffffu,0xfffff000u,0x80000000u};
 for(int i=0;i<4;i++){
  SetLastError(0);
  void *p=VirtualAlloc(NULL,sizes[i],MEM_RESERVE,PAGE_READWRITE);
  DWORD e=GetLastError();
  printf("reserve %08lx: %d error %d\n",(unsigned long)sizes[i],p!=NULL,e!=0);
  if(p)VirtualFree(p,0,MEM_RELEASE);
  SetLastError(0);
  p=VirtualAlloc(NULL,sizes[i],MEM_COMMIT|MEM_RESERVE,PAGE_READWRITE);
  e=GetLastError();
  printf("commit %08lx: %d error %d\n",(unsigned long)sizes[i],p!=NULL,e!=0);
  if(p)VirtualFree(p,0,MEM_RELEASE);
  p=HeapAlloc(GetProcessHeap(),0,sizes[i]-16);
  printf("heap %08lx: %d\n",(unsigned long)sizes[i]-16,p!=NULL);
  if(p)HeapFree(GetProcessHeap(),0,p);
 }
 /* still works after */
 char *q=VirtualAlloc(NULL,0x10000,MEM_COMMIT|MEM_RESERVE,PAGE_READWRITE);
 q[0xffff]=1;
 printf("small reserve after: %d\n",q!=NULL);
 return 0;
}
