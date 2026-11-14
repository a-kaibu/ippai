/* libs: -loleaut32 -lole32 -luuid */
/* VARIANT interface references: VariantClear releases, VariantCopy / VariantCopyInd add one
   (clearing the destination first), SafeArray elements of VARIANTs follow the same rules. */
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <oleauto.h>
#include <stdio.h>

static LONG refs=1;
static HRESULT WINAPI qi(IUnknown *s,REFIID iid,void **out){*out=s;InterlockedIncrement(&refs);return S_OK;}
static ULONG WINAPI add(IUnknown *s){return InterlockedIncrement(&refs);}
static ULONG WINAPI rel(IUnknown *s){return InterlockedDecrement(&refs);}
static IUnknownVtbl vt={qi,add,rel};
static IUnknown obj={&vt};

int main(void){
 VARIANT a,b;VariantInit(&a);VariantInit(&b);
 V_VT(&a)=VT_UNKNOWN;V_UNKNOWN(&a)=&obj;IUnknown_AddRef(&obj);
 printf("held by a: %ld\n",refs);
 HRESULT hr=VariantCopy(&b,&a);
 printf("copy: %lx refs %ld\n",hr,refs);
 hr=VariantCopy(&b,&a);
 printf("copy over a copy: %lx refs %ld\n",hr,refs);
 hr=VariantCopy(&a,&a);
 printf("copy onto itself: %lx refs %ld\n",hr,refs);
 hr=VariantClear(&b);
 printf("clear: %lx refs %ld vt %d\n",hr,refs,V_VT(&b));
 /* by reference: the copy is of what it points at */
 IUnknown *p=&obj;VARIANT r;VariantInit(&r);V_VT(&r)=VT_UNKNOWN|VT_BYREF;V_UNKNOWNREF(&r)=&p;
 hr=VariantCopyInd(&b,&r);
 printf("copy by reference: %lx vt %d same %d refs %ld\n",hr,V_VT(&b),V_UNKNOWN(&b)==&obj,refs);
 hr=VariantClear(&r);
 printf("clear a reference: %lx refs %ld\n",hr,refs);
 VariantClear(&b);
 /* a BSTR copied over an interface lets the interface go */
 V_VT(&b)=VT_UNKNOWN;V_UNKNOWN(&b)=&obj;IUnknown_AddRef(&obj);
 VARIANT s;VariantInit(&s);V_VT(&s)=VT_BSTR;V_BSTR(&s)=SysAllocString(L"x");
 hr=VariantCopy(&b,&s);
 printf("string over an interface: %lx vt %d refs %ld\n",hr,V_VT(&b),refs);
 VariantClear(&b);VariantClear(&s);
 /* a SAFEARRAY of VARIANTs */
 SAFEARRAYBOUND bd={2,0};SAFEARRAY *sa=SafeArrayCreate(VT_VARIANT,1,&bd);
 LONG i=0;hr=SafeArrayPutElement(sa,&i,&a);
 printf("put: %lx refs %ld\n",hr,refs);
 hr=SafeArrayPutElement(sa,&i,&a);
 printf("put over it: %lx refs %ld\n",hr,refs);
 VariantInit(&b);hr=SafeArrayGetElement(sa,&i,&b);
 printf("get: %lx refs %ld\n",hr,refs);
 VariantClear(&b);
 hr=SafeArrayDestroy(sa);
 printf("destroy: %lx refs %ld\n",hr,refs);
 VariantClear(&a);
 printf("at the end: %ld\n",refs);
 return 0;
}
