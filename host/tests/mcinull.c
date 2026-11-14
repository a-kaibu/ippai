/* libs: -lwinmm */
/* MCI commands given no parameter block (review b07): MCIERR_NULL_PARAMETER_BLOCK, as
   Windows, instead of reading (or, MCI_WHERE, writing) the bottom of the address space. */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>

int main(void){
 char dir[MAX_PATH],path[MAX_PATH];GetTempPathA(MAX_PATH,dir);GetTempFileNameA(dir,"mn",0,path);
 /* a second of 8 kHz 8-bit mono silence */
 HANDLE f=CreateFileA(path,GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 struct{char riff[4];DWORD size;char wave[4];char fmt[4];DWORD fsize;WORD tag,ch;DWORD rate,bps;WORD align,bits;char data[4];DWORD dsize;} h=
  {{'R','I','F','F'},36+8000,{'W','A','V','E'},{'f','m','t',' '},16,1,1,8000,8000,1,8,{'d','a','t','a'},8000};
 static BYTE pcm[8000];memset(pcm,0x80,sizeof(pcm));DWORD w;
 WriteFile(f,&h,sizeof(h),&w,NULL);WriteFile(f,pcm,sizeof(pcm),&w,NULL);CloseHandle(f);
 char cmd[MAX_PATH+64];wsprintfA(cmd,"open \"%s\" type waveaudio alias w",path);
 MCIERROR e=mciSendStringA(cmd,NULL,0,NULL);
 printf("open %lu\n",e);
 MCIDEVICEID id=mciGetDeviceIDA("w");
 printf("set %lu\n",mciSendCommandA(id,MCI_SET,MCI_SET_TIME_FORMAT,0));
 printf("info %lu\n",mciSendCommandA(id,MCI_INFO,MCI_INFO_FILE,0));
 printf("status %lu\n",mciSendCommandA(id,MCI_STATUS,MCI_STATUS_ITEM,0));
 MCI_SET_PARMS sp={0};sp.dwTimeFormat=MCI_FORMAT_MILLISECONDS;
 printf("set with a block %lu\n",mciSendCommandA(id,MCI_SET,MCI_SET_TIME_FORMAT,(DWORD_PTR)&sp));
 MCI_STATUS_PARMS st={0};st.dwItem=MCI_STATUS_LENGTH;
 printf("status with a block %lu\n",mciSendCommandA(id,MCI_STATUS,MCI_STATUS_ITEM,(DWORD_PTR)&st));
 mciSendStringA("close w",NULL,0,NULL);
 DeleteFileA(path);
 return 0;
}
