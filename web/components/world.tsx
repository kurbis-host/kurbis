"use client";

import * as Dialog from "@radix-ui/react-dialog";
import { useQuery } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Copy, Plus, RotateCcw, Trash2 } from "lucide-react";
import { useState } from "react";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import { Button, controlStyles, Field, Notice, SwitchField } from "./ui";

type WorldGeneration = components["schemas"]["WorldGeneration"];
type NewWorld = components["schemas"]["NewWorld"];
type WorldType = components["schemas"]["WorldType"];
type FlatSettings = components["schemas"]["FlatSettings"];
type WorldCatalog = components["schemas"]["WorldCatalog"];
export type ResetMode = components["schemas"]["ResetMode"];

/** Vanilla's limits on the superflat screen and the seed field. */
const MAX_FLAT_LAYERS = 64;
const MAX_FLAT_HEIGHT = 384;
const MAX_SEED_LENGTH = 32;

const WORLD_TYPE_HELP: Record<WorldType, string> = {
  normal: "Vanilla terrain with every biome.",
  flat: "Flat layers you choose, from a preset or your own.",
  large_biomes: "Vanilla terrain with biomes four times as large.",
  amplified: "Vanilla terrain stretched into towering mountains. Heavier on the server.",
  single_biome_surface: "Vanilla terrain covered by one biome you choose.",
};

export function useWorldCatalog() {
  return useQuery({
    queryKey: queryKeys.worldCatalog,
    queryFn: browserApi.getWorldCatalog,
    staleTime: Infinity,
  });
}

function nameOf(entries: { id: string; name: string }[] | undefined, id: string) {
  return entries?.find((entry) => entry.id === id)?.name ?? id.replace(/^minecraft:/, "");
}

/** The id of the vanilla preset these layers are, if they are one. */
export function matchingPreset(catalog: WorldCatalog, flat: FlatSettings) {
  return catalog.flat_presets.find((preset) => JSON.stringify(preset.settings) === JSON.stringify(flat))?.id;
}

export function describeWorld(world: WorldGeneration, catalog: WorldCatalog | undefined) {
  const type = nameOf(catalog?.world_types, world.world_type);
  if (world.flat) {
    const preset = catalog && matchingPreset(catalog, world.flat);
    return `${type}, ${preset ? nameOf(catalog.flat_presets, preset) : "custom layers"}`;
  }
  if (world.biome) return `${type}, ${nameOf(catalog?.biomes, world.biome)}`;
  return type;
}

export function CurrentWorld({ world }: { world: WorldGeneration }) {
  const { data: catalog } = useWorldCatalog();
  const [copied, setCopied] = useState(false);
  const seed = world.seed === null || world.seed === undefined ? null : String(world.seed);
  return (
    <dl className="mt-[17px] mb-0 grid gap-2 rounded-[9px] bg-[#eee7d8] p-3 text-xs">
      <div className="flex items-center justify-between gap-3">
        <dt>World type</dt>
        <dd className="m-0 text-right font-[760]">{describeWorld(world, catalog)}</dd>
      </div>
      <div className="flex items-center justify-between gap-3">
        <dt>Structures</dt>
        <dd className="m-0 font-[760]">{world.structures ? "On" : "Off"}</dd>
      </div>
      <div className="flex items-center justify-between gap-3">
        <dt>Seed</dt>
        <dd className="m-0 flex items-center gap-1.5 font-[760]">
          {seed ? (
            <>
              <code className="font-mono">{seed}</code>
              <Button
                type="button"
                variant="quiet"
                size="small"
                className="min-h-7 px-1.5"
                readSafe
                aria-label="Copy seed"
                onClick={async () => {
                  await navigator.clipboard.writeText(seed);
                  setCopied(true);
                  window.setTimeout(() => setCopied(false), 1200);
                }}
              >
                <Copy size={13} />
              </Button>
              {copied ? <span className="font-normal text-ink-soft">Copied</span> : null}
            </>
          ) : (
            <span className="font-normal text-ink-soft">Shown once the world next runs</span>
          )}
        </dd>
      </div>
    </dl>
  );
}

function startingWorld(): NewWorld {
  return { world_type: "normal", structures: true };
}

