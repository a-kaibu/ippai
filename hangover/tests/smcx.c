/* Code in the exe's own W|X section (like ASProtect) rewritten in place by code in the same
 * section, with no protection change or cache flush, after both ran hot. */
#include <stdio.h>
#include <string.h>
#include <windows.h>
/* f: mov eax,[esp+4]; add eax,imm32; ret   w: xor [ecx],edx; ret   s: mov edi,ecx; mov al,dl; stosb; ret */
__attribute__((section(".rwx"))) unsigned char code[64] = {
  0x8b,0x44,0x24,0x04, 0x05,1,0,0,0, 0xc3,  0,0,0,0,0,0,
  0x31,0x11,0xc3, 0,
  0x57, 0x89,0xcf, 0x88,0xd0, 0xaa, 0x5f, 0xc3,
};
typedef unsigned (*fn)(unsigned);
typedef void (__fastcall *wfn)(void *, unsigned);
__attribute__((noinline)) static unsigned run(fn f){ unsigned a=0; for(int i=0;i<200000;i++) a+=f(i); return a; }
__attribute__((noinline)) static void wr(wfn w, unsigned m, int n){ for(int i=0;i<n;i++) w(code+5, i==n-1 ? m : 0); }
int main(void){
  fn f=(fn)code; wfn w=(wfn)(code+16), s=(wfn)(code+20);
  unsigned a=0,b=0,c=0;
  a=run(f);
  wr(w, 1 ^ 7, 20000);                             /* imm 1 -> 7 (xor 0 before: hot) */
  b=run(f);
  wr(s, 7, 20000); wr(s, 9, 2);                    /* imm 7 -> 9 by stosb */
  c=run(f);
  printf("smcx %u %u %u %s\n", a, b, c, (a-b== (unsigned)(-6*200000) && c-b==2*200000)?"ok":"BAD");
  return 0;
}
