#!/usr/bin/env python3
"""Regenerate FSR1 oracle bytes using pinned, unchanged AMD filter functions.
Usage: python3 generate.py /path/to/FidelityFX-SDK/sdk/include/FidelityFX/gpu
Requires clang++ with ext_vector_type and _Float16 (Apple Clang supports both).
Only fixtures are generated; no network, SGL shader source, or SGL algorithm
implementation is used. AMD source stays under its original MIT license.
"""
from pathlib import Path
import hashlib, re, subprocess, sys, tempfile
sdk=Path(sys.argv[1])
for file, digest in {
    "fsr1/ffx_fsr1.h": "5d608d9d0645da03a3aa6c729c5ad990c6f222053eab850b8e218607c1baaa9d",
    "ffx_core_gpu_common.h": "0e3c50cb0b4592c051d87e028a1dfa54fbd097942ebb3146964db801dec095d0",
}.items():
    if hashlib.sha256((sdk/file).read_bytes()).hexdigest() != digest:
        raise SystemExit("Expected AMD FidelityFX SDK 1.1.4 c6efa6bf source: " + file)
h=(sdk/'fsr1/ffx_fsr1.h').read_text()
core=(sdk/'ffx_core_gpu_common.h').read_text()
def function(source,name):
 start=source.rfind('\n',0,source.index(name+'('))+1
 brace=source.index('{',start);depth=1;end=brace+1
 while depth:
  depth+= (source[end]=='{')-(source[end]=='}');end+=1
 return source[start:end]+'\n'
funcs='\n'.join(function(h,n) for n in ['ffxFsrPopulateEasuConstants','fsrEasuTapFloat','fsrEasuSetFloat','ffxFsrEasuFloat','FsrRcasF'])
funcs=re.sub(r'FFX_PARAMETER_(?:INOUT|OUT)\s+(\w+)',r'\1 &',funcs)
funcs=funcs.replace('FFX_PARAMETER_IN ','').replace('FFX_STATIC ','')
funcs=re.sub(r'\b(?:out|inout) (Ffx\w+) ',r'\1 &',funcs)
shim=r'''
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <algorithm>
using FfxFloat32=float;
using FfxUInt32=uint32_t;
using FfxBoolean=bool;
typedef float FfxFloat32x2 __attribute__((ext_vector_type(2)));
typedef float FfxFloat32x3 __attribute__((ext_vector_type(3)));
typedef float FfxFloat32x4 __attribute__((ext_vector_type(4)));
typedef uint32_t FfxUInt32x2 __attribute__((ext_vector_type(2)));
typedef uint32_t FfxUInt32x4 __attribute__((ext_vector_type(4)));
typedef int FfxInt32x2 __attribute__((ext_vector_type(2)));
#define FfxFloat32x2(...) make_float2(__VA_ARGS__)
FfxFloat32x2 make_float2(float x,float y){return {x,y};}
FfxFloat32x2 make_float2(FfxUInt32x2 v){return {(float)v.x,(float)v.y};}
#define FfxFloat32x3(...) ((FfxFloat32x3){__VA_ARGS__})
#define FfxFloat32x4(...) ((FfxFloat32x4){__VA_ARGS__})
#define FfxInt32x2(...) __builtin_convertvector((__VA_ARGS__),FfxInt32x2)
#define FfxUInt32x2(...) ((FfxUInt32x2){__VA_ARGS__})
#define FFX_TRUE true
#define FFX_FALSE false
#define FSR_RCAS_LIMIT (0.25-(1.0/16.0))
using std::max;
using std::abs;
float ffxAsFloat(uint32_t u){float f;memcpy(&f,&u,4);return f;}
uint32_t ffxAsUInt32(float f){uint32_t u;memcpy(&u,&f,4);return u;}
FfxFloat32x2 ffxAsFloat(FfxUInt32x2 u){return {ffxAsFloat(u.x),ffxAsFloat(u.y)};}
float ffxReciprocal(float x){return 1.f/x;}
float ffxSaturate(float x){return std::clamp(x,0.f,1.f);}
FfxFloat32x2 floor(FfxFloat32x2 v){return {std::floor(v.x),std::floor(v.y)};}
FfxFloat32x2 ffxBroadcast2(float x){return {x,x};}
FfxFloat32x3 ffxBroadcast3(float x){return {x,x,x};}
FfxFloat32x4 ffxBroadcast4(float x){return {x,x,x,x};}
template<class T>T ffxMin(T a,T b){return a<b?a:b;}
template<> FfxFloat32x3 ffxMin(FfxFloat32x3 a,FfxFloat32x3 b){return {std::min(a.x,b.x),std::min(a.y,b.y),std::min(a.z,b.z)};}
FfxFloat32x3 max(FfxFloat32x3 a,FfxFloat32x3 b){return {std::max(a.x,b.x),std::max(a.y,b.y),std::max(a.z,b.z)};}
template<class T>T ffxMin3(T a,T b,T c){return ffxMin(ffxMin(a,b),c);}
template<class T>T ffxMax3(T a,T b,T c){return max(max(a,b),c);}
FfxFloat32x3 source(int x,int y){x=std::clamp(x,0,3);y=std::clamp(y,0,2);return {float((x+2*y)%4+1)/4,float((3*x+y)%4+1)/4,float((x+y)%3+1)/4};}
FfxFloat32x4 gather(FfxFloat32x2 p,int channel){int x=int(std::floor(p.x*4-.5)),y=int(std::floor(p.y*3-.5));return {source(x,y+1)[channel],source(x+1,y+1)[channel],source(x+1,y)[channel],source(x,y)[channel]};}
FfxFloat32x4 FsrEasuRF(FfxFloat32x2 p){return gather(p,0);}
FfxFloat32x4 FsrEasuGF(FfxFloat32x2 p){return gather(p,1);}
FfxFloat32x4 FsrEasuBF(FfxFloat32x2 p){return gather(p,2);}
FfxFloat32x3 easu[35];
FfxFloat32x4 FsrRcasLoadF(FfxInt32x2 p){auto c=easu[std::clamp(p.y,0,4)*7+std::clamp(p.x,0,6)];return {c.r,c.g,c.b,1};}
void FsrRcasInputF(float&,float&,float&){}
'''
# constructor mixed casting special: int vectors have only two-offset or vector conversion
shim=shim.replace('#define FfxInt32x2(...) __builtin_convertvector((__VA_ARGS__),FfxInt32x2)', '#define FfxInt32x2(...) make_int2(__VA_ARGS__)\nFfxInt32x2 make_int2(int x,int y){return {x,y};}\nFfxInt32x2 make_int2(FfxUInt32x2 v){return {(int)v.x,(int)v.y};}')
for n in ['ffxApproximateReciprocal','ffxApproximateReciprocalMedium','ffxApproximateReciprocalSquareRoot']:
 shim+=function(core,n)