/** The request for a draft: only the choices that belong to its world type. */
export function newWorldRequest(draft: NewWorld): NewWorld {
  const seed = draft.seed?.trim();
  return {
    world_type: draft.world_type,
    structures: draft.structures,
    ...(draft.world_type === "flat" && draft.flat ? { flat: draft.flat } : {}),
    ...(draft.world_type === "single_biome_surface" && draft.biome ? { biome: draft.biome } : {}),
    ...(seed ? { seed } : {}),
  };
}

export function flatProblem(flat: FlatSettings | null | undefined, catalog: WorldCatalog) {
  if (!flat) return null;
  if (flat.layers.length === 0) return "Add at least one layer.";
  const unknown = flat.layers.find((layer) => !catalog.blocks.some((block) => block.id === layer.block));
  if (unknown) return `${unknown.block || "An empty layer"} is not a block. Pick one from the list.`;
  if (flat.layers.length > MAX_FLAT_LAYERS) return `A superflat world has at most ${MAX_FLAT_LAYERS} layers.`;
  if (flat.layers.some((layer) => !Number.isInteger(layer.height) || layer.height < 1)) {
    return "Every layer is at least one block high.";
  }
  const height = flat.layers.reduce((total, layer) => total + layer.height, 0);
  if (height > MAX_FLAT_HEIGHT) return `The layers are ${height} blocks high; the world has room for ${MAX_FLAT_HEIGHT}.`;
  return null;
}

