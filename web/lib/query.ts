export const queryKeys = {
  session: ["session"] as const,
  account: ["account"] as const,
  servers: ["servers"] as const,
  serverSessions: ["server-sessions"] as const,
  server: (serverId: string) => ["servers", serverId] as const,
  activity: (serverId: string) => ["servers", serverId, "activity"] as const,
  logs: (serverId: string) => ["servers", serverId, "logs"] as const,
  snapshots: (serverId: string) => ["servers", serverId, "snapshots"] as const,
  players: (serverId: string) => ["servers", serverId, "players"] as const,
  whitelist: (serverId: string) => ["servers", serverId, "whitelist"] as const,
  exports: (serverId: string) => ["servers", serverId, "exports"] as const,
  eventStatus: (serverId: string) => ["servers", serverId, "event-status"] as const,
};

export function selectedServerId(pathname: string) {
  const match = pathname.match(/^\/manage\/([^/]+)/);
  return match ? decodeURIComponent(match[1]) : null;
}
