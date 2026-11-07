// HLSL compute shader -> WGSL through Slang (an independent compiler, for differential tests)
#include "slang.h"
#include <cstdio>
#include <string>
#include <vector>
#include <fstream>
#include <sstream>
static void diag(slang::IBlob* d){ if(d){ fprintf(stderr,"%s\n",(const char*)d->getBufferPointer()); } }
int main(int argc,char**argv){
  if(argc<4){fprintf(stderr,"usage: dir file entry [N=V...]\n");return 2;}
  std::string dir=argv[1], file=argv[2], entry=argv[3];
  std::ifstream f(dir+"/"+file); std::stringstream ss; ss<<f.rdbuf(); std::string src=ss.str();
  slang::IGlobalSession* g=nullptr; if(SLANG_FAILED(slang_createGlobalSession(SLANG_API_VERSION,&g))) return 3;
  slang::TargetDesc t{}; t.format=SLANG_WGSL;
  std::vector<std::string> names, vals; for(int i=4;i<argc;i++){ std::string a=argv[i]; auto p=a.find('='); names.push_back(a.substr(0,p)); vals.push_back(p==std::string::npos?"1":a.substr(p+1)); }
  std::vector<slang::PreprocessorMacroDesc> m; for(size_t i=0;i<names.size();i++) m.push_back({names[i].c_str(),vals[i].c_str()});
  const char* paths[]={dir.c_str()};
  slang::SessionDesc sd{}; sd.targets=&t; sd.targetCount=1; sd.searchPaths=paths; sd.searchPathCount=1; sd.preprocessorMacros=m.data(); sd.preprocessorMacroCount=m.size();
  slang::ISession* s=nullptr; if(SLANG_FAILED(g->createSession(sd,&s))) return 4;
  slang::IBlob* d=nullptr;
  slang::IModule* mod=s->loadModuleFromSourceString("m",(dir+"/"+file).c_str(),src.c_str(),&d); diag(d); if(!mod) return 5;
  slang::IEntryPoint* ep=nullptr; d=nullptr; if(SLANG_FAILED(mod->findAndCheckEntryPoint(entry.c_str(),SLANG_STAGE_COMPUTE,&ep,&d))){diag(d);return 6;}
  slang::IComponentType* comps[]={mod,ep}; slang::IComponentType* comp=nullptr; d=nullptr;
  if(SLANG_FAILED(s->createCompositeComponentType(comps,2,&comp,&d))){diag(d);return 7;}
  slang::IComponentType* linked=nullptr; d=nullptr; if(SLANG_FAILED(comp->link(&linked,&d))){diag(d);return 8;}
  slang::IBlob* code=nullptr; d=nullptr; if(SLANG_FAILED(linked->getEntryPointCode(0,0,&code,&d))){diag(d);return 9;}
  fwrite(code->getBufferPointer(),1,code->getBufferSize(),stdout);
  return 0;
}