export function ResetWorldDialog({
  mode,
  disabled,
  onReset,
}: {
  mode: ResetMode;
  disabled: boolean;
  onReset: (mode: ResetMode, world: NewWorld) => Promise<unknown>;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<NewWorld>(startingWorld);
  const { data: catalog, error: catalogError } = useWorldCatalog();
  const full = mode === "full";
  const problem = draft.world_type === "flat" && catalog ? flatProblem(draft.flat, catalog) : null;
  const update = (change: Partial<NewWorld>) => setDraft((current) => ({ ...current, ...change }));

  const chooseType = (worldType: WorldType) => {
    if (!catalog) return;
    update({
      world_type: worldType,
      flat: worldType === "flat" ? (draft.flat ?? catalog.flat_presets[0]!.settings) : draft.flat,
      biome: worldType === "single_biome_surface" ? (draft.biome ?? "minecraft:plains") : draft.biome,
    });
  };

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) setDraft(startingWorld());
      }}
    >
      <Dialog.Trigger asChild disabled={disabled}>
        <Button variant={full ? "danger" : "secondary"} disabled={disabled}>
          <RotateCcw size={15} />
          {full ? "Full server reset" : "World reset"}
        </Button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-80 animate-[fade-in_140ms_ease-out] bg-[rgba(35,31,25,0.56)]" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-81 max-h-[calc(100dvh-32px)] w-[min(620px,calc(100vw-32px))] -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-2xl bg-paper p-6 shadow-high">
          <Dialog.Title className="font-display text-[23px] leading-[1.15] font-[800]">
            {full ? "Reset the full server?" : "Reset world progress?"}
          </Dialog.Title>
          <Dialog.Description className="mt-2.5 leading-[1.55] text-ink-soft">
            {full
              ? "This generates a new world and also restores settings to defaults and clears added whitelist, ban, and operator access. Ownership, address, and derived owner privileges remain. A pre-reset backup is created first."
              : "This generates a new world and clears every dimension, inventory, advancement, and player progress while preserving server settings, whitelist, bans, and operator access. A pre-reset backup is created first."}
          </Dialog.Description>

          {catalogError ? (
            <div className="mt-5">
              <Notice tone="danger">The world types could not be loaded. Close this and try again.</Notice>
            </div>
          ) : !catalog ? (
            <p className="mt-5 mb-0 text-sm text-ink-soft">Loading world types…</p>
          ) : (
            <div className="mt-5 grid gap-[18px]">
              <Field label="World type" help={WORLD_TYPE_HELP[draft.world_type]}>
                <select
                  className={controlStyles.select}
                  aria-label="World type"
                  value={draft.world_type}
                  onChange={(event) => chooseType(event.target.value as WorldType)}
                >
                  {catalog.world_types.map((type) => (
                    <option key={type.id} value={type.id}>
                      {type.name}
                    </option>
                  ))}
                </select>
              </Field>

              {draft.world_type === "single_biome_surface" ? (
                <Field label="Biome">
                  <select
                    className={controlStyles.select}
                    aria-label="Biome"
                    value={draft.biome ?? "minecraft:plains"}
                    onChange={(event) => update({ biome: event.target.value })}
                  >
                    {catalog.biomes.map((biome) => (
                      <option key={biome.id} value={biome.id}>
                        {biome.name}
                      </option>
                    ))}
                  </select>
                </Field>
              ) : null}

              {draft.world_type === "flat" && draft.flat ? (
                <FlatEditor catalog={catalog} flat={draft.flat} onChange={(flat) => update({ flat })} problem={problem} />
              ) : null}

              <SwitchField
                label="Generate structures"
                description={
                  draft.world_type === "flat"
                    ? "Villages and the other structures this superflat world allows."
                    : "Villages, strongholds, temples and every other structure."
                }
                checked={draft.structures}
                onCheckedChange={(structures) => update({ structures })}
              />

              <Field
                label="Seed"
                help="Leave empty for a random seed. A number, or any text, as in Minecraft."
              >
                <input
                  className={controlStyles.input}
                  aria-label="Seed"
                  maxLength={MAX_SEED_LENGTH}
                  placeholder="Random"
                  value={draft.seed ?? ""}
                  onChange={(event) => update({ seed: event.target.value })}
                />
              </Field>
            </div>
          )}

          <div className="mt-6 flex justify-end gap-[9px] max-sm:flex-col-reverse max-sm:[&>button]:w-full">
            <Dialog.Close asChild>
              <Button variant="secondary" disabled={busy}>
                Cancel
              </Button>
            </Dialog.Close>
            <Button
              variant={full ? "danger" : "primary"}
              disabled={busy || !catalog || problem !== null}
              onClick={async () => {
                setBusy(true);
                try {
                  await onReset(mode, newWorldRequest(draft));
                  setOpen(false);
                } catch {
                  // The settings page shows why the reset was refused.
                } finally {
                  setBusy(false);
                }
              }}
            >
              {busy ? "Working…" : full ? "Reset full server" : "Reset world"}
            </Button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function FlatEditor({
  catalog,
  flat,
  onChange,
  problem,
}: {
  catalog: WorldCatalog;
  flat: FlatSettings;
  onChange: (flat: FlatSettings) => void;
  problem: string | null;
}) {
  const preset = matchingPreset(catalog, flat);
  // Shown top first, as on vanilla's superflat screen; stored bottom first.
  const layers = flat.layers.map((layer, index) => ({ layer, index })).reverse();
  const setLayers = (next: FlatSettings["layers"]) => onChange({ ...flat, layers: next });
  const move = (index: number, by: number) => {
    const next = [...flat.layers];
    const [layer] = next.splice(index, 1);
    next.splice(index + by, 0, layer!);
    setLayers(next);
  };
  const total = flat.layers.reduce((sum, layer) => sum + (Number.isFinite(layer.height) ? layer.height : 0), 0);

  return (
    <div className="grid gap-[14px] rounded-[9px] bg-[#f3eee3] p-[14px] shadow-[inset_0_0_0_1px_#ddd4c3]">
      <Field label="Preset" help="Pick a vanilla preset, then change any layer below.">
        <select
          className={controlStyles.select}
          aria-label="Preset"
          value={preset ?? "custom"}
          onChange={(event) => {
            const chosen = catalog.flat_presets.find((item) => item.id === event.target.value);
            if (chosen) onChange(chosen.settings);
          }}
        >
          {catalog.flat_presets.map((item) => (
            <option key={item.id} value={item.id}>
              {item.name}
            </option>
          ))}
          {preset ? null : <option value="custom">Custom</option>}
        </select>
      </Field>

      <div className="grid gap-2">
        <div className="flex items-baseline justify-between gap-3">
          <span className="text-[13px] font-[760] text-[#4d4b42]">Layers, top first; layer 1 is the bottom</span>
          <span className="text-xs text-ink-soft">
            {total} of {MAX_FLAT_HEIGHT} blocks high
          </span>
        </div>
        <datalist id="flat-layer-blocks">
          {catalog.blocks.map((block) => (
            <option key={block.id} value={block.id}>
              {block.name}
            </option>
          ))}
        </datalist>
        <ol className="m-0 grid list-none gap-1.5 p-0">
          {layers.map(({ layer, index }) => (
            <li key={index} className="grid grid-cols-[minmax(0,1fr)_84px_auto] items-center gap-1.5">
              <input
                className={controlStyles.input}
                aria-label={`Layer ${index + 1} block`}
                list="flat-layer-blocks"
                value={layer.block}
                onChange={(event) =>
                  setLayers(flat.layers.map((item, at) => (at === index ? { ...item, block: event.target.value } : item)))
                }
              />
              <input
                className={controlStyles.input}
                aria-label={`Layer ${index + 1} height`}
                type="number"
                min={1}
                max={MAX_FLAT_HEIGHT}
                value={Number.isFinite(layer.height) ? layer.height : ""}
                onChange={(event) =>
                  setLayers(flat.layers.map((item, at) => (at === index ? { ...item, height: event.target.valueAsNumber } : item)))
                }
              />
              <span className="flex gap-1">
                <Button type="button" variant="quiet" size="small" aria-label="Move layer up" disabled={index === flat.layers.length - 1} onClick={() => move(index, 1)}>
                  <ArrowUp size={14} />
                </Button>
                <Button type="button" variant="quiet" size="small" aria-label="Move layer down" disabled={index === 0} onClick={() => move(index, -1)}>
                  <ArrowDown size={14} />
                </Button>
                <Button type="button" variant="quiet" size="small" aria-label="Remove layer" onClick={() => setLayers(flat.layers.filter((_, at) => at !== index))}>
                  <Trash2 size={14} />
                </Button>
              </span>
            </li>
          ))}
        </ol>
        <Button
          type="button"
          variant="secondary"
          size="small"
          className="justify-self-start"
          disabled={flat.layers.length >= MAX_FLAT_LAYERS}
          onClick={() => setLayers([...flat.layers, { block: "minecraft:stone", height: 1 }])}
        >
          <Plus size={14} />
          Add layer on top
        </Button>
        {problem ? (
          <span className="text-xs leading-[1.35] text-danger" role="alert">
            {problem}
          </span>
        ) : null}
      </div>

      <Field label="Biome">
        <select
          className={controlStyles.select}
          aria-label="Superflat biome"
          value={flat.biome}
          onChange={(event) => onChange({ ...flat, biome: event.target.value })}
        >
          {catalog.biomes.map((biome) => (
            <option key={biome.id} value={biome.id}>
              {biome.name}
            </option>
          ))}
        </select>
      </Field>
      <SwitchField
        label="Decorations"
        description="The biome's trees, flowers and ores."
        checked={flat.features}
        onCheckedChange={(features) => onChange({ ...flat, features })}
      />
      <SwitchField
        label="Lava lakes"
        description="Lakes of lava scattered over the surface."
        checked={flat.lakes}
        onCheckedChange={(lakes) => onChange({ ...flat, lakes })}
      />
      <fieldset className="m-0 grid gap-2 border-0 p-0">
        <legend className="mb-2 p-0 text-[13px] font-[760] text-[#4d4b42]">Structures this world may generate</legend>
        <div className="grid grid-cols-2 gap-x-4 gap-y-1.5 max-[520px]:grid-cols-1">
          {catalog.structures.map((structure) => (
            <label key={structure.id} className="flex items-center gap-2 text-[13px]">
              <input
                type="checkbox"
                className="size-4 accent-grass"
                checked={flat.structure_overrides.includes(structure.id)}
                onChange={(event) =>
                  onChange({
                    ...flat,
                    structure_overrides: event.target.checked
                      ? catalog.structures.map((item) => item.id).filter((id) => id === structure.id || flat.structure_overrides.includes(id))
                      : flat.structure_overrides.filter((id) => id !== structure.id),
                  })
                }
              />
              {structure.name}
            </label>
          ))}
        </div>
      </fieldset>
    </div>
  );
}