main=r'''
int main(){
 FfxUInt32x4 c0,c1,c2,c3;
 ffxFsrPopulateEasuConstants(c0,c1,c2,c3,4,3,4,3,7,5);
 for(unsigned y=0;y<5;y++)for(unsigned x=0;x<7;x++) {
  ffxFsrEasuFloat(easu[y*7+x],FfxUInt32x2(x,y),c0,c1,c2,c3);
  for(int c=0;c<3;c++)easu[y*7+x][c]=(float)(_Float16)easu[y*7+x][c];
 }
 for(auto c:easu)for(int i=0;i<3;i++)fwrite(&c[i],4,1,stdout);
 FfxUInt32x4 rcas={ffxAsUInt32(std::exp2(-.2f)),0,0,0};
 for(unsigned y=0;y<5;y++)for(unsigned x=0;x<7;x++){
  float r,g,b;FsrRcasF(r,g,b,FfxUInt32x2(x,y),rcas);
  float c[3]={std::clamp(r,0.f,1.f),std::clamp(g,0.f,1.f),std::clamp(b,0.f,1.f)};
  for(auto v:c){v*=v;fwrite(&v,4,1,stdout);}
 }
}
'''
# vector lane addresses not addressable: serialize local float
main=main.replace('fwrite(&c[i],4,1,stdout);','{float v=c[i];fwrite(&v,4,1,stdout);}')
original=shim+funcs+main
with tempfile.TemporaryDirectory(prefix="sgl-fsr1-oracle-") as directory:
    temporary=Path(directory)
    for name, source in [("amd-reference",original)]:
        cpp=temporary/(name+".cpp")
        binary=temporary/name
        # Keep the original AMD MIT notice with extracted upstream code.
        cpp.write_text(h[:h.index("/// @defgroup")]+source)
        subprocess.run(["clang++","-std=c++17","-O2",str(cpp),"-o",str(binary)],check=True)
        data=subprocess.check_output([str(binary)])
        if len(data) != 840:
            raise SystemExit("Unexpected fixture size")
        (Path(__file__).parent/(name+".rgb32")).write_bytes(data)
