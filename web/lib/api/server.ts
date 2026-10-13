import { cookies } from "next/headers";
import { cache } from "react";
import createClient from "openapi-fetch";
import { controlPlaneOrigin } from "../control-plane";
import { ApiError } from "./client";
import { apiOperations } from "./operations";
import type { paths } from "./generated";

export { ApiError as ServerApiError };

async function withServerApi<T>(run: (api: ReturnType<typeof apiOperations>) => Promise<T>): Promise<T> {
  const cookie = (await cookies()).getAll().map(({ name, value }) => `${name}=${value}`).join("; ");
  const client = createClient<paths>({
    baseUrl: controlPlaneOrigin(),
    credentials: "include",
    headers: { accept: "application/json", ...(cookie ? { cookie } : {}) },
    fetch: (request) => fetch(request, { cache: "no-store" }),
  });
  return run(apiOperations(client));
}

export const getSession = () => withServerApi((api) => api.getSession());
export const getAccount = () => withServerApi((api) => api.getAccount());
export const getServers = () => withServerApi((api) => api.getServers());
export const getServerSessions = () => withServerApi((api) => api.getServerSessions());
export async function getSessionOrNull() {
  try {
    return await getSession();
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) return null;
    throw error;
  }
}
export const getServer = cache((serverId: string) => withServerApi((api) => api.getServer(serverId)));
export const getActivity = (serverId: string) => withServerApi((api) => api.getActivity(serverId));
export const getLogs = (serverId: string) => withServerApi((api) => api.getLogs(serverId));
export const getSnapshots = (serverId: string) => withServerApi((api) => api.getSnapshots(serverId));
export const getPlayers = (serverId: string) => withServerApi((api) => api.getPlayers(serverId));
export const getWhitelist = (serverId: string) => withServerApi((api) => api.getWhitelist(serverId));
export const getAiOverview = (serverId: string) => withServerApi((api) => api.getAiOverview(serverId));
export const getPlugins = (serverId: string) => withServerApi((api) => api.getPlugins(serverId));
