// 64-bit comparisons (cmp + sbb on 32-bit) and carries (add + adc): the JIT reads B/BE/L/LE
// straight from SBB's and CF from ADC's lazy flags. Expected (gcc -O2 on Linux too): "c64 87480b07".
//   i686-w64-mingw32-gcc -O2 cmp64.c -o cmp64.exe
#include <stdio.h>
#include <stdint.h>
typedef long long ll; typedef unsigned long long ull;
__attribute__((noinline)) int f(ll a, ll b){ return (a<b)|(a<=b)<<1|(a>b)<<2|(a>=b)<<3; }
__attribute__((noinline)) int g(ull a, ull b){ return (a<b)|(a<=b)<<1|(a>b)<<2|(a>=b)<<3; }
__attribute__((noinline)) int h(ll a, ll b){ int r=0; if(a<b) r|=1; if(a<=b) r|=2; if((ull)a<(ull)b) r|=4; if((ull)a<=(ull)b) r|=8; return r; }
__attribute__((noinline)) ull add(ull a, ull b){ return a+b < a; }
int main(){
  ll v[]={0,1,-1,2,-2,0x7fffffffLL,0x80000000LL,-0x80000000LL,0xffffffffLL,0x100000000LL,-0x100000000LL,0x7fffffffffffffffLL,(ll)0x8000000000000000ULL,0x123456789LL,0x123456788LL,0x223456789LL,-0x123456789LL,0x1ffffffffLL};
  int n=sizeof v/sizeof v[0]; unsigned s=0;
  for(int i=0;i<n;i++) for(int j=0;j<n;j++){ s=s*31+f(v[i],v[j]); s=s*31+g(v[i],v[j]); s=s*31+h(v[i],v[j]); s=s*31+(unsigned)add(v[i],v[j]); }
  printf("c64 %08x\n", s); return 0; }
