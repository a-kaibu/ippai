/* libs: -lnetapi32 -liphlpapi */
#include <winsock2.h>
#include <windows.h>
#include <nb30.h>
#include <iphlpapi.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
int main(void){
 NCB n={0};LANA_ENUM list={0};n.ncb_command=NCBENUM;n.ncb_buffer=(PUCHAR)&list;n.ncb_length=sizeof(list);UCHAR r=Netbios(&n);
 printf("enum: %u %d %d\n",r,list.length==1,n.ncb_retcode==r&&n.ncb_cmd_cplt==r);
 n.ncb_command=NCBRESET;n.ncb_lana_num=list.lana[0];printf("reset: %u\n",Netbios(&n));
 unsigned char data[sizeof(ADAPTER_STATUS)+4];memset(data,0x55,sizeof(data));n.ncb_command=NCBASTAT;n.ncb_buffer=data;n.ncb_length=sizeof(ADAPTER_STATUS);memset(n.ncb_callname,' ',NCBNAMSZ);n.ncb_callname[0]='*';r=Netbios(&n);
 ULONG size=0;GetAdaptersInfo(NULL,&size);IP_ADAPTER_INFO *a=malloc(size);GetAdaptersInfo(a,&size);
 printf("status: %u %d %d %d\n",r,memcmp(data,a->Address,6)==0,((ADAPTER_STATUS*)data)->name_count==0,data[sizeof(ADAPTER_STATUS)]==0x55);free(a);
 n.ncb_length=1;printf("short: %u\n",Netbios(&n));n.ncb_lana_num=99;n.ncb_command=NCBRESET;printf("bad lana: %u\n",Netbios(&n));n.ncb_command=NCBSEND;printf("unsupported: %u\n",Netbios(&n));
 return 0;
}
