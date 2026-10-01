import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const portBase = Number(process.env.E2E_PORT_BASE ?? "18080");
if (!Number.isInteger(portBase) || portBase < 1024 || portBase > 65533) {
  throw new Error("E2E_PORT_BASE must be an integer between 1024 and 65533");
}

const env = {
  ...process.env,
  NEXT_DIST_DIR: `.next-e2e-${portBase}`,
  CONTROL_PLANE_INTERNAL_URL: `http://127.0.0.1:${portBase}`,
};
const generatedTypeFiles = ["next-env.d.ts", "tsconfig.json"];
const originalTypeFiles = new Map(
  generatedTypeFiles.map((file) => [file, readFileSync(file)]),
);

function run(args, extraEnv = {}) {
  const result = spawnSync("pnpm", args, {
    cwd: process.cwd(),
    env: { ...env, ...extraEnv },
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.signal) throw new Error(`pnpm ${args.join(" ")} stopped by ${result.signal}`);
  if (result.status !== 0) throw new Error(`pnpm ${args.join(" ")} exited with status ${result.status}`);
}

try {
  run(["run", "build"]);
  const testArgs = process.argv.slice(2);
  if (testArgs[0] === "--") testArgs.shift();
  run(["exec", "playwright", "test", ...testArgs], {
    E2E_NEXT_MODE: "production",
  });
} finally {
  // Next writes distDir-specific generated type paths into these tracked files.
  for (const [file, contents] of originalTypeFiles) writeFileSync(file, contents);
}
