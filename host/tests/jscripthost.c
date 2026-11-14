/* libs: -lole32 -loleaut32 -luuid */
/* An Active Scripting host of JScript: host objects that come back to the host (an expression's
   value, a script function's result), references the engine gives back, a host error the
   script catches, script <-> host recursion that ends in a catchable error, a syntax error
   reported to the site, and everything released at Close (review b07). */
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <objbase.h>
#include <activscp.h>
#include <stdio.h>
#include <string.h>

static const IID iid_parse32={0xbb1a2ae2,0xa4f9,0x11cf,{0x8f,0x20,0x00,0x80,0x5f,0x2c,0xd0,0x64}};
static const IID iid_site={0xdb01a1e3,0xa42b,0x11cf,{0x8f,0x20,0x00,0x80,0x5f,0x2c,0xd0,0x64}};

/* host objects: the root ("root" named item) and the children it makes */
typedef struct { IDispatchVtbl *vt; LONG refs; int kind; } Obj;
static LONG alive;
static IDispatchVtbl obj_vt;
static Obj root={&obj_vt,1,0};

static Obj *make_child(void){Obj *o=(Obj *)HeapAlloc(GetProcessHeap(),0,sizeof(Obj));o->vt=&obj_vt;o->refs=1;o->kind=1;InterlockedIncrement(&alive);return o;}
static HRESULT WINAPI o_qi(IDispatch *s,REFIID iid,void **out){
 if(IsEqualIID(iid,&IID_IUnknown)||IsEqualIID(iid,&IID_IDispatch)){*out=s;IDispatch_AddRef(s);return S_OK;}
 *out=NULL;return E_NOINTERFACE;}
static ULONG WINAPI o_add(IDispatch *s){return InterlockedIncrement(&((Obj *)s)->refs);}
static ULONG WINAPI o_rel(IDispatch *s){Obj *o=(Obj *)s;LONG r=InterlockedDecrement(&o->refs);if(!r&&o->kind==1){InterlockedDecrement(&alive);HeapFree(GetProcessHeap(),0,o);}return r;}
static HRESULT WINAPI o_count(IDispatch *s,UINT *n){*n=0;return S_OK;}
static HRESULT WINAPI o_info(IDispatch *s,UINT i,LCID l,ITypeInfo **t){*t=NULL;return E_NOTIMPL;}
static const WCHAR *names[]={L"",L"getChild",L"self",L"call",L"fail",L"alive"};
static HRESULT WINAPI o_ids(IDispatch *s,REFIID r,LPOLESTR *n,UINT c,LCID l,DISPID *ids){
 for(int i=1;i<6;i++)if(!wcscmp(n[0],names[i])){ids[0]=i;return S_OK;}
 ids[0]=DISPID_UNKNOWN;return DISP_E_UNKNOWNNAME;}
static HRESULT WINAPI o_invoke(IDispatch *s,DISPID id,REFIID r,LCID l,WORD flags,DISPPARAMS *dp,VARIANT *res,EXCEPINFO *ei,UINT *err){
 if(res)VariantInit(res);
 /* (methods are only called: a plain property get of one is not a member) */
 if(id>=1&&id<=4&&!(flags&DISPATCH_METHOD))return DISP_E_MEMBERNOTFOUND;
 switch(id){
 case 1: if(res){V_VT(res)=VT_DISPATCH;V_DISPATCH(res)=(IDispatch *)make_child();}return S_OK;
 case 2: if(res){V_VT(res)=VT_DISPATCH;V_DISPATCH(res)=s;IDispatch_AddRef(s);}return S_OK;
 case 3: { /* call(fn, n): fn(n) */
  if(!dp||dp->cArgs<2||V_VT(&dp->rgvarg[1])!=VT_DISPATCH)return DISP_E_TYPEMISMATCH;
  IDispatch *fn=V_DISPATCH(&dp->rgvarg[1]);
  DISPPARAMS p={&dp->rgvarg[0],NULL,1,0};
  return IDispatch_Invoke(fn,DISPID_VALUE,&IID_NULL,0,DISPATCH_METHOD,&p,res,ei,err);
 }
 case 4: if(ei){memset(ei,0,sizeof(*ei));ei->scode=E_FAIL;ei->bstrSource=SysAllocString(L"host");ei->bstrDescription=SysAllocString(L"boom");}return DISP_E_EXCEPTION;
 case 5: if(res){V_VT(res)=VT_I4;V_I4(res)=alive;}return S_OK;
 }
 return DISP_E_MEMBERNOTFOUND;}
