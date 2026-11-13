/* libs: -luser32 */
/* A rich edit control (RichEdit20W, riched20.dll) given its text by EM_STREAMIN: RTF through
 * the program's EDITSTREAM callback, a few bytes at a time (a license agreement, #164), plain
 * ANSI text, and a callback that fails. WM_GETTEXT gives the text. */
#include <windows.h>
#include <richedit.h>
#include <stdio.h>
#include <string.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
typedef struct { const char *p; int left, step, calls; } Src;
static DWORD CALLBACK feed(DWORD_PTR cookie, LPBYTE buf, LONG cb, LONG *pcb){
 Src *s=(Src*)cookie; int n=s->left<s->step?s->left:s->step; if(n>cb)n=cb;
 memcpy(buf,s->p,n); s->p+=n; s->left-=n; *pcb=n; s->calls++; return 0;}
static DWORD CALLBACK broken(DWORD_PTR cookie, LPBYTE buf, LONG cb, LONG *pcb){ *pcb=0; return 7; }
static LRESULT in(HWND h, WPARAM f, const char *text, int step, int *calls){
 Src s={text,(int)strlen(text),step,0}; EDITSTREAM es={(DWORD_PTR)&s,0,feed};
 LRESULT r=SendMessageW(h,EM_STREAMIN,f,(LPARAM)&es); if(calls)*calls=s.calls; return r;}
int main(void){
 CHECK("riched20.dll loads",LoadLibraryA("riched20.dll")!=NULL);
 HWND top=CreateWindowExA(0,"STATIC","re",WS_OVERLAPPEDWINDOW,0,0,400,300,NULL,NULL,NULL,NULL);
 HWND re=CreateWindowExW(0,L"RichEdit20W",L"",WS_CHILD|WS_VISIBLE|WS_VSCROLL|ES_MULTILINE|ES_READONLY,5,5,300,200,top,(HMENU)1,NULL,NULL);
 CHECK("RichEdit20W created",re!=NULL);
 const char *rtf="{\\rtf1\\ansi\\ansicpg932\\deff0{\\fonttbl{\\f0\\fnil\\fcharset128 \\'82\\'6c\\'82\\'72 Gothic;}}\r\n"
  "{\\colortbl ;\\red255\\green0\\blue0;}{\\*\\generator Msftedit 5.41.21.2510;}\\viewkind4\\uc1\\pard\\lang1041\\f0\\fs20"
  "\\'8e\\'67\\'97\\'70\\'8b\\'96\\'91\\'f8\\par\r\nOK {\\b bold} \\{x\\} \\u12354?}";
 int calls=0; LRESULT n=in(re,SF_RTF,rtf,7,&calls);
 WCHAR t[256]={0}; int len=GetWindowTextW(re,t,256);
 CHECK("RTF read through several callbacks",calls>10&&n>0);
 CHECK("RTF text",len==19&&!wcscmp(t,L"\x4f7f\x7528\x8a31\x8afe\r\nOK bold {x} \x3042"));
 in(re,SF_TEXT,"plain ANSI",100,NULL);
 len=GetWindowTextW(re,t,256);
 CHECK("SF_TEXT replaces the text",len==10&&!wcscmp(t,L"plain ANSI"));
 EDITSTREAM es={0,0,broken}; SendMessageW(re,EM_STREAMIN,SF_TEXT,(LPARAM)&es);
 CHECK("callback error in dwError",es.dwError==7);
 DestroyWindow(top);return failed;}
