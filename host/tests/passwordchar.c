/* libs: -luser32 */
/* ES_PASSWORD edit controls: the character drawn for each one typed (EM_GET/SETPASSWORDCHAR);
 * the text itself stays as typed. */
#include <windows.h>
#include <stdio.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
int main(void){
 HWND top=CreateWindowExA(0,"STATIC","pw",WS_OVERLAPPEDWINDOW,0,0,300,200,NULL,NULL,NULL,NULL);
 HWND pw=CreateWindowExA(0,"EDIT","secret",WS_CHILD|WS_VISIBLE|ES_PASSWORD,5,5,100,20,top,(HMENU)1,NULL,NULL);
 HWND plain=CreateWindowExA(0,"EDIT","plain",WS_CHILD|WS_VISIBLE,5,30,100,20,top,(HMENU)2,NULL,NULL);
 char t[32];
 CHECK("ES_PASSWORD default character",SendMessageA(pw,EM_GETPASSWORDCHAR,0,0)=='*');
 CHECK("plain edit has none",SendMessageA(plain,EM_GETPASSWORDCHAR,0,0)==0);
 CHECK("text stays as typed",GetWindowTextA(pw,t,sizeof t)==6&&!strcmp(t,"secret"));
 SendMessageA(pw,EM_SETPASSWORDCHAR,'#',0);
 CHECK("set character",SendMessageA(pw,EM_GETPASSWORDCHAR,0,0)=='#');
 SendMessageA(pw,EM_SETPASSWORDCHAR,0,0);
 CHECK("zero removes ES_PASSWORD",SendMessageA(pw,EM_GETPASSWORDCHAR,0,0)==0&&!(GetWindowLongA(pw,GWL_STYLE)&ES_PASSWORD));
 SendMessageA(plain,EM_SETPASSWORDCHAR,'x',0);
 CHECK("nonzero adds ES_PASSWORD",SendMessageA(plain,EM_GETPASSWORDCHAR,0,0)=='x'&&(GetWindowLongA(plain,GWL_STYLE)&ES_PASSWORD));
 DestroyWindow(top);return failed;}
