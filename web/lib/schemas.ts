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
});
export type SetupValues = z.infer<typeof setupSchema>;

export const settingsFormSchema = setupSchema.omit({ preset: true });
export type SettingsFormValues = z.infer<typeof settingsFormSchema>;

export const whitelistFormSchema = z.object({
  player_id: z.string().uuid("Enter a valid Minecraft player UUID."),
  name: z.string().trim().min(1, "Enter a player name.").max(64, "Player names are limited to 64 characters."),
});
export type WhitelistFormValues = z.infer<typeof whitelistFormSchema>;
