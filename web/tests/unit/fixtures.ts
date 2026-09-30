import type { Server, Session, Snapshot } from "../../lib/contracts";

export const snapshot: Snapshot = {
  generation: 4,
  reason: "shutdown",
  size_bytes: 256 * 1024 * 1024,
  created_at: "2026-09-30T18:00:00Z",
};

export const server: Server = {
  id: "server-1",
  subdomain: "melon-world",
  java_address: "melon-world.mc.example.test",
  bedrock_address: "melon-world.mc.example.test",
  desired_state: "stopped",
  observed_state: "sleeping",
  settings: {
    motd: "Hosted by Kurbis",
    difficulty: "Normal",
    gamemode: "Survival",
    view_distance: 12,
    simulation_distance: 8,
    hardcore: false,
    whitelist: false,
  },
  storage_bytes: 512 * 1024 * 1024,
  storage_limit_bytes: 5 * 1024 * 1024 * 1024,
  memory_limit_bytes: 2 * 1024 * 1024 * 1024,
  player_count: 0,
  created_at: "2026-09-29T18:00:00Z",
  updated_at: "2026-09-30T18:00:00Z",
  current_operation: null,
  latest_snapshot: snapshot,
  error: null,
};

export const session: Session = {
  user: {
    id: "user-1",
    email: "player@example.test",
    display_name: "Browser Player",
    account_status: "active",
  },
  limits: {
    memory_bytes: 2 * 1024 * 1024 * 1024,
    storage_bytes: 5 * 1024 * 1024 * 1024,
    idle_seconds: 300,
    player_limit: null,
  },
};
