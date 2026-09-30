import { accountSchema, activityListSchema, logPageSchema, optionalServerSchema, playerListSchema, sessionSchema, snapshotListSchema, whitelistListSchema } from "../contracts";
import { apiRequest } from "./client";

export const fetchSession = () => apiRequest("/api/v1/session", sessionSchema);
export const fetchAccount = () => apiRequest("/api/v1/account", accountSchema);
export const fetchServer = () => apiRequest("/api/v1/server", optionalServerSchema);
export const fetchActivity = () => apiRequest("/api/v1/server/activity", activityListSchema);
export const fetchLogs = () => apiRequest("/api/v1/server/logs?limit=240", logPageSchema);
export const fetchSnapshots = () => apiRequest("/api/v1/server/snapshots", snapshotListSchema);
export const fetchPlayers = () => apiRequest("/api/v1/server/players", playerListSchema);
export const fetchWhitelist = () => apiRequest("/api/v1/server/whitelist", whitelistListSchema);
