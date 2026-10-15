import createClient, { type Client } from "openapi-fetch";
import type { components, paths } from "./generated";
import { ApiError, apiRequest, apiVoid } from "./client";

export function apiOperations(client: Client<paths>) {
  const serverParams = (serverId: string) => ({ params: { path: { server_id: serverId } } });
  const aiJobParams = (serverId: string, jobId: string) => ({ params: { path: { server_id: serverId, job_id: jobId } } });
  const pluginParams = (serverId: string, pluginId: string) => ({ params: { path: { server_id: serverId, plugin_id: pluginId } } });

  return {
    getSession: () => apiRequest(client.GET("/api/v1/session")),
    getAccount: () => apiRequest(client.GET("/api/v1/account")),
    getServers: () => apiRequest(client.GET("/api/v1/servers")),
    getServerSessions: () => apiRequest(client.GET("/api/v1/server/sessions")),
    getServer: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}", serverParams(serverId))).then((server) => {
      if (!server) throw new ApiError("Server not found", 404);
      return server;
    }),
    getActivity: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/activity", serverParams(serverId))),
    getLogs: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/logs", {
      params: { path: { server_id: serverId }, query: { limit: 240 } },
    })),
    getSnapshots: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/snapshots", serverParams(serverId))),
    getWorldExports: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/exports", serverParams(serverId))),
    getPlayers: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/players", serverParams(serverId))),
    getWhitelist: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/whitelist", serverParams(serverId))),
    /** Player mutations intentionally accept a name without a UUID. */
    addWhitelistEntry: (serverId: string, body: components["schemas"]["WhitelistMutation"]) =>
      apiRequest(client.POST("/api/v1/servers/{server_id}/whitelist", {
        ...serverParams(serverId),
        body,
      })),
    removeWhitelistEntry: (serverId: string, playerId: string) => apiRequest(client.DELETE("/api/v1/servers/{server_id}/whitelist", {
      params: { path: { server_id: serverId }, query: { player_id: playerId } },
    })),
    /** Join requests carry the UUID the server reported; the panel only answers them. */
    letInJoinRequest: (serverId: string, playerId: string) =>
      apiRequest(client.POST("/api/v1/servers/{server_id}/whitelist/requests/{player_id}/let-in", {
        params: { path: { server_id: serverId, player_id: playerId } },
      })),
    claimJoinRequest: (serverId: string, playerId: string) =>
      apiRequest(client.POST("/api/v1/servers/{server_id}/whitelist/requests/{player_id}/claim", {
        params: { path: { server_id: serverId, player_id: playerId } },
      })),
    dismissJoinRequest: (serverId: string, playerId: string) =>
      apiRequest(client.DELETE("/api/v1/servers/{server_id}/whitelist/requests/{player_id}", {
        params: { path: { server_id: serverId, player_id: playerId } },
      })),
    getOperators: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/operators", serverParams(serverId))),
    addOperatorEntry: (serverId: string, body: components["schemas"]["OperatorMutation"]) =>
      apiRequest(client.POST("/api/v1/servers/{server_id}/operators", { ...serverParams(serverId), body })),
    removeOperatorEntry: (serverId: string, playerId: string) => apiRequest(client.DELETE("/api/v1/servers/{server_id}/operators", {
      params: { path: { server_id: serverId }, query: { player_id: playerId } },
    })),
    getServerAddress: (subdomain: string) => apiRequest(client.GET("/api/v1/server-addresses/{subdomain}", {
      params: { path: { subdomain } },
    })),
    updateServer: (serverId: string, body: components["schemas"]["UpdateServerRequest"]) => apiRequest(client.PATCH("/api/v1/servers/{server_id}", { ...serverParams(serverId), body })),
    command: (serverId: string, body: components["schemas"]["ServerCommandRequest"]) => apiRequest(client.POST("/api/v1/servers/{server_id}/commands", { ...serverParams(serverId), body })),
    sendConsoleCommand: async (serverId: string, commandText: string, requestId = globalThis.crypto.randomUUID()): Promise<void> => {
      await apiRequest(client.POST("/api/v1/servers/{server_id}/console/commands", {
        ...serverParams(serverId),
        body: { request_id: requestId, command: commandText },
      }));
    },
    getAiOverview: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/ai", serverParams(serverId))),
    updateAiSettings: (serverId: string, body: components["schemas"]["AiSettings"]) =>
      apiRequest(client.PUT("/api/v1/servers/{server_id}/ai/settings", { ...serverParams(serverId), body })),
    getAiJob: (serverId: string, jobId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/ai/jobs/{job_id}", aiJobParams(serverId, jobId))),
    cancelAiJob: (serverId: string, jobId: string) => apiRequest(client.POST("/api/v1/servers/{server_id}/ai/jobs/{job_id}/cancel", aiJobParams(serverId, jobId))),
    getPlugins: (serverId: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/plugins", serverParams(serverId))),
    getMarketPlugins: (serverId: string, query?: string) => apiRequest(client.GET("/api/v1/servers/{server_id}/plugins/market", {
      params: { path: { server_id: serverId }, query: query ? { query } : {} },
    })),
    installMarketPlugin: (serverId: string, marketPluginId: number) => apiRequest(client.POST("/api/v1/servers/{server_id}/plugins/market", {
      ...serverParams(serverId),
      body: { market_plugin_id: marketPluginId },
    })),
    /** The file is sent as it is; its name names the plugin. */
    uploadPlugin: (serverId: string, file: File) => apiRequest(client.POST("/api/v1/servers/{server_id}/plugins/uploads", {
      params: { path: { server_id: serverId }, query: { file_name: file.name } },
      // The contract types a binary body as a string; the browser sends the file's bytes.
      body: file as unknown as string,
      bodySerializer: (body) => body,
      headers: { "content-type": "application/wasm" },
    })),
    setPluginEnabled: (serverId: string, pluginId: string, enabled: boolean) => apiRequest(client.PATCH("/api/v1/servers/{server_id}/plugins/{plugin_id}", {
      ...pluginParams(serverId, pluginId),
      body: { enabled },
    })),
    removePlugin: (serverId: string, pluginId: string) => apiVoid(client.DELETE("/api/v1/servers/{server_id}/plugins/{plugin_id}", pluginParams(serverId, pluginId))),
    createWorldExport: (serverId: string) => apiRequest(client.POST("/api/v1/servers/{server_id}/exports", serverParams(serverId))),
    cancelWorldExport: (serverId: string, exportId: string) => apiRequest(client.DELETE("/api/v1/servers/{server_id}/exports/{export_id}", {
      params: { path: { server_id: serverId, export_id: exportId } },
    })),
    setDefaultWorld: (serverId: string) => apiVoid(client.PUT("/api/v1/account/worlds/{server_id}/default", {
      params: { path: { server_id: serverId } },
    })),
    revokeAccountSession: (sessionId: string) => apiVoid(client.DELETE("/api/v1/account/sessions/{session_id}", {
      params: { path: { session_id: sessionId } },
    })),
    revokeServerSession: (sessionId: string) => apiVoid(client.DELETE("/api/v1/server/sessions/{session_id}", {
      params: { path: { session_id: sessionId } },
    })),
    logout: () => apiVoid(client.POST("/auth/logout")),
    redeemMinecraft: (token: string) => apiVoid(client.POST("/auth/minecraft/redeem", { body: { token } })),
    redeemServer: (token: string) => apiRequest(client.POST("/auth/server/redeem", { body: { token } })),
  };
}

export const browserClient = createClient<paths>({
  baseUrl: typeof window === "undefined" ? "" : window.location.origin,
  credentials: "include",
  headers: { accept: "application/json" },
  fetch: (request) => globalThis.fetch(request),
});
export const browserApi = apiOperations(browserClient);
