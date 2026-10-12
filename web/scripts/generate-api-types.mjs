import { mkdir, readFile, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import openapiTS, { astToString } from "openapi-typescript";
import ts from "typescript";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryDirectory = path.resolve(scriptDirectory, "../..");
const contractDirectory = path.join(repositoryDirectory, "contracts");
const specPath = path.join(contractDirectory, "openapi.json");
const outputPath = path.resolve(scriptDirectory, "../lib/api/generated.ts");
const modulesDirectory = path.resolve(scriptDirectory, "../lib/api/generated");
const check = process.argv.includes("--check");
const fromExistingSpec = process.argv.includes("--from-openapi");

const document = fromExistingSpec
  ? JSON.parse(await readFile(specPath, "utf8"))
  : JSON.parse(execFileSync(
      "cargo",
      ["run", "--locked", "--manifest-path", "../Cargo.toml", "--bin", "api-openapi"],
      { cwd: path.resolve(scriptDirectory, ".."), encoding: "utf8" },
    ));
const generatedSpec = `${JSON.stringify(document, null, 2)}\n`;
const surfaces = {
  auth: (path) => path.startsWith("/auth/"),
  account: (path) => path === "/api/v1/session" || path === "/api/v1/account" || path.startsWith("/api/v1/account/"),
  servers: (path) => path === "/api/v1/servers"
    || path === "/api/v1/server/login-links"
    || path.startsWith("/api/v1/server/sessions/")
    || path === "/api/v1/server/sessions"
    || path === "/api/v1/servers/{server_id}"
    || path === "/api/v1/servers/{server_id}/commands",
  "server-data": (path) => path.startsWith("/api/v1/servers/{server_id}/")
    && path !== "/api/v1/servers/{server_id}/ai"
    && !path.startsWith("/api/v1/servers/{server_id}/ai/")
    && path !== "/api/v1/servers/{server_id}/commands"
    && path !== "/api/v1/servers/{server_id}/runtime-stats"
    && path !== "/api/v1/servers/{server_id}/console/commands"
    && path !== "/api/v1/servers/{server_id}/console/commands/{command_id}",
  "server-stats": (path) => path === "/api/v1/servers/{server_id}/runtime-stats",
  "server-console": (path) => path === "/api/v1/servers/{server_id}/console/commands"
    || path === "/api/v1/servers/{server_id}/console/commands/{command_id}",
  "server-ai": (path) => path === "/api/v1/servers/{server_id}/ai"
    || path.startsWith("/api/v1/servers/{server_id}/ai/"),
};
for (const path of Object.keys(document.paths)) {
  const owners = Object.values(surfaces).filter((belongs) => belongs(path)).length;
  if (owners !== 1) throw new Error(`OpenAPI path '${path}' belongs to ${owners} generated surfaces.`);
}
function declarations(source, moduleName) {
  const file = ts.createSourceFile(moduleName, source, ts.ScriptTarget.Latest, true);
  const wanted = new Map();
  for (const statement of file.statements) {
    if (ts.isInterfaceDeclaration(statement) && ["paths", "components", "operations"].includes(statement.name.text)) {
      wanted.set(statement.name.text, statement.getText(file));
    } else if (ts.isTypeAliasDeclaration(statement) && ["components", "operations"].includes(statement.name.text)) {
      wanted.set(statement.name.text, statement.getText(file));
    }
  }
  return wanted;
}

const generatedSchemaSource = astToString(await openapiTS({ ...document, paths: {} }));
const generatedSchemas = declarations(generatedSchemaSource, "generated/schemas.ts");
if (!generatedSchemas.has("components")) throw new Error("OpenAPI generation omitted shared schemas.");

const generatedModules = { schemas: generatedSchemaSource };
for (const [name, belongs] of Object.entries(surfaces)) {
    const partial = {
      ...document,
      paths: Object.fromEntries(Object.entries(document.paths).filter(([path]) => belongs(path))),
    };
    if (Object.keys(partial.paths).length === 0) {
      throw new Error(`OpenAPI surface '${name}' has no operations.`);
    }
    const source = astToString(await openapiTS(partial));
    const parts = declarations(source, `generated/${name}.ts`);
    if (!parts.has("paths")) throw new Error(`OpenAPI generation omitted paths for '${name}'.`);
    const operationTypes = parts.get("operations");
    const generatedModule = [
      "/* This file is generated from contracts/openapi.json. Do not edit it directly. */",
      'import type { components } from "./schemas";',
      "",
      parts.get("paths"),
      ...(operationTypes ? ["", operationTypes] : []),
      "",
    ].join("\n");
    if (generatedModule.split("\n").length > 1800) {
      throw new Error(`Generated OpenAPI surface '${name}' exceeds 1,800 lines; split the surface further.`);
    }
    generatedModules[name] = generatedModule;
}
const surfaceNames = Object.keys(surfaces);
const importLines = surfaceNames.map((name, index) =>
  `import type { operations as Operations${index}, paths as Paths${index} } from "./generated/${name}";`,
);
const generatedTypes = [
  "/* This file is generated from contracts/openapi.json. Do not edit it directly. */",
  'import type { components as SharedComponents, webhooks as SharedWebhooks, $defs as SharedDefinitions } from "./generated/schemas";',
  ...importLines,
  "",
  `export interface paths extends ${surfaceNames.map((_, index) => `Paths${index}`).join(", ")} {}`,
  "export type components = SharedComponents;",
  `export type operations = ${surfaceNames.map((_, index) => `Operations${index}`).join(" & ")};`,
  "export type webhooks = SharedWebhooks;",
  "export type $defs = SharedDefinitions;",
  "",
].join("\n");
if (!check) {
  await mkdir(contractDirectory, { recursive: true });
  await mkdir(modulesDirectory, { recursive: true });
  await writeFile(specPath, generatedSpec);
  process.stdout.write(`Generated ${path.relative(process.cwd(), specPath)}\n`);
  for (const [name, contents] of Object.entries(generatedModules)) {
    const file = path.join(modulesDirectory, `${name}.ts`);
    await writeFile(file, contents);
    process.stdout.write(`Generated ${path.relative(process.cwd(), file)}\n`);
  }
  await writeFile(outputPath, generatedTypes);
  process.stdout.write(`Generated ${path.relative(process.cwd(), outputPath)}\n`);
} else {
  let existingSpec;
  let existingTypes;
  let existingModules;
  try {
    [existingSpec, existingTypes, ...existingModules] = await Promise.all([
      readFile(specPath, "utf8"),
      readFile(outputPath, "utf8"),
      ...surfaceNames.map((name) => readFile(path.join(modulesDirectory, `${name}.ts`), "utf8")),
    ]);
  } catch {
    throw new Error("Generated API files are missing; run pnpm api:generate.");
  }
  const modulesMatch = surfaceNames.every((name, index) => existingModules[index] === generatedModules[name]);
  if (existingSpec !== generatedSpec || existingTypes !== generatedTypes || !modulesMatch) {
    throw new Error("Generated API files are stale; run pnpm api:generate and commit the result.");
  }
  process.stdout.write("OpenAPI and TypeScript output match the Rust API definition.\n");
}
