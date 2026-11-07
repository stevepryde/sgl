// Explicit bake of Bistro's published HDRI using the PMREM generator whose
// output SGL accepts. Three is a build tool in ignored target/, never a game
// dependency. No code or assets are copied into the repository.
import { mkdir, copyFile, writeFile } from "node:fs/promises";
import { basename, isAbsolute, relative, resolve, sep } from "node:path";
import { chromium } from "playwright";
import { assertNoInputFileAliases } from "./bistro-export-files";

const [sourceArg, outputArg] = Bun.argv.slice(2);
if (!sourceArg || !outputArg) throw new Error("usage: bun scripts/bistro-environment.ts source.hdr output-directory");
const source=resolve(sourceArg), output=resolve(outputArg);
await assertNoInputFileAliases(["pmrem.rgba16","panorama.png","environment.json","THREE-LICENSE.txt"].map(name=>resolve(output,name)),[source]);
const tools=resolve(import.meta.dir,"../target/bistro-environment-tools");
await mkdir(tools,{recursive:true});
await mkdir(output,{recursive:true});
await writeFile(resolve(tools,"package.json"),JSON.stringify({private:true,dependencies:{three:"0.185.1"}}));
const install=Bun.spawnSync(["bun","install","--ignore-scripts"],{cwd:tools,stdout:"inherit",stderr:"inherit"});
if(install.exitCode!==0) process.exit(install.exitCode??1);
const three=resolve(tools,"node_modules/three");
let metadata: {width:number,height:number}|undefined;
const server=Bun.serve({hostname:"127.0.0.1",port:0,async fetch(request){
  const path=new URL(request.url).pathname;
  if(path==="/") return new Response(Bun.file(resolve(import.meta.dir,"bistro-environment.html")));
  if(path==="/backend.js") return new Response(Bun.file(resolve(import.meta.dir,"bistro-environment-backend.js")));
  if(path==="/source.hdr") return new Response(Bun.file(source));
  if(path.startsWith("/three/")) {
    const file=resolve(three,path.slice("/three/".length));
    const local=relative(three,file);
    if(local===".."||local.startsWith(`..${sep}`)||isAbsolute(local)) return new Response("invalid path",{status:400});
    return new Response(Bun.file(file));
  }
  if(request.method==="POST"&&path==="/pmrem") {
    metadata={width:Number(request.headers.get("x-width")),height:Number(request.headers.get("x-height"))};
    await Bun.write(resolve(output,"pmrem.rgba16"),await request.arrayBuffer());
    return new Response("saved");
  }
  if(request.method==="POST"&&path==="/panorama") {
    await Bun.write(resolve(output,"panorama.png"),await request.arrayBuffer());
    return new Response("saved");
  }
  return new Response("not found",{status:404});
}});
const browser=await chromium.launch({headless:true,args:["--enable-unsafe-webgpu",...(process.platform==="darwin"?["--use-angle=metal"]:[])]});
try {
  const page=await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.port}/`);
  const result=await page.evaluate(async()=>await (window as unknown as {bake:Promise<{error?:string,value?:{backend?:string,backend_asserted?:boolean,adapter_available?:boolean}}>}).bake);
  if(result.error) throw new Error(result.error);
  if(result.value?.backend!=="WebGPU"||!result.value.backend_asserted||!result.value.adapter_available) throw new Error("PMREM bake did not verify its WebGPU backend and adapter");
  await writeFile(resolve(output,"environment.json"),JSON.stringify({...metadata,generator:"Three.js 0.185.1 common/WebGPU PMREMGenerator (PMREMNode render-target convention)",source:basename(source),backend:result.value},null,2));
  await copyFile(resolve(three,"LICENSE"),resolve(output,"THREE-LICENSE.txt"));
  console.log(`Baked Bistro environment: ${JSON.stringify(result.value)}`);
} finally {
  await browser.close(); server.stop(true);
}