static IDispatchVtbl obj_vt={o_qi,o_add,o_rel,o_count,o_info,o_ids,o_invoke};

/* the site */
static int errors,error_scode,error_desc;
static HRESULT WINAPI s_qi(IActiveScriptSite *s,REFIID iid,void **out){
 if(IsEqualIID(iid,&IID_IUnknown)||IsEqualIID(iid,&iid_site)){*out=s;return S_OK;}
 *out=NULL;return E_NOINTERFACE;}
static ULONG WINAPI s_add(IActiveScriptSite *s){return 2;}
static ULONG WINAPI s_rel(IActiveScriptSite *s){return 1;}
static HRESULT WINAPI s_lcid(IActiveScriptSite *s,LCID *l){*l=0x409;return S_OK;}
static HRESULT WINAPI s_item(IActiveScriptSite *s,LPCOLESTR name,DWORD mask,IUnknown **unk,ITypeInfo **ti){
 if(ti)*ti=NULL;
 if(wcscmp(name,L"root"))return TYPE_E_ELEMENTNOTFOUND;
 if(mask&SCRIPTINFO_IUNKNOWN){*unk=(IUnknown *)&root;IUnknown_AddRef(*unk);}
 return S_OK;}
static HRESULT WINAPI s_ver(IActiveScriptSite *s,BSTR *v){*v=NULL;return E_NOTIMPL;}
static HRESULT WINAPI s_term(IActiveScriptSite *s,const VARIANT *v,const EXCEPINFO *e){return S_OK;}
static HRESULT WINAPI s_state(IActiveScriptSite *s,SCRIPTSTATE st){return S_OK;}
static HRESULT WINAPI s_error(IActiveScriptSite *s,IActiveScriptError *e){
 EXCEPINFO ei;memset(&ei,0,sizeof(ei));
 errors++;
 if(SUCCEEDED(IActiveScriptError_GetExceptionInfo(e,&ei))){error_scode=ei.scode!=0;error_desc=ei.bstrDescription&&ei.bstrDescription[0];}
 SysFreeString(ei.bstrSource);SysFreeString(ei.bstrDescription);SysFreeString(ei.bstrHelpFile);
 return S_OK;}
static HRESULT WINAPI s_enter(IActiveScriptSite *s){return S_OK;}
static HRESULT WINAPI s_leave(IActiveScriptSite *s){return S_OK;}
static IActiveScriptSiteVtbl site_vt={s_qi,s_add,s_rel,s_lcid,s_item,s_ver,s_term,s_state,s_error,s_enter,s_leave};
static IActiveScriptSite site={&site_vt};

static IActiveScriptParse *parse;
static HRESULT run(const WCHAR *code,DWORD flags,VARIANT *v){
 EXCEPINFO ei;memset(&ei,0,sizeof(ei));
 HRESULT hr=parse->lpVtbl->ParseScriptText(parse,code,NULL,NULL,NULL,0,0,flags,v,&ei);
 SysFreeString(ei.bstrSource);SysFreeString(ei.bstrDescription);SysFreeString(ei.bstrHelpFile);
 return hr;}
