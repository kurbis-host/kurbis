import { cookies } from "next/headers";
import type { ZodType } from "zod";
import { accountSchema, activityListSchema, logPageSchema, optionalServerSchema, playerListSchema, sessionSchema, snapshotListSchema, whitelistListSchema } from "../contracts";
import { controlPlaneOrigin } from "../control-plane";

export class ServerApiError extends Error {
  constructor(message: string, public readonly status: number) {
    super(message);
    this.name = "ServerApiError";
  }
}

async function request<T>(path: string, schema: ZodType<T>): Promise<T> {
  const cookieStore = await cookies();
  const cookie = cookieStore.getAll().map(({ name, value }) => `${name}=${value}`).join("; ");
  const response = await fetch(`${controlPlaneOrigin()}${path}`, { cache: "no-store", headers: { accept: "application/json", ...(cookie ? { cookie } : {}) } });
  if (!response.ok) {
    let message = `Control plane request failed (${response.status})`;
    try {
      const payload = (await response.json()) as { error?: string; message?: string };
      message = payload.message ?? payload.error ?? message;
    } catch {}
    throw new ServerApiError(message, response.status);
  }
  return schema.parse(await response.json());
}

export const getSession = () => request("/api/v1/session", sessionSchema);
export const getAccount = () => request("/api/v1/account", accountSchema);
export async function getSessionOrNull() {
  try { return await getSession(); } catch (error) { if (error instanceof ServerApiError && error.status === 401) return null; return null; }
}
export const getServer = () => request("/api/v1/server", optionalServerSchema);
export const getActivity = () => request("/api/v1/server/activity", activityListSchema);
export const getLogs = () => request("/api/v1/server/logs?limit=240", logPageSchema);
export const getSnapshots = () => request("/api/v1/server/snapshots", snapshotListSchema);
export const getPlayers = () => request("/api/v1/server/players", playerListSchema);
export const getWhitelist = () => request("/api/v1/server/whitelist", whitelistListSchema);
