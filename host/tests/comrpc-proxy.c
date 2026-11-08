/* A minimal independent MIDL -Oif metadata fixture for the test interface.
 * No server implementation or application result is contained in this DLL. */
#define CINTERFACE
#include "comrpc-fixture.h"
#include <rpcproxy.h>
#include <string.h>
static HINSTANCE module;
static unsigned char procs[3 * 42];
static unsigned short offsets[6] = {0,0,0,0,42,84};
static unsigned char types[] = {0xb4,0x83,0,0,sizeof(void *),0,0,0,0,0};
static const USER_MARSHAL_ROUTINE_QUADRUPLE users[] = {{
    (USER_MARSHAL_SIZING_ROUTINE)BSTR_UserSize,
    (USER_MARSHAL_MARSHALLING_ROUTINE)BSTR_UserMarshal,
    (USER_MARSHAL_UNMARSHALLING_ROUTINE)BSTR_UserUnmarshal,
    (USER_MARSHAL_FREEING_ROUTINE)BSTR_UserFree}};
static const MIDL_STUB_DESC desc = {.pFormatTypes=types, .aUserMarshalQuadruple=users};
static const MIDL_SERVER_INFO server = {.pStubDesc=&desc, .ProcString=procs, .FmtStringOffset=offsets};
static CInterfaceStubVtbl stub = {.header={&test_iid,&server,6,NULL}};
static CInterfaceStubVtbl *stubs[] = {&stub,NULL};
static const ProxyFileInfo info = {.pStubVtblList=stubs,.TableSize=1,.TableVersion=2};
static const ProxyFileInfo *list[] = {&info,NULL};
static void word(unsigned char *p, unsigned v) { p[0]=v; p[1]=v>>8; }
static void init(void) {
    for (int i=0;i<3;i++) {
        unsigned char *p=procs+i*42;
        p[0]=0x33; p[1]=0x6c; word(p+6,i+3);
        word(p+8,(i==2?3:4)*sizeof(void *));
        p[14]=0x44; p[15]=i==2?2:3; p[16]=8; p[17]=1;
        if (i==2) {
            word(p+24,0x2150); word(p+26,sizeof(void *)); p[28]=8;
            word(p+30,0x70); word(p+32,2*sizeof(void *)); p[34]=8;
        } else {
            word(p+24,i==1?0x8b:0x48); word(p+26,sizeof(void *)); p[28]=i==1?0:8;
            word(p+30,i==1?0x2113:0x2150); word(p+32,2*sizeof(void *)); p[34]=i==1?0:8;
            word(p+36,0x70); word(p+38,3*sizeof(void *)); p[40]=8;
        }
    }
}
__declspec(dllexport) void WINAPI GetProxyDllInfo(const ProxyFileInfo ***out, const CLSID **clsid) {
    init(); *out=list; *clsid=&test_iid;
}
__declspec(dllexport) HRESULT WINAPI DllRegisterServer(void) { init(); return NdrDllRegisterProxy(module,list,&test_iid); }
BOOL WINAPI DllMain(HINSTANCE h,DWORD reason,LPVOID unused) { if(reason==DLL_PROCESS_ATTACH)module=h;return TRUE; }