static void print_str(const char *label,const WCHAR *code){
 VARIANT v;VariantInit(&v);HRESULT hr=run(code,SCRIPTTEXT_ISEXPRESSION,&v);
 printf("%s: hr %d vt %d %ls\n",label,SUCCEEDED(hr),V_VT(&v),V_VT(&v)==VT_BSTR?V_BSTR(&v):L"");
 VariantClear(&v);}

int main(void){
 CoInitialize(NULL);
 CLSID clsid;IActiveScript *eng=NULL;
 if(FAILED(CLSIDFromProgID(L"JScript",&clsid))||FAILED(CoCreateInstance(&clsid,NULL,CLSCTX_INPROC_SERVER,&IID_IActiveScript,(void **)&eng))){printf("no engine\n");return 1;}
 if(FAILED(IActiveScript_QueryInterface(eng,&iid_parse32,(void **)&parse))){printf("no parse\n");return 1;}
 parse->lpVtbl->InitNew(parse);
 IActiveScript_SetScriptSite(eng,&site);
 IActiveScript_AddNamedItem(eng,L"root",SCRIPTITEM_ISVISIBLE);
 IActiveScript_SetScriptState(eng,SCRIPTSTATE_STARTED);

 /* an expression whose value is a new host object */
 VARIANT v;VariantInit(&v);
 HRESULT hr=run(L"root.getChild()",SCRIPTTEXT_ISEXPRESSION,&v);
 printf("expression child: hr %d vt %d alive %ld\n",SUCCEEDED(hr),V_VT(&v),alive);
 VariantClear(&v);

 /* a script function returning the host's own object */
 LONG before=root.refs;
 run(L"function f() { return root; }",0,NULL);
 IDispatch *g=NULL;IActiveScript_GetScriptDispatch(eng,NULL,&g);
 DISPID id=0;LPOLESTR fname=L"f";
 if(g&&SUCCEEDED(IDispatch_GetIDsOfNames(g,&IID_NULL,&fname,1,0,&id))){
  DISPPARAMS none={NULL,NULL,0,0};VariantInit(&v);
  hr=IDispatch_Invoke(g,id,&IID_NULL,0,DISPATCH_METHOD,&none,&v,NULL,NULL);
  printf("function returns root: hr %d vt %d same %d\n",SUCCEEDED(hr),V_VT(&v),V_VT(&v)==VT_DISPATCH&&V_DISPATCH(&v)==(IDispatch *)&root);
  VariantClear(&v);
 }else printf("function returns root: no f\n");
 if(g)IDispatch_Release(g);
 printf("root references back: %d\n",root.refs==before);

 /* the same object handed out 1000 times: its references don't pile up */
 before=root.refs;
 run(L"for (var i = 0; i < 1000; i++) root.self(); CollectGarbage();",0,NULL);
 printf("self 1000 times, root references grew by at most 2: %d\n",root.refs-before<=2);
 run(L"for (var i = 0; i < 200; i++) root.getChild();",0,NULL);

 /* a host error the script catches */
 run(L"var msg = 'none'; try { root.fail(); } catch (e) { msg = e.message + ' ' + (e.number == -2147467259); }",0,NULL);
 print_str("host error caught",L"msg");

 /* script -> host -> script ... ends in an error the script catches */
 run(L"function r(n) { return root.call(r, n + 1); } var deep = 'returned'; try { r(0); } catch (e) { deep = 'caught ' + (e.number == -2146828260); }",0,NULL);
 print_str("deep recursion",L"deep");
 print_str("engine still runs",L"'a' + 'b'");

 /* a syntax error goes to the site */
 hr=run(L"var = ;",0,NULL);
 printf("syntax error: failed %d reported %d scode %d description %d\n",FAILED(hr),errors,error_scode,error_desc);

 IActiveScript_Close(eng);
 parse->lpVtbl->Release(parse);
 IActiveScript_Release(eng);
 printf("after close: root references %ld children alive %ld\n",root.refs,alive);
 CoUninitialize();
 return 0;
}
