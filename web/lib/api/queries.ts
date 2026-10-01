import {
  accountSchema,
  activityListSchema,
  logPageSchema,
  playerListSchema,
  serverListSchema,
  serverSchema,
  sessionSchema,
  snapshotListSchema,
  whitelistListSchema,
  worldExportListSchema,
} from "../contracts";
import { apiRequest } from "./client";

export const serverPath = (serverId: string, suffix = "") =>
  `/api/v1/servers/${encodeURIComponent(serverId)}${suffix}`;

export const fetchSession = () => apiRequest("/api/v1/session", sessionSchema);
export const fetchAccount = () => apiRequest("/api/v1/account", accountSchema);
export const fetchServers = () => apiRequest("/api/v1/servers", serverListSchema);
export const fetchServer = (serverId: string) =>
  apiRequest(serverPath(serverId), serverSchema);
export const fetchActivity = (serverId: string) =>
  apiRequest(`${serverPath(serverId, "/activity")}`, activityListSchema);
export const fetchLogs = (serverId: string) =>
  apiRequest(`${serverPath(serverId, "/logs")}?limit=240`, logPageSchema);
export const fetchSnapshots = (serverId: string) =>
  apiRequest(serverPath(serverId, "/snapshots"), snapshotListSchema);
export const fetchWorldExports = (serverId: string) =>
  apiRequest(serverPath(serverId, "/exports"), worldExportListSchema);
export const fetchPlayers = (serverId: string) =>
  apiRequest(serverPath(serverId, "/players"), playerListSchema);
export const fetchWhitelist = (serverId: string) =>
  apiRequest(serverPath(serverId, "/whitelist"), whitelistListSchema);
