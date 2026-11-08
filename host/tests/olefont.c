/* libs: -loleaut32 -lole32 -lgdi32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <olectl.h>
#include <stdio.h>
#include <wchar.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
int main(void){CoInitialize(NULL);FONTDESC desc={sizeof(desc),L"Arial",{.int64=120000},400,128,FALSE,FALSE,FALSE};IFont *font=NULL;
 HRESULT hr=OleCreateFontIndirect(&desc,&IID_IFont,(void **)&font);CHECK("create OLE font",SUCCEEDED(hr)&&font);if(!font)return 1;
 BSTR name=NULL;CY size;SHORT weight,charset;BOOL bold;
 CHECK("font description preserved",SUCCEEDED(IFont_get_Name(font,&name))&&!wcscmp(name,L"Arial")&&SUCCEEDED(IFont_get_Size(font,&size))&&size.int64==120000&&SUCCEEDED(IFont_get_Charset(font,&charset))&&charset==128);SysFreeString(name);
 HFONT h;LOGFONTW lf;CHECK("real GDI font",SUCCEEDED(IFont_get_hFont(font,&h))&&GetObjectW(h,sizeof(lf),&lf)==sizeof(lf)&&lf.lfHeight==-16&&lf.lfCharSet==128&&!wcscmp(lf.lfFaceName,L"Arial"));
 CHECK("font handle references",SUCCEEDED(IFont_AddRefHfont(font,h))&&SUCCEEDED(IFont_ReleaseHfont(font,h)));
 CHECK("bold weight coherence",SUCCEEDED(IFont_put_Bold(font,TRUE))&&SUCCEEDED(IFont_get_Bold(font,&bold))&&bold&&SUCCEEDED(IFont_get_Weight(font,&weight))&&weight==700);
 IFont *copy=NULL;CHECK("font clone",SUCCEEDED(IFont_Clone(font,&copy))&&copy&&IFont_IsEqual(font,copy)==S_OK);
 size.int64=180000;CHECK("independent font properties",SUCCEEDED(IFont_put_Size(copy,size))&&IFont_IsEqual(font,copy)==S_FALSE&&SUCCEEDED(IFont_get_Size(font,&size))&&size.int64==120000);
 IDispatch *dispatch=NULL;hr=IFont_QueryInterface(font,&IID_IFontDisp,(void **)&dispatch);CHECK("font dispatch interface",SUCCEEDED(hr)&&dispatch);
 if(dispatch){LPOLESTR property=L"Name";DISPID id=-1;VARIANT value;VariantInit(&value);DISPPARAMS params={0};
 CHECK("font automation property",SUCCEEDED(IDispatch_GetIDsOfNames(dispatch,&IID_NULL,&property,1,0,&id))&&id==0&&SUCCEEDED(IDispatch_Invoke(dispatch,id,&IID_NULL,0,DISPATCH_PROPERTYGET,&params,&value,NULL,NULL))&&V_VT(&value)==VT_BSTR&&!wcscmp(V_BSTR(&value),L"Arial"));VariantClear(&value);
 IUnknown *identity=NULL;hr=IDispatch_QueryInterface(dispatch,&IID_IUnknown,(void **)&identity);CHECK("font COM identity",SUCCEEDED(hr)&&(void *)identity==(void *)font);if(identity)IUnknown_Release(identity);IDispatch_Release(dispatch);}
 void *unsupported=(void *)1;hr=IFont_QueryInterface(font,&IID_IStream,&unsupported);CHECK("font rejects unknown interface",hr==E_NOINTERFACE&&!unsupported);
 if(copy)IFont_Release(copy);IFont_Release(font);CoUninitialize();return failed;}
