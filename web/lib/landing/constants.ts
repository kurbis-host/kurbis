export const ROOT_DOMAIN = "kurbis.host";

export type LayerKey =
  | "sky"
  | "surface"
  | "dirt"
  | "cave"
  | "ores"
  | "spawner"
  | "chest"
  | "diamonds"
  | "bedrock"
  | "nether";

export interface Layer {
  key: LayerKey;
  /** Y level shown on the meter. */
  y: number;
  /** Camera height in world units while this layer is centred. */
  worldY: number;
  /** Where this layer sits on the minimap, 0 (top) to 1 (bottom). */
  map: number;
  /** Achievement toast on first arrival. */
  toast?: string;
}

export const LAYERS: readonly Layer[] = [
  { key: "sky", y: 320, worldY: 12, map: 0.06 },
  { key: "surface", y: 64, worldY: 1, map: 0.25, toast: "Touched grass" },
  { key: "dirt", y: 50, worldY: -9, map: 0.33, toast: "Dirt enthusiast" },
  { key: "cave", y: 20, worldY: -22, map: 0.43, toast: "Found the cave" },
  { key: "ores", y: 0, worldY: -34, map: 0.5, toast: "Oh, shiny" },
  { key: "spawner", y: -10, worldY: -44.5, map: 0.56, toast: "Spooky" },
  { key: "chest", y: -20, worldY: -52, map: 0.6, toast: "Treasure hunter" },
  { key: "diamonds", y: -40, worldY: -61, map: 0.67, toast: "Diamonds!" },
  { key: "bedrock", y: -64, worldY: -69, map: 0.76, toast: "Rock bottom" },
  { key: "nether", y: -80, worldY: -84, map: 0.9, toast: "We need to go deeper" },
] as const;

export const LAYER_IDS = LAYERS.map((layer) => `layer-${layer.key}`);

export const HOTBAR: readonly { label: string; layer: LayerKey }[] = [
  { label: "Sky", layer: "sky" },
  { label: "Surface", layer: "surface" },
  { label: "Dirt", layer: "dirt" },
  { label: "Cave", layer: "cave" },
  { label: "Ores", layer: "ores" },
  { label: "Spawner", layer: "spawner" },
  { label: "Chest", layer: "chest" },
  { label: "Diamonds", layer: "diamonds" },
  { label: "Nether", layer: "nether" },
] as const;

/** Seconds of page idle that stand in for the five real minutes. */
export const IDLE_SECONDS = 12;
