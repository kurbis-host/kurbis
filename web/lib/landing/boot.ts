/**
 * What the landing page's HTML starts by itself, in a few lines of script at its top,
 * before any of the page's own code has arrived (which is most of a second on a slow
 * line or a slow device): the worker that builds the valley, and the fetching of every
 * other file the first frame needs. The page's code takes it all over when it comes
 * (engine/world.ts, engine/early.ts).
 *
 * Only where the film can play: a browser without WebGL2 starts and fetches nothing.
 *
 * This runs on the server, where the page is rendered.
 */

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { bundleUrl } from "./engine/address";

/** The worker's script (public/world.worker.js), by its version: a browser may keep it for good. */
export function workerUrl(): string {
  const script = readFileSync(join(process.cwd(), "public", "world.worker.js"));
  return `/world.worker.js?v=${createHash("sha256").update(script).digest("hex").slice(0, 16)}`;
}

/** The script. (Old-fashioned on purpose: it must run as it is, in whatever reads the page.) */
export function bootScript(worker: string): string {
  const u = (file: string) => JSON.stringify(bundleUrl(file));
  return `(function(){try{
var g=document.createElement("canvas").getContext("webgl2");if(!g)return;
var x=g.getExtension("WEBGL_lose_context");if(x)x.loseContext();
var t=innerWidth<innerHeight,a=t?"tall":"wide",k=window.__kurbis={replies:[],fetched:{}};
var w=k.worker=new Worker(${JSON.stringify(worker)}),p;
w.onmessage=function(e){k.replies.push(e.data)};w.onerror=function(){k.failed=true};
if((navigator.hardwareConcurrency||2)>2){var h=k.helper=new Worker(${JSON.stringify(worker)}),c=new MessageChannel();h.postMessage({help:c.port2},[c.port2]);p=c.port1}
k.grid=${u("valley.grid.gz")};k.patch=t?${u("dungeon.tall.patch.gz")}:void 0;
w.postMessage({id:0,grid:k.grid,patch:k.patch,port:p},p?[p]:[]);
var f=["scene.json","layout."+a+".json","small.pack.gz","models."+a+".glb.gz"];
for(var i=0;i<f.length;i++)k.fetched[f[i]]=fetch(${JSON.stringify(bundleUrl("@"))}.replace("@",f[i]));
if(window.performance&&performance.mark)performance.mark("landing:booted");
}catch(e){}})()`;
}
