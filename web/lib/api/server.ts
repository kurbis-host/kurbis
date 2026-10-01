import { cookies } from "next/headers";
import { cache } from "react";
import type { ZodType } from "zod";
import { accountSchema, activityListSchema, logPageSchema, playerListSchema, serverListSchema, serverSchema, sessionSchema, snapshotListSchema, whitelistListSchema } from "../contracts";
import { controlPlaneOrigin } from "../control-plane";
import { serverPath } from "./queries";

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
export const getServers = () => request("/api/v1/servers", serverListSchema);
export async function getSessionOrNull() {
  try { return await getSession(); } catch (error) { if (error instanceof ServerApiError && error.status === 401) return null; return null; }
}
export const getServer = cache((serverId: string) => request(serverPath(serverId), serverSchema));
export const getActivity = (serverId: string) => request(serverPath(serverId, "/activity"), activityListSchema);
export const getLogs = (serverId: string) => request(`${serverPath(serverId, "/logs")}?limit=240`, logPageSchema);
export const getSnapshots = (serverId: string) => request(serverPath(serverId, "/snapshots"), snapshotListSchema);
export const getPlayers = (serverId: string) => request(serverPath(serverId, "/players"), playerListSchema);
export const getWhitelist = (serverId: string) => request(serverPath(serverId, "/whitelist"), whitelistListSchema);
