import { z } from "zod";

export const observedStateSchema = z.enum([
  "sleeping",
  "queued",
  "restoring",
  "starting",
  "online",
  "stopping",
  "snapshotting",
  "failed",
  "deleted",
]);
export type ObservedState = z.infer<typeof observedStateSchema>;

export const serverSettingsSchema = z.object({
  motd: z.string().max(120),
  difficulty: z.enum(["Peaceful", "Easy", "Normal", "Hard"]),
  gamemode: z.enum(["Survival", "Creative", "Adventure", "Spectator"]),
  view_distance: z.number().int().min(2).max(32),
  simulation_distance: z.number().int().min(2).max(32),
  hardcore: z.boolean(),
  whitelist: z.boolean(),
});
export type ServerSettings = z.infer<typeof serverSettingsSchema>;

export const userSchema = z.object({
  id: z.string(),
  email: z.string().email().nullable(),
  display_name: z.string().min(1),
  account_status: z.string(),
});
export type User = z.infer<typeof userSchema>;

export const serviceLimitsSchema = z.object({
  memory_bytes: z.number().nonnegative(),
  storage_bytes: z.number().nonnegative(),
  idle_seconds: z.number().int().nonnegative(),
  player_limit: z.number().int().positive().nullable(),
});
export type ServiceLimits = z.infer<typeof serviceLimitsSchema>;

export const sessionSchema = z.object({
  user: userSchema,
  limits: serviceLimitsSchema,
});
export type Session = z.infer<typeof sessionSchema>;

export const accountIdentitySchema = z.object({
  id: z.string(),
  provider: z.enum(["google", "discord", "minecraft_java", "minecraft_bedrock"]),
  display_name: z.string(),
  email: z.string().nullable(),
  email_verified: z.boolean(),
  created_at: z.string(),
});

export const accountSessionSchema = z.object({
  id: z.string(),
  created_at: z.string(),
  last_seen_at: z.string(),
  current: z.boolean(),
});

export const accountWorldSchema = z.object({
  id: z.string().uuid(),
  subdomain: z.string(),
  observed_state: observedStateSchema,
  is_default: z.boolean(),
  created_at: z.string(),
});

export const accountSchema = z.object({
  identities: z.array(accountIdentitySchema),
  sessions: z.array(accountSessionSchema),
  worlds: z.array(accountWorldSchema),
});
export type AccountData = z.infer<typeof accountSchema>;

export const operationSchema = z.object({
  id: z.string(),
  kind: z.enum(["start", "stop", "restart", "snapshot", "restore", "delete"]),
  status: z.enum(["pending", "running", "succeeded", "failed", "cancelled"]),
  stage: z.string().nullable(),
  message: z.string().nullable(),
  created_at: z.string(),
  updated_at: z.string(),
});
export type Operation = z.infer<typeof operationSchema>;

export const snapshotSchema = z.object({
  generation: z.number().int().nonnegative(),
  reason: z.enum(["periodic", "shutdown", "manual", "restore"]),
  size_bytes: z.number().nonnegative(),
  created_at: z.string(),
});
export type Snapshot = z.infer<typeof snapshotSchema>;

export const serverErrorSchema = z.object({
  code: z.string(),
  message: z.string(),
  detail: z.string().nullable(),
  retryable: z.boolean(),
});
export type ServerError = z.infer<typeof serverErrorSchema>;

export const serverSchema = z.object({
  id: z.string(),
  subdomain: z.string(),
  java_address: z.string(),
  bedrock_address: z.string(),
  desired_state: z.enum(["running", "stopped", "deleted"]),
  observed_state: observedStateSchema,
  settings: serverSettingsSchema,
  storage_bytes: z.number().nonnegative(),
  storage_limit_bytes: z.number().positive(),
  memory_limit_bytes: z.number().positive(),
  player_count: z.number().int().nonnegative(),
  created_at: z.string(),
  updated_at: z.string(),
  current_operation: operationSchema.nullable(),
  latest_snapshot: snapshotSchema.nullable(),
  error: serverErrorSchema.nullable(),
});
export type Server = z.infer<typeof serverSchema>;

export const logEntrySchema = z.object({
  id: z.number().int().nonnegative(),
  created_at: z.string(),
  line: z.string(),
});
export type LogEntry = z.infer<typeof logEntrySchema>;

export const logPageSchema = z.object({
  items: z.array(logEntrySchema),
  next_after_id: z.number().int().nonnegative().nullable(),
});
export type LogPage = z.infer<typeof logPageSchema>;

export const activitySchema = z.object({
  id: z.union([z.string(), z.number()]),
  type: z.string(),
  summary: z.string(),
  detail: z.string().nullable().optional(),
  status: z.string().nullable().optional(),
  created_at: z.string(),
});
export type Activity = z.infer<typeof activitySchema>;
export const activityListSchema = z.array(activitySchema);

export const playerPresenceSchema = z.object({
  player_id: z.string(),
  name: z.string(),
  edition: z.enum(["java", "bedrock"]),
  connected_at: z.string(),
  last_seen_at: z.string(),
});
export type PlayerPresence = z.infer<typeof playerPresenceSchema>;
export const playerListSchema = z.array(playerPresenceSchema);

export const whitelistEntrySchema = z.object({
  player_id: z.string(),
  name: z.string(),
  created_at: z.string(),
  updated_at: z.string(),
});
export type WhitelistEntry = z.infer<typeof whitelistEntrySchema>;
export const whitelistListSchema = z.object({
  enabled: z.boolean(),
  revision: z.number().int().nonnegative(),
  applied_revision: z.number().int().nonnegative(),
  synchronized: z.boolean(),
  entries: z.array(whitelistEntrySchema),
});
export type WhitelistState = z.infer<typeof whitelistListSchema>;
export const snapshotListSchema = z.array(snapshotSchema);

export const serverEventSchema = z.discriminatedUnion("type", [
  z.object({ id: z.number().int().nonnegative(), type: z.literal("server"), data: serverSchema.optional() }),
  z.object({ id: z.number().int().nonnegative(), type: z.literal("operation"), data: operationSchema.optional() }),
  z.object({ id: z.number().int().nonnegative(), type: z.literal("snapshot"), data: snapshotSchema.optional() }),
  z.object({ id: z.number().int().nonnegative(), type: z.literal("players"), data: playerListSchema.optional() }),
  z.object({ id: z.number().int().nonnegative(), type: z.literal("whitelist"), data: whitelistListSchema.optional() }),
  z.object({ id: z.number().int().nonnegative(), type: z.literal("log"), data: logEntrySchema.optional() }),
]);
export type ServerEvent = z.infer<typeof serverEventSchema>;
export const optionalServerSchema = serverSchema.nullable();
