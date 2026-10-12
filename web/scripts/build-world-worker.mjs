// The worker that builds the landing film's world, as one plain script the page's HTML
// can start by itself (lib/landing/boot.ts): public/world.worker.js. Run from web/ after
// changing anything under lib/landing/world:
//
//   pnpm run build:worker
//
// The file is committed; tests/unit/landing/world.test.ts says when it is out of date.

import { writeFileSync } from "node:fs";
import { rolldown } from "rolldown";

export async function worldWorker() {
  const bundle = await rolldown({ input: "lib/landing/world/worker.ts", logLevel: "silent" });
  const { output } = await bundle.generate({ format: "iife", minify: true });
  await bundle.close();
  return output[0].code;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const code = await worldWorker();
  writeFileSync("public/world.worker.js", code);
  console.log(`public/world.worker.js: ${code.length} bytes`);
}
