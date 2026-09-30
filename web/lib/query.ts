export const queryKeys = {
  session: ["session"] as const,
  account: ["account"] as const,
  server: ["server"] as const,
  activity: ["server", "activity"] as const,
  logs: ["server", "logs"] as const,
  snapshots: ["server", "snapshots"] as const,
  players: ["server", "players"] as const,
  whitelist: ["server", "whitelist"] as const,
  eventStatus: ["server", "event-status"] as const,
};
