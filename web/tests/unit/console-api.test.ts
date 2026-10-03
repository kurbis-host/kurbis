import { afterEach, describe, expect, it, vi } from "vitest";
import { browserApi } from "../../lib/api/operations";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("console command API adapter", () => {
  it("queues through the generated route with a durable request ID and session credentials", async () => {
    const requestId = "00000000-0000-4000-8000-000000000099";
    vi.spyOn(globalThis.crypto, "randomUUID").mockReturnValue(requestId);
    const fetchMock = vi.fn(async (_input: RequestInfo | URL) =>
      Response.json({ id: "command-1", status: "queued" }, { status: 202 }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await browserApi.sendConsoleCommand("server/with spaces", "say hello", requestId);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const request = fetchMock.mock.calls[0][0] as Request;
    expect(new URL(request.url).pathname).toBe("/api/v1/servers/server%2Fwith%20spaces/console/commands");
    expect(request.method).toBe("POST");
    expect(request.credentials).toBe("include");
    expect(await request.json()).toEqual({ request_id: requestId, command: "say hello" });
  });

  it("surfaces the API message when command submission is rejected", async () => {
    vi.stubGlobal("fetch", vi.fn(async (_input: RequestInfo | URL) => Response.json(
      { code: "runtime_offline", message: "The runtime is no longer online." },
      { status: 409 },
    )));

    await expect(browserApi.sendConsoleCommand("server-1", "list"))
      .rejects.toThrow("The runtime is no longer online.");
  });
});
