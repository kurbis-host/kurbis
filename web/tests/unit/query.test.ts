import { describe, expect, it } from "vitest";
import { selectedServerId } from "../../lib/query";

describe("selected server route", () => {
  it("recognizes the shared management URL only", () => {
    expect(selectedServerId("/manage/server-42/backups")).toBe("server-42");
    expect(selectedServerId("/app/servers")).toBeNull();
    expect(selectedServerId("/app/servers/server-42")).toBeNull();
    expect(selectedServerId("/app/account")).toBeNull();
  });

  it("decodes encoded server identifiers", () => {
    expect(selectedServerId("/manage/server%2F42/settings")).toBe("server/42");
  });
});
