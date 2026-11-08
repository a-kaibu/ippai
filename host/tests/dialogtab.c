/* libs: -luser32 */
#include <windows.h>
#include <stdio.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
int main(void){HWND d=CreateWindowExA(0,"STATIC","Tab order",WS_OVERLAPPEDWINDOW|WS_VISIBLE,0,0,400,300,NULL,NULL,NULL,NULL);
 HWND a=CreateWindowExA(0,"EDIT","",WS_CHILD|WS_VISIBLE|WS_TABSTOP,5,5,100,20,d,(HMENU)1,NULL,NULL);
 HWND hidden=CreateWindowExA(0,"EDIT","",WS_CHILD|WS_TABSTOP,5,30,100,20,d,(HMENU)2,NULL,NULL);
 HWND disabled=CreateWindowExA(0,"EDIT","",WS_CHILD|WS_VISIBLE|WS_TABSTOP|WS_DISABLED,5,55,100,20,d,(HMENU)3,NULL,NULL);
 HWND b=CreateWindowExA(0,"EDIT","",WS_CHILD|WS_VISIBLE|WS_TABSTOP,5,80,100,20,d,(HMENU)4,NULL,NULL);
 CHECK("initial dialog tab item",GetNextDlgTabItem(d,NULL,FALSE)==a&&GetNextDlgTabItem(d,NULL,TRUE)==b);
 CHECK("skip hidden and disabled controls",GetNextDlgTabItem(d,a,FALSE)==b&&GetNextDlgTabItem(d,b,TRUE)==a);
 CHECK("dialog tab wrap",GetNextDlgTabItem(d,b,FALSE)==a&&GetNextDlgTabItem(d,a,TRUE)==b);
 SetFocus(a);MSG msg={a,WM_KEYDOWN,VK_TAB,0,0,{0,0}};
 CHECK("Tab moves keyboard focus",IsDialogMessageA(d,&msg)&&GetFocus()==b);
 ShowWindow(hidden,SW_SHOW);EnableWindow(disabled,TRUE);
 CHECK("tab order reflects state changes",GetNextDlgTabItem(d,a,FALSE)==hidden&&GetNextDlgTabItem(d,hidden,FALSE)==disabled);
 DestroyWindow(d);return failed;}
