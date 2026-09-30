export type ObservedState =
  | "sleeping"
  | "queued"
  | "restoring"
  | "starting"
  | "online"
  | "stopping"
  | "snapshotting"
  | "failed"
  | "deleted";

export type DesiredState = "running" | "stopped" | "deleted";

export type User = {
  id: string;
  email: string | null;
  display_name: string;
  account_status: string;
};

export type Settings = {
  motd?: string | null;
  difficulty?: "Peaceful" | "Easy" | "Normal" | "Hard" | null;
  gamemode?: "Survival" | "Creative" | "Adventure" | "Spectator" | null;
  view_distance?: number | null;
  simulation_distance?: number | null;
  hardcore?: boolean | null;
  whitelist?: boolean | null;
};

export type Server = {
  id: string;
  subdomain: string;
  java_address: string;
  bedrock_address: string;
  desired_state: DesiredState;
  observed_state: ObservedState;
  settings: Settings;
  storage_bytes: number;
  storage_limit_bytes: number;
  memory_limit_bytes: number;
  last_error: string | null;
};

export type LogLine = {
  id: number;
  created_at: string;
  line: string;
};

const API_BASE = (process.env.NEXT_PUBLIC_CONTROL_PLANE_URL ?? "").replace(
  /\/$/,
  "",
);

export function apiUrl(path: string) {
  return `${API_BASE}${path}`;
}

export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
  ) {
    super(message);
  }
}

export async function api<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  const response = await fetch(apiUrl(path), {
    ...init,
    credentials: "include",
    headers: {
      ...(init?.body ? { "content-type": "application/json" } : {}),
      ...init?.headers,
    },
  });
  if (!response.ok) {
    let message = `Request failed (${response.status})`;
    try {
      const payload = (await response.json()) as { error?: string };
      if (payload.error) message = payload.error;
    } catch {
      // Keep the status fallback when the response has no JSON body.
    }
    throw new ApiError(message, response.status);
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}
