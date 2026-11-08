/* libs: -ladvapi32
 * Actual RSA operations and a second process reopening the same key container. */
#include <windows.h>
#include <wincrypt.h>
#include <stdio.h>
#include <string.h>
static const char container[]="sake-capi-regression";
static const char provider_name[]="Microsoft Enhanced Cryptographic Provider v1.0";
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
static int reopen(void) {
    HCRYPTPROV p=0;HCRYPTKEY key=0;BYTE expected[1024],actual[1024];DWORD n=sizeof(actual),read=0;
    if(!CryptAcquireContextA(&p,container,provider_name,PROV_RSA_FULL,CRYPT_MACHINE_KEYSET))return 1;
    if(!CryptGetUserKey(p,AT_SIGNATURE,&key))return 2;
    HANDLE f=CreateFileA("C:\\capiprovider-public.blob",GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
    if(f==INVALID_HANDLE_VALUE||!ReadFile(f,expected,sizeof(expected),&read,NULL))return 3;
    CloseHandle(f);
    if(!CryptExportKey(key,0,PUBLICKEYBLOB,0,actual,&n)||n!=read||memcmp(actual,expected,n))return 4;
    CryptDestroyKey(key);CryptReleaseContext(p,0);return 0;
}
int main(int argc,char **argv) {
    if(argc>1&&!strcmp(argv[1],"--open"))return reopen();
    HCRYPTPROV p=0,other=0;HCRYPTKEY key=0,absent=0;
    CryptAcquireContextA(NULL,container,provider_name,PROV_RSA_FULL,CRYPT_DELETEKEYSET|CRYPT_MACHINE_KEYSET);
    BOOL r=CryptAcquireContextA(&p,container,provider_name,PROV_RSA_FULL,CRYPT_MACHINE_KEYSET);
    CHECK("missing container",!r&&GetLastError()==NTE_BAD_KEYSET);
    CHECK("create container",CryptAcquireContextA(&p,container,provider_name,PROV_RSA_FULL,CRYPT_NEWKEYSET|CRYPT_MACHINE_KEYSET));
    r=CryptAcquireContextA(&other,container,provider_name,PROV_RSA_FULL,CRYPT_NEWKEYSET|CRYPT_MACHINE_KEYSET);
    CHECK("reject duplicate container",!r&&GetLastError()==NTE_EXISTS);
    r=CryptGetUserKey(p,AT_SIGNATURE,&absent);CHECK("missing key",!r&&GetLastError()==NTE_NO_KEY);
    CHECK("generate RSA",CryptGenKey(p,AT_SIGNATURE,512u<<16,&key));
    if(!key)return 1;
    BYTE public_key[1024];DWORD n=sizeof(public_key);
    CHECK("export real public key",CryptExportKey(key,0,PUBLICKEYBLOB,0,public_key,&n)&&n==84);
    DWORD private_size=0;r=CryptExportKey(key,0,PRIVATEKEYBLOB,0,NULL,&private_size);
    CHECK("private export permission",!r);
    HANDLE f=CreateFileA("C:\\capiprovider-public.blob",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);DWORD written;
    if(f==INVALID_HANDLE_VALUE||!WriteFile(f,public_key,n,&written,NULL))return 1;CloseHandle(f);
    HCRYPTHASH hash=0;BYTE message[]={1,2,3,4};BYTE signature[64];DWORD signature_size=sizeof(signature);
    CHECK("hash",CryptCreateHash(p,CALG_SHA1,0,0,&hash)&&CryptHashData(hash,message,sizeof(message),0));
    CHECK("sign",CryptSignHashA(hash,AT_SIGNATURE,NULL,0,signature,&signature_size)&&signature_size==64);
    HCRYPTKEY public_handle=0;CHECK("import public",CryptImportKey(p,public_key,n,0,0,&public_handle));
    CHECK("verify actual signature",CryptVerifySignatureA(hash,signature,signature_size,public_handle,NULL,0));
    signature[0]^=1;r=CryptVerifySignatureA(hash,signature,signature_size,public_handle,NULL,0);
    CHECK("reject changed signature",!r&&GetLastError()==NTE_BAD_SIGNATURE);
    CryptDestroyHash(hash);CryptDestroyKey(key);CryptDestroyKey(public_handle);CryptReleaseContext(p,0);
    char image[MAX_PATH],command[MAX_PATH+32];GetModuleFileNameA(NULL,image,sizeof(image));snprintf(command,sizeof(command),"\"%s\" --open",image);
    STARTUPINFOA si={sizeof(si)};PROCESS_INFORMATION pi={0};BOOL created=CreateProcessA(NULL,command,NULL,NULL,FALSE,0,NULL,NULL,&si,&pi);
    DWORD code=99;if(created&&WaitForSingleObject(pi.hProcess,10000)==WAIT_OBJECT_0)GetExitCodeProcess(pi.hProcess,&code);
    CHECK("key survives process",created&&code==0);if(created){CloseHandle(pi.hThread);CloseHandle(pi.hProcess);}
    CHECK("delete container",CryptAcquireContextA(NULL,container,provider_name,PROV_RSA_FULL,CRYPT_DELETEKEYSET|CRYPT_MACHINE_KEYSET));
    r=CryptAcquireContextA(&p,container,provider_name,PROV_RSA_FULL,CRYPT_MACHINE_KEYSET);
    CHECK("deleted container missing",!r&&GetLastError()==NTE_BAD_KEYSET);
    DeleteFileA("C:\\capiprovider-public.blob");return failed;
}
