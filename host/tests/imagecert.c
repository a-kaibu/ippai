/* libs: -limagehlp
 * Certificate-table metadata fixtures, deliberately not authentic signatures. */
#include <windows.h>
#include <imagehlp.h>
#include <stdio.h>
#include <string.h>
static int failed;
#define CHECK(label,expr) do{int ok=!!(expr);printf("%s: %s\n",label,ok?"ok":"FAIL");failed|=!ok;}while(0)
static BYTE stream[4096];static DWORD used,calls,stop_at;
static BOOL CALLBACK digest(DIGEST_HANDLE handle,PBYTE bytes,DWORD size) {
    if(handle!=(DIGEST_HANDLE)(ULONG_PTR)0x1234||size>sizeof(stream)-used)return FALSE;
    calls++;if(stop_at&&calls==stop_at){SetLastError(ERROR_CANCELLED);return FALSE;}
    memcpy(stream+used,bytes,size);used+=size;return TRUE;
}
static void test_digest(void) {
    BYTE image[1024]={0},expected[1024]={0};DWORD n;
    IMAGE_DOS_HEADER *dos=(void *)image;IMAGE_NT_HEADERS32 *nt=(void *)(image+128);
    dos->e_magic=IMAGE_DOS_SIGNATURE;dos->e_lfanew=128;nt->Signature=IMAGE_NT_SIGNATURE;
    nt->FileHeader.NumberOfSections=3;nt->FileHeader.SizeOfOptionalHeader=sizeof(IMAGE_OPTIONAL_HEADER32);
    nt->OptionalHeader.Magic=IMAGE_NT_OPTIONAL_HDR32_MAGIC;nt->OptionalHeader.NumberOfRvaAndSizes=16;
    nt->OptionalHeader.SizeOfInitializedData=77;nt->OptionalHeader.SizeOfImage=0x4000;nt->OptionalHeader.CheckSum=0xfeed;
    nt->OptionalHeader.DataDirectory[4].VirtualAddress=1000;nt->OptionalHeader.DataDirectory[4].Size=24;
    IMAGE_SECTION_HEADER *sh=IMAGE_FIRST_SECTION(nt);
    memcpy(sh[0].Name,".data",5);sh[0].PointerToRawData=800;sh[0].SizeOfRawData=4;
    memcpy(sh[1].Name,".text",5);sh[1].PointerToRawData=700;sh[1].SizeOfRawData=4;sh[1].Characteristics=IMAGE_SCN_CNT_CODE;
    memcpy(sh[2].Name,".rsrc",5);sh[2].PointerToRawData=900;sh[2].SizeOfRawData=4;
    memcpy(image+700,"CODE",4);memcpy(image+800,"DATA",4);memcpy(image+900,"RSRC",4);
    DWORD head=128+sizeof(*nt)+3*sizeof(*sh);memcpy(expected,image,head);
    IMAGE_NT_HEADERS32 *en=(void *)(expected+128);en->OptionalHeader.CheckSum=0;
    memset(&en->OptionalHeader.DataDirectory[4],0,sizeof(IMAGE_DATA_DIRECTORY));
    memcpy(expected+head,"CODEDATARSRC",12);
    HANDLE f=CreateFileA("imagecert-digest.bin",GENERIC_READ|GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(f==INVALID_HANDLE_VALUE){failed=1;return;}WriteFile(f,image,sizeof(image),&n,NULL);SetFilePointer(f,19,NULL,FILE_BEGIN);
    used=calls=stop_at=0;
    CHECK("digest selected bytes and order",ImageGetDigestStream(f,6,digest,(DIGEST_HANDLE)(ULONG_PTR)0x1234)&&used==head+12&&!memcmp(stream,expected,used));
    CHECK("digest preserves cursor",SetFilePointer(f,0,NULL,FILE_CURRENT)==19);
    used=calls=0;stop_at=2;
    CHECK("digest callback cancellation",!ImageGetDigestStream(f,6,digest,(DIGEST_HANDLE)(ULONG_PTR)0x1234)&&calls==2&&GetLastError()==ERROR_CANCELLED);
    used=calls=stop_at=0;
    CHECK("unsupported digest fails closed",!ImageGetDigestStream(f,0,digest,(DIGEST_HANDLE)(ULONG_PTR)0x1234)&&GetLastError()==ERROR_NOT_SUPPORTED&&calls==0);
    sh[1].SizeOfRawData=0xffffffff;SetFilePointer(f,0,NULL,FILE_BEGIN);WriteFile(f,image,sizeof(image),&n,NULL);
    CHECK("reject out of bounds digest",!ImageGetDigestStream(f,6,digest,(DIGEST_HANDLE)(ULONG_PTR)0x1234)&&calls==0);
    CloseHandle(f);DeleteFileA("imagecert-digest.bin");
}
int main(void) {
    BYTE data[536]={0};IMAGE_DOS_HEADER *dos=(void *)data;IMAGE_NT_HEADERS32 *nt=(void *)(data+128);
    dos->e_magic=IMAGE_DOS_SIGNATURE;dos->e_lfanew=128;nt->Signature=IMAGE_NT_SIGNATURE;
    nt->FileHeader.SizeOfOptionalHeader=sizeof(IMAGE_OPTIONAL_HEADER32);nt->OptionalHeader.Magic=IMAGE_NT_OPTIONAL_HDR32_MAGIC;
    nt->OptionalHeader.NumberOfRvaAndSizes=16;
    nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_SECURITY].VirtualAddress=512;
    nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_SECURITY].Size=24;
    WIN_CERTIFICATE *first=(void *)(data+512),*second=(void *)(data+528);
    first->dwLength=12;first->wRevision=WIN_CERT_REVISION_2_0;first->wCertificateType=WIN_CERT_TYPE_PKCS_SIGNED_DATA;
    memcpy(first->bCertificate,"data",4);second->dwLength=8;second->wRevision=WIN_CERT_REVISION_2_0;second->wCertificateType=WIN_CERT_TYPE_X509;
    HANDLE f=CreateFileA("imagecert-fixture.bin",GENERIC_READ|GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);DWORD n;
    if(f==INVALID_HANDLE_VALUE||!WriteFile(f,data,sizeof(data),&n,NULL))return 1;
    SetFilePointer(f,17,NULL,FILE_BEGIN);
    WIN_CERTIFICATE header={0};CHECK("certificate header",ImageGetCertificateHeader(f,0,&header)&&header.dwLength==12&&header.wCertificateType==2);
    CHECK("preserve file position",SetFilePointer(f,0,NULL,FILE_CURRENT)==17);
    DWORD count=0,index=99;CHECK("enumerate all",ImageEnumerateCertificates(f,CERT_SECTION_TYPE_ANY,&count,NULL,0)&&count==2);
    CHECK("filter type",ImageEnumerateCertificates(f,WIN_CERT_TYPE_X509,&count,&index,1)&&count==1&&index==1);
    n=0;BOOL r=ImageGetCertificateData(f,0,NULL,&n);CHECK("certificate size",!r&&GetLastError()==ERROR_INSUFFICIENT_BUFFER&&n==12);
    BYTE out[16];memset(out,0xa5,sizeof(out));n=4;r=ImageGetCertificateData(f,0,(void *)out,&n);
    CHECK("short certificate buffer",!r&&GetLastError()==ERROR_INSUFFICIENT_BUFFER&&n==12&&out[0]==0xa5);
    n=sizeof(out);CHECK("certificate bytes",ImageGetCertificateData(f,0,(void *)out,&n)&&n==12&&!memcmp(out,data+512,12));
    CHECK("reject invalid index",!ImageGetCertificateHeader(f,2,&header));
    CHECK("reject invalid handle",!ImageGetCertificateHeader(INVALID_HANDLE_VALUE,0,&header));
    DWORD bad=0;SetFilePointer(f,512,NULL,FILE_BEGIN);WriteFile(f,&bad,4,&n,NULL);
    CHECK("reject malformed certificate",!ImageGetCertificateHeader(f,0,&header));
    CloseHandle(f);DeleteFileA("imagecert-fixture.bin");test_digest();return failed;
}
