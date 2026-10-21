export const queryKeys = {
  session: ["session"] as const,
  account: ["account"] as const,
  servers: ["servers"] as const,
  serverAddress: (subdomain: string) => ["server-addresses", subdomain] as const,
  serverSessions: ["server-sessions"] as const,
  server: (serverId: string) => ["servers", serverId] as const,
  activity: (serverId: string) => ["servers", serverId, "activity"] as const,
  logs: (serverId: string) => ["servers", serverId, "logs"] as const,
  snapshots: (serverId: string) => ["servers", serverId, "snapshots"] as const,
  players: (serverId: string) => ["servers", serverId, "players"] as const,
  whitelist: (serverId: string) => ["servers", serverId, "whitelist"] as const,
  operators: (serverId: string) => ["servers", serverId, "operators"] as const,
  exports: (serverId: string) => ["servers", serverId, "exports"] as const,
  ai: (serverId: string) => ["servers", serverId, "ai"] as const,
  aiJobs: (serverId: string) => ["servers", serverId, "ai", "jobs"] as const,
  aiJob: (serverId: string, jobId: string) => ["servers", serverId, "ai", "jobs", jobId] as const,
  plugins: (serverId: string) => ["servers", serverId, "plugins"] as const,
  pluginMarket: (serverId: string) => ["servers", serverId, "plugins", "market"] as const,
  pluginMarketSearch: (serverId: string, query: string) => ["servers", serverId, "plugins", "market", query] as const,
  eventStatus: (serverId: string) => ["servers", serverId, "event-status"] as const,
  admin: {
    overview: ["admin", "overview"] as const,
    fleet: (range: string) => ["admin", "fleet", range] as const,
    servers: (query: object) => ["admin", "servers", query] as const,
    server: (serverId: string, range: string) => ["admin", "servers", serverId, range] as const,
    accounts: (query: object) => ["admin", "accounts", query] as const,
    account: (accountId: string) => ["admin", "accounts", accountId] as const,
    workers: ["admin", "workers"] as const,
    failures: (range: string) => ["admin", "failures", range] as const,
    ci: ["admin", "ci"] as const,
    buildLog: (buildId: string) => ["admin", "ci", "builds", buildId] as const,
    ai: ["admin", "ai"] as const,
    growth: ["admin", "growth"] as const,
  },
};

export function selectedServerId(pathname: string) {
  const match = pathname.match(/^\/manage\/([^/]+)/);
  return match ? decodeURIComponent(match[1]) : null;
}
