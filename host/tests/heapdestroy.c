/* A private heap's blocks go with HeapDestroy (review b07): 1500 rounds of a heap filled with
   about 2 MB of small and big blocks, then destroyed, fit in the address space only if the
   blocks are freed; blocks of other heaps and of the process heap keep their bytes. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

int main(void){
 HANDLE keep=HeapCreate(0,0,0);
 char *kept[64],*plain[64];
 for(int i=0;i<64;i++){kept[i]=HeapAlloc(keep,0,i%2?40:5000);memset(kept[i],'k',i%2?40:5000);
  plain[i]=HeapAlloc(GetProcessHeap(),0,i%2?24:3000);memset(plain[i],'p',i%2?24:3000);}
 int rounds=0,fail=0;
 for(int r=0;r<1500&&!fail;r++){
  HANDLE h=HeapCreate(0,0,0);
  if(!h){fail=1;break;}
  for(int i=0;i<16;i++){void *p=HeapAlloc(h,0,100000);if(!p){fail=2;break;}memset(p,r,64);}
  for(int i=0;i<2000&&!fail;i++){char *p=HeapAlloc(h,0,(i%31)*16+8);if(!p){fail=3;break;}p[0]=(char)i;}
  /* one that moves */
  void *m=HeapAlloc(h,0,32);m=HeapReAlloc(h,0,m,200000);if(!m)fail=4;
  if(!HeapDestroy(h))fail=5;
  rounds++;
 }
 printf("rounds %d failure %d\n",rounds,fail);
 int ok=1;
 for(int i=0;i<64;i++){
  for(int j=0;j<(i%2?40:5000);j++)if(kept[i][j]!='k')ok=0;
  for(int j=0;j<(i%2?24:3000);j++)if(plain[i][j]!='p')ok=0;
 }
 printf("other heaps kept their bytes: %d\n",ok);
 printf("destroy the kept heap: %d\n",HeapDestroy(keep));
 for(int i=0;i<64;i++)HeapFree(GetProcessHeap(),0,plain[i]);
 return 0;
}
