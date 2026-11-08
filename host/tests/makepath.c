#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>
#include <errno.h>
int main(void){char p[64];wchar_t w[64];int r=_makepath_s(p,64,"C:","dir","game","exe");printf("ansi: %d %d\n",r,strcmp(p,"C:dir\\game.exe")==0);r=_wmakepath_s(w,64,L"C:",L"dir",L"game",L".exe");printf("wide: %d %d\n",r,wcscmp(w,L"C:dir\\game.exe")==0);r=_makepath_s(p,64,0,0,"name",0);printf("optional: %d %d\n",r,strcmp(p,"name")==0);memset(p,0x55,sizeof(p));errno=0;r=_makepath_s(p,3,0,0,"long",0);printf("short: %d %d %d\n",r,errno==ERANGE,p[0]==0&&p[3]==0x55);errno=0;r=_makepath_s(0,64,0,0,"name",0);printf("null: %d %d\n",r,errno==EINVAL);_makepath(p,"D:","dir/","game","dat");printf("legacy: %d\n",strcmp(p,"D:dir/game.dat")==0);_wmakepath(w,0,L"dir",L"game",L"dat");printf("legacy wide: %d\n",wcscmp(w,L"dir\\game.dat")==0);return 0;}
