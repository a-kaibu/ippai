// Golden outputs of Mesa's libGLU 9.0.2 (on OSMesa) for crates/sake/src/glu.rs's tests.
#include <GL/osmesa.h>
#include <GL/glu.h>
#include <stdio.h>
#include <stdlib.h>
static unsigned s=12345; static unsigned rnd(){s=s*1103515245+12345;return (s>>8)&0xffffff;}
static void pm(FILE*f,const char*n){double m[16];glGetDoublev(GL_MODELVIEW_MATRIX,m);fprintf(f,"%s",n);for(int i=0;i<16;i++)fprintf(f," %.9g",m[i]);fprintf(f,"\n");}
int main(){
 OSMesaContext c=OSMesaCreateContext(OSMESA_RGBA,NULL);static unsigned char buf[64*64*4];OSMesaMakeCurrent(c,buf,GL_UNSIGNED_BYTE,64,64);
 FILE*f=fopen("matrix.txt","w");
 glMatrixMode(GL_MODELVIEW);
 for(int k=0;k<200;k++){double a=10+rnd()%150,b=0.25+(rnd()%400)/100.0,n=0.1+(rnd()%300)/100.0,fa=n+1+(rnd()%1000);
  glLoadIdentity();gluPerspective(a,b,n,fa);char t[128];sprintf(t,"persp %.17g %.17g %.17g %.17g",a,b,n,fa);pm(f,t);}
 for(int k=0;k<200;k++){double v[9];for(int i=0;i<9;i++)v[i]=((int)(rnd()%2000)-1000)/100.0;v[6]=0;v[7]=1;v[8]=0;
  glLoadIdentity();gluLookAt(v[0],v[1],v[2],v[3],v[4],v[5],v[6],v[7],v[8]);char t[400];sprintf(t,"lookat %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g",v[0],v[1],v[2],v[3],v[4],v[5],v[6],v[7],v[8]);pm(f,t);}
 for(int k=0;k<100;k++){GLint vp[4]={(int)(rnd()%50),(int)(rnd()%50),100+(int)(rnd()%500),100+(int)(rnd()%500)};double x=rnd()%600,y=rnd()%600,dx=1+rnd()%50,dy=1+rnd()%50;
  glLoadIdentity();gluPickMatrix(x,y,dx,dy,vp);char t[300];sprintf(t,"pick %.17g %.17g %.17g %.17g %d %d %d %d",x,y,dx,dy,vp[0],vp[1],vp[2],vp[3]);pm(f,t);}
 fclose(f);
 // gluProject with float-free inputs (double matrices given directly)
 f=fopen("project.txt","w");
 for(int k=0;k<200;k++){double m[16],p[16];for(int i=0;i<16;i++){m[i]=((int)(rnd()%2001)-1000)/250.0;p[i]=((int)(rnd()%2001)-1000)/250.0;}
  GLint vp[4]={(int)(rnd()%50),(int)(rnd()%50),1+(int)(rnd()%800),1+(int)(rnd()%800)};double o[3];for(int i=0;i<3;i++)o[i]=((int)(rnd()%2001)-1000)/100.0;
  double x,y,z;int r=gluProject(o[0],o[1],o[2],m,p,vp,&x,&y,&z);
  double ux=0,uy=0,uz=0;int r2=gluUnProject(o[0]*10,o[1]*10,o[2]/10,m,p,vp,&ux,&uy,&uz);
  fprintf(f,"m");for(int i=0;i<16;i++)fprintf(f," %.17g",m[i]);fprintf(f," p");for(int i=0;i<16;i++)fprintf(f," %.17g",p[i]);
  fprintf(f," vp %d %d %d %d o %.17g %.17g %.17g = %d %.17g %.17g %.17g un %d %.17g %.17g %.17g\n",vp[0],vp[1],vp[2],vp[3],o[0],o[1],o[2],r,x,y,z,r2,ux,uy,uz);}
 fclose(f);
 f=fopen("scale.txt","w");
 int al[4]={1,2,4,8};
 for(int k=0;k<400;k++){int cm=1+rnd()%4,wi=1+rnd()%17,hi=1+rnd()%17,wo=1+rnd()%17,ho=1+rnd()%17,ua=al[rnd()%4],pa=al[rnd()%4];
  if(k%5==0){wo=(wi+1)/2;ho=(hi+1)/2;wi=wo*2;hi=ho*2;}
  static unsigned char in[20000],out[20000];int ri=((wi*cm+ua-1)/ua)*ua,ro=((wo*cm+pa-1)/pa)*pa;
  for(int i=0;i<ri*hi;i++)in[i]=rnd()&255; for(int i=0;i<ro*ho;i++)out[i]=0;
  GLenum fm=cm==1?GL_LUMINANCE:cm==2?GL_LUMINANCE_ALPHA:cm==3?GL_RGB:GL_RGBA;
  glPixelStorei(GL_UNPACK_ALIGNMENT,ua);glPixelStorei(GL_PACK_ALIGNMENT,pa);
  int r=gluScaleImage(fm,wi,hi,GL_UNSIGNED_BYTE,in,wo,ho,GL_UNSIGNED_BYTE,out);
  fprintf(f,"%d %d %d %d %d %d %d %d |",cm,wi,hi,wo,ho,ua,pa,r);for(int i=0;i<ri*hi;i++)fprintf(f,"%02x",in[i]);fprintf(f,"|");
  for(int y=0;y<ho;y++)for(int i=0;i<wo*cm;i++)fprintf(f,"%02x",out[y*ro+i]);fprintf(f,"\n");}
 fclose(f);
 return 0;}
