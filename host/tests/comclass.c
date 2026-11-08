/* libs: -lole32 -luuid -ladvapi32 */
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <objbase.h>
#include <stdio.h>
#include <string.h>
static const CLSID class_id={0xe026b213,0x4632,0x4313,{0xa2,0x10,1,2,3,4,5,6}};
#ifdef BUILD_FIXTURE
static LONG refs=1;
static HRESULT WINAPI query(IClassFactory *self,REFIID iid,void **out){*out=NULL;if(!IsEqualIID(iid,&IID_IUnknown)&&!IsEqualIID(iid,&IID_IClassFactory))return E_NOINTERFACE;*out=self;InterlockedIncrement(&refs);return S_OK;}
static ULONG WINAPI add(IClassFactory *self){return InterlockedIncrement(&refs);}
static ULONG WINAPI release(IClassFactory *self){return InterlockedDecrement(&refs);}
static HRESULT WINAPI create(IClassFactory *self,IUnknown *outer,REFIID iid,void **out){*out=NULL;if(outer)return CLASS_E_NOAGGREGATION;return query(self,iid,out);}
static HRESULT WINAPI lock(IClassFactory *self,BOOL yes){return S_OK;}
static IClassFactoryVtbl vt={query,add,release,create,lock};static IClassFactory factory={&vt};
__declspec(dllexport) HRESULT WINAPI DllGetClassObject(REFCLSID clsid,REFIID iid,void **out){*out=NULL;if(!IsEqualCLSID(clsid,&class_id))return CLASS_E_CLASSNOTAVAILABLE;return query(&factory,iid,out);}
#else
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
int main(void){
 char dll[MAX_PATH];GetModuleFileNameA(NULL,dll,sizeof(dll));char *name=strrchr(dll,'\\');if(!name)return 1;strcpy(name+1,"comclass-fixture.dll");
 HKEY key;const char path[]="CLSID\\{e026b213-4632-4313-a210-010203040506}\\InprocServer32";
 if(RegCreateKeyExA(HKEY_CLASSES_ROOT,path,0,NULL,0,KEY_ALL_ACCESS,NULL,&key,NULL))return 1;
 RegSetValueExA(key,NULL,0,REG_SZ,(BYTE *)dll,strlen(dll)+1);RegCloseKey(key);
 CoInitialize(NULL);IClassFactory *factory=NULL;
 HRESULT hr=CoGetClassObject(&class_id,CLSCTX_INPROC_SERVER,NULL,&IID_IClassFactory,(void **)&factory);
 CHECK("load real inproc factory",SUCCEEDED(hr)&&factory);
 IUnknown *object=NULL;if(factory){hr=IClassFactory_CreateInstance(factory,NULL,&IID_IUnknown,(void **)&object);CHECK("factory executes guest code",SUCCEEDED(hr)&&object);if(object)IUnknown_Release(object);IClassFactory_Release(factory);}else return 1;
 object=NULL;hr=CoCreateInstance(&class_id,NULL,CLSCTX_INPROC_SERVER,&IID_IUnknown,(void **)&object);
 CHECK("inproc CoCreateInstance",SUCCEEDED(hr)&&object);if(object)IUnknown_Release(object);
 factory=(void *)1;hr=CoGetClassObject(&class_id,CLSCTX_INPROC_SERVER,NULL,&IID_IStream,(void **)&factory);
 CHECK("factory rejects unsupported interface",hr==E_NOINTERFACE&&factory==NULL);
 CLSID absent=class_id;absent.Data1++;factory=(void *)1;
 hr=CoGetClassObject(&absent,CLSCTX_INPROC_SERVER,NULL,&IID_IClassFactory,(void **)&factory);
 CHECK("missing class clears output",hr==REGDB_E_CLASSNOTREG&&factory==NULL);
 RegDeleteKeyA(HKEY_CLASSES_ROOT,path);CoUninitialize();return failed;
}
#endif
