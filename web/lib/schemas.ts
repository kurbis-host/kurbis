import { z } from "zod";

export const subdomainSchema = z
  .string()
  .min(3, "Use at least 3 characters.")
  .max(32, "Use at most 32 characters.")
  .regex(/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/, "Use lowercase letters, numbers, or dashes; do not start or end with a dash.");

export const setupSchema = z.object({
  subdomain: subdomainSchema,
  preset: z.enum(["survival", "creative"]),
  motd: z.string().max(120, "Keep the MOTD under 120 characters."),
  difficulty: z.enum(["Peaceful", "Easy", "Normal", "Hard"]),
  gamemode: z.enum(["Survival", "Creative", "Adventure", "Spectator"]),
  view_distance: z.number().int().min(2).max(32),
  simulation_distance: z.number().int().min(2).max(32),
  hardcore: z.boolean(),
  whitelist: z.boolean(),
  tab_stats: z.boolean(),
});
export type SetupValues = z.infer<typeof setupSchema>;

export const settingsFormSchema = setupSchema.omit({ preset: true });
export type SettingsFormValues = z.infer<typeof settingsFormSchema>;

const playerNameSchema = z
  .string()
  .trim()
  .min(1, "Enter a player name.")
  .max(64, "Player names are limited to 64 characters.")
  .refine((name) => [...name].every((character) => !/\p{Cc}/u.test(character)), "Player names cannot contain control characters.");

/** Names are the source of truth for panel mutations; the API resolves the
 * verified game profile or reports when a Bedrock player must reconnect first. */
export const playerMutationSchema = z.object({ name: playerNameSchema });
export type PlayerMutationValues = z.infer<typeof playerMutationSchema>;

export const whitelistFormSchema = playerMutationSchema;
export type WhitelistFormValues = PlayerMutationValues;
