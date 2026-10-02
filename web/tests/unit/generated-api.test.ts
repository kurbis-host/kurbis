import { describe, expect, it } from "vitest";
import openapi from "../../../contracts/openapi.json";
import type { components, paths } from "../../lib/api/generated";

describe("generated public API contract", () => {
  it("documents a required path parameter for every templated public route", () => {
    const paths = openapi.paths as unknown as Record<string, Record<string, { parameters?: { in: string; name: string; required?: boolean }[] }>>;
    for (const [path, pathItem] of Object.entries(paths)) {
      const names = [...path.matchAll(/\{([^}]+)\}/g)].map((match) => match[1]);
      for (const [method, operation] of Object.entries(pathItem)) {
        if (!["get", "post", "put", "patch", "delete"].includes(method)) continue;
        const parameters = operation.parameters ?? [];
        const pathParameters = parameters.filter((parameter) => parameter.in === "path");
        expect(pathParameters.map((parameter) => parameter.name).sort(), `${method.toUpperCase()} ${path}`).toEqual(names.toSorted());
        expect(pathParameters.every((parameter) => parameter.required), `${method.toUpperCase()} ${path}`).toBe(true);
      }
    }
  });

  it("declares every referenced security scheme", () => {
    const schemes = new Set(Object.keys(openapi.components?.securitySchemes ?? {}));
    for (const [path, item] of Object.entries(openapi.paths)) {
      for (const [method, operation] of Object.entries(item)) {
        if (!["get", "post", "put", "patch", "delete"].includes(method)) continue;
        for (const requirement of operation.security ?? []) {
          for (const name of Object.keys(requirement)) expect(schemes.has(name), `${method} ${path} -> ${name}`).toBe(true);
        }
      }
    }
  });

  it("excludes private ZIP storage metadata and keeps lifecycle commands as a tagged union", () => {
    const exportSchema = openapi.components?.schemas?.WorldExport;
    expect(exportSchema && "properties" in exportSchema ? exportSchema.properties : {}).not.toHaveProperty("archive_key");

    const grantSchema = openapi.components?.schemas?.ServerScopedSession;
    expect(grantSchema && "properties" in grantSchema ? grantSchema.properties : {}).not.toHaveProperty("expires_at");

    const commandSchema = openapi.components?.schemas?.ServerCommandRequest as unknown as { oneOf?: { properties?: Record<string, { const?: string }> }[] } | undefined;
    const variants = commandSchema?.oneOf ?? [];
    const commands = variants.map((variant) => {
      const properties = "properties" in variant ? variant.properties : undefined;
      const discriminator = properties?.command;
      return discriminator && "const" in discriminator ? discriminator.const : undefined;
    });
    expect(commands).toEqual(["start", "stop", "restart", "snapshot", "reset", "delete", "restore"]);
  });

  it("exposes command and server DTO types to TypeScript consumers", () => {
    const start: components["schemas"]["ServerCommandRequest"] = { command: "start" };
    const restore: components["schemas"]["ServerCommandRequest"] = { command: "restore", generation: 1 };
    const commandRoute: keyof paths = "/api/v1/servers/{server_id}/commands";
    expect([start.command, restore.command, commandRoute]).toEqual(["start", "restore", "/api/v1/servers/{server_id}/commands"]);
  });
});
