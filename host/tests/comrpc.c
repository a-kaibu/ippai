/* libs: -ladvapi32 -lole32 -loleaut32 -luuid
 * Real STA class factory and object in an SCM-launched process. */
#define CINTERFACE
#include "comrpc-fixture.h"
#include <stdio.h>
#include <string.h>
static int failed;
#define CHECK(label, expr) do { int ok=!!(expr); printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok; } while(0)
static HRESULT WINAPI object_qi(Test *t,REFIID iid,void **out) {
    if(!IsEqualIID(iid,&IID_IUnknown)&&!IsEqualIID(iid,&test_iid)){*out=NULL;return E_NOINTERFACE;}
    *out=t;InterlockedIncrement(&t->refs);return S_OK;
}
static ULONG WINAPI object_add(Test *t) { return InterlockedIncrement(&t->refs); }
static ULONG WINAPI object_release(Test *t) { LONG n=InterlockedDecrement(&t->refs);if(!n)HeapFree(GetProcessHeap(),0,t);return n; }
static HRESULT WINAPI get_value(Test *t,LONG in,LONG *out) { *out=in+GetCurrentProcessId();return S_OK; }
static HRESULT WINAPI echo(Test *t,BSTR in,BSTR *out) { *out=in?SysAllocStringByteLen((char *)in,SysStringByteLen(in)):NULL;return S_OK; }
static HRESULT WINAPI fail_call(Test *t,LONG *out) { *out=77;return E_ACCESSDENIED; }
static const TestVtbl object_vt={object_qi,object_add,object_release,get_value,echo,fail_call};
static HRESULT WINAPI factory_qi(IClassFactory *t,REFIID iid,void **out) {
    if(!IsEqualIID(iid,&IID_IUnknown)&&!IsEqualIID(iid,&IID_IClassFactory)){*out=NULL;return E_NOINTERFACE;}
    *out=t;return S_OK;
}
static ULONG WINAPI factory_add(IClassFactory *t){return 2;}
static ULONG WINAPI factory_release(IClassFactory *t){return 1;}
static HRESULT WINAPI factory_create(IClassFactory *t,IUnknown *outer,REFIID iid,void **out) {
    if(outer)return CLASS_E_NOAGGREGATION;
    Test *object=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,sizeof(*object));
    object->lpVtbl=&object_vt;object->refs=1;
    HRESULT hr=object_qi(object,iid,out);object_release(object);return hr;
}
static HRESULT WINAPI factory_lock(IClassFactory *t,BOOL lock){return S_OK;}
static IClassFactoryVtbl factory_vt={factory_qi,factory_add,factory_release,factory_create,factory_lock};
static IClassFactory factory={&factory_vt};
static SERVICE_STATUS_HANDLE status_handle;
static DWORD server_thread;
static void report(DWORD state) {
    SERVICE_STATUS s={SERVICE_WIN32_OWN_PROCESS,state,state==SERVICE_RUNNING?SERVICE_ACCEPT_STOP:0,0,0,0,0};
    SetServiceStatus(status_handle,&s);
}
static void WINAPI control(DWORD code){if(code==SERVICE_CONTROL_STOP){report(SERVICE_STOP_PENDING);PostThreadMessageA(server_thread,WM_QUIT,0,0);}}
static void WINAPI service_main(DWORD argc,LPSTR *argv) {
    status_handle=RegisterServiceCtrlHandlerA("sake-com-test",control);if(!status_handle)return;
    report(SERVICE_START_PENDING);server_thread=GetCurrentThreadId();
    CoInitialize(NULL);DWORD cookie=0;
    HRESULT hr=CoRegisterClassObject(&test_class,(IUnknown *)&factory,CLSCTX_LOCAL_SERVER,REGCLS_MULTIPLEUSE,&cookie);
    if(FAILED(hr)){report(SERVICE_STOPPED);return;}
    report(SERVICE_RUNNING);MSG msg;
    while(GetMessageA(&msg,NULL,0,0)>0)DispatchMessageA(&msg);
    CoRevokeClassObject(cookie);CoUninitialize();report(SERVICE_STOPPED);
}
static void set(HKEY root,const char *key,const char *name,const char *value) {
    HKEY h;if(RegCreateKeyExA(root,key,0,NULL,0,KEY_ALL_ACCESS,NULL,&h,NULL))return;
    RegSetValueExA(h,name,0,REG_SZ,(BYTE *)value,strlen(value)+1);RegCloseKey(h);
}
struct Parallel { Test *object; LONG pid; LONG input; int ok; };
static DWORD WINAPI parallel(LPVOID argument) {
    struct Parallel *p=argument;CoInitializeEx(NULL,COINIT_MULTITHREADED);p->ok=1;
    for(int i=0;i<4;i++){LONG value=0;HRESULT hr=p->object->lpVtbl->GetValue(p->object,p->input+i,&value);
        if(FAILED(hr)||value!=p->pid+p->input+i)p->ok=0;}
    CoUninitialize();return 0;
}
int main(int argc,char **argv) {
    if(argc>1&&!strcmp(argv[1],"--service")) {
        SERVICE_TABLE_ENTRYA entries[]={{"sake-com-test",service_main},{NULL,NULL}};
        return StartServiceCtrlDispatcherA(entries)?0:1;
    }
    char image[MAX_PATH],command[MAX_PATH+32],dll[MAX_PATH];GetModuleFileNameA(NULL,image,sizeof(image));
    strcpy(dll,image);char *name=strrchr(dll,'\\');if(!name)return 1;strcpy(name+1,"comrpc-proxy.dll");
    HMODULE proxy=LoadLibraryA(dll);
    HRESULT (WINAPI *register_proxy)(void)=proxy?(void *)GetProcAddress(proxy,"DllRegisterServer"):NULL;
    CHECK("register proxy metadata",register_proxy&&SUCCEEDED(register_proxy()));
    SC_HANDLE manager=OpenSCManagerA(NULL,NULL,SC_MANAGER_ALL_ACCESS);
    SC_HANDLE old=OpenServiceA(manager,"sake-com-test",SERVICE_ALL_ACCESS);if(old){DeleteService(old);CloseServiceHandle(old);}
    snprintf(command,sizeof(command),"\"%s\" --service",image);
    SC_HANDLE service=CreateServiceA(manager,"sake-com-test","COM test",SERVICE_ALL_ACCESS,SERVICE_WIN32_OWN_PROCESS,
        SERVICE_DEMAND_START,SERVICE_ERROR_NORMAL,command,NULL,NULL,NULL,NULL,NULL);
    CHECK("create service",service!=NULL);if(!service)return 1;
    set(HKEY_CLASSES_ROOT,"CLSID\\" CLASS_TEXT,"AppID",CLASS_TEXT);
    set(HKEY_CLASSES_ROOT,"AppID\\" CLASS_TEXT,"LocalService","sake-com-test");
    CoInitializeEx(NULL,COINIT_MULTITHREADED);Test *object=NULL;
    HRESULT hr=CoCreateInstance(&test_class,NULL,CLSCTX_LOCAL_SERVER,&test_iid,(void **)&object);
    CHECK("activate service",SUCCEEDED(hr)&&object);if(!object){DeleteService(service);CloseServiceHandle(service);return 1;}
    LONG value=0;hr=object->lpVtbl->GetValue(object,10,&value);
    CHECK("remote scalar call",SUCCEEDED(hr)&&value>10&&value!=10+(LONG)GetCurrentProcessId());
    struct Parallel work[2]={{object,value-10,100,0},{object,value-10,200,0}};
    HANDLE threads[2]={CreateThread(NULL,0,parallel,&work[0],0,NULL),CreateThread(NULL,0,parallel,&work[1],0,NULL)};
    DWORD wait=WaitForMultipleObjects(2,threads,TRUE,10000);
    CHECK("parallel responses",wait==WAIT_OBJECT_0&&work[0].ok&&work[1].ok);CloseHandle(threads[0]);CloseHandle(threads[1]);
    BYTE binary[]={0x61,0,0,0,0xff,0x22,0x73};
    BSTR in=SysAllocStringByteLen((char *)binary,sizeof(binary)),out=NULL;
    hr=object->lpVtbl->Echo(object,in,&out);
    CHECK("binary BSTR roundtrip",SUCCEEDED(hr)&&out&&SysStringByteLen(out)==sizeof(binary)&&!memcmp(out,binary,sizeof(binary)));
    SysFreeString(in);SysFreeString(out);out=(BSTR)1;
    CHECK("null BSTR",SUCCEEDED(object->lpVtbl->Echo(object,NULL,&out))&&out==NULL);
    value=0;hr=object->lpVtbl->Fail(object,&value);
    CHECK("remote error preserved",hr==E_ACCESSDENIED&&value==77);
    IUnknown *unknown=NULL;
    hr=object->lpVtbl->QueryInterface(object,&IID_IUnknown,(void **)&unknown);
    CHECK("IUnknown identity",SUCCEEDED(hr)&&(void *)unknown==(void *)object);if(unknown)unknown->lpVtbl->Release(unknown);
    object->lpVtbl->Release(object);
    /* (the object can be served before the service reports RUNNING: until then it takes no STOP,
     * ERROR_SERVICE_CANNOT_ACCEPT_CTRL, on Windows as on sake) */
    SERVICE_STATUS s;for(int i=0;i<500&&QueryServiceStatus(service,&s)&&s.dwCurrentState!=SERVICE_RUNNING;i++)Sleep(10);
    CHECK("stop server",ControlService(service,SERVICE_CONTROL_STOP,&s));
    DeleteService(service);CloseServiceHandle(service);CloseServiceHandle(manager);CoUninitialize();
    return failed;
}
