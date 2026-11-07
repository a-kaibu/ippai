// Golden mipmap chains of Mesa's libGLU 9.0.2 gluBuild2DMipmaps (on OSMesa), read back per level.
#include <GL/osmesa.h>
#include <GL/glu.h>
#include <stdio.h>
static unsigned s=777; static unsigned rnd(){s=s*1103515245+12345;return (s>>8)&0xffffff;}
int main(){
 OSMesaContext c=OSMesaCreateContext(OSMESA_RGBA,NULL);static unsigned char buf[64*64*4];OSMesaMakeCurrent(c,buf,GL_UNSIGNED_BYTE,64,64);
 FILE*f=fopen("mipmap.txt","w");int al[4]={1,2,4,8};GLuint t;glGenTextures(1,&t);glBindTexture(GL_TEXTURE_2D,t);
 for(int k=0;k<150;k++){int cm=(int[]){1,3,4}[rnd()%3],w=1+rnd()%21,h=1+rnd()%21,ua=al[rnd()%4];
  if(k%4==0){w=1<<(rnd()%5);h=1<<(rnd()%5);}
  static unsigned char in[20000],out[20000];int ri=((w*cm+ua-1)/ua)*ua;for(int i=0;i<ri*h;i++)in[i]=rnd()&255;
  GLenum fm=cm==1?GL_LUMINANCE:cm==3?GL_RGB:GL_RGBA;
  glPixelStorei(GL_UNPACK_ALIGNMENT,ua);glPixelStorei(GL_PACK_ALIGNMENT,1);
  int r=gluBuild2DMipmaps(GL_TEXTURE_2D,cm,w,h,fm,GL_UNSIGNED_BYTE,in);
  fprintf(f,"%d %d %d %d %d |",cm,w,h,ua,r);for(int i=0;i<ri*h;i++)fprintf(f,"%02x",in[i]);
  for(int lv=0;lv<16;lv++){GLint lw=0,lh=0;glGetTexLevelParameteriv(GL_TEXTURE_2D,lv,GL_TEXTURE_WIDTH,&lw);glGetTexLevelParameteriv(GL_TEXTURE_2D,lv,GL_TEXTURE_HEIGHT,&lh);
   if(lw==0)break; glGetTexImage(GL_TEXTURE_2D,lv,fm,GL_UNSIGNED_BYTE,out);fprintf(f,"|%dx%d:",lw,lh);for(int i=0;i<lw*lh*cm;i++)fprintf(f,"%02x",out[i]);
   if(lw==1&&lh==1)break;}
  fprintf(f,"\n");
  // (clear the levels for the next one)
  glDeleteTextures(1,&t);glGenTextures(1,&t);glBindTexture(GL_TEXTURE_2D,t);}
 fclose(f);return 0;}
