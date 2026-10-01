"use client";

import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RotateCcw, Save, Square, Trash2 } from "lucide-react";
import { useRouter } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { apiRequest, apiVoid, errorMessage, jsonBody } from "../lib/api/client";
import { fetchServer } from "../lib/api/queries";
import { serverSchema, type Server } from "../lib/contracts";
import { lifecycle, lifecycleActions } from "../lib/lifecycle";
import { queryKeys } from "../lib/query";
import { settingsFormSchema, type SettingsFormValues } from "../lib/schemas";
import {
  Button,
  ConfirmDialog,
  controlStyles,
  Field,
  Notice,
  PageHeader,
  Surface,
  SwitchField,
} from "./ui";

export function serverToSettingsValues(server: Server): SettingsFormValues {
  return {
    subdomain: server.subdomain,
    motd: server.settings.motd,
    difficulty: server.settings.difficulty,
    gamemode: server.settings.gamemode,
    view_distance: server.settings.view_distance,
    simulation_distance: server.settings.simulation_distance,
    hardcore: server.settings.hardcore,
    whitelist: server.settings.whitelist,
  };
}

export function Settings({ initialServer, afterDeleteHref = "/app/setup" }: { initialServer: Server; afterDeleteHref?: string }) {
  const queryClient = useQueryClient();
  const router = useRouter();
  const serverId = initialServer.id;
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(serverId),
    queryFn: () => fetchServer(serverId),
    initialData: initialServer,
  });
  if (!server) throw new Error("Server disappeared while settings were open.");

  const form = useForm<SettingsFormValues>({
    resolver: zodResolver(settingsFormSchema),
    defaultValues: serverToSettingsValues(initialServer),
  });
  const {
    register,
    control,
    reset: resetForm,
    handleSubmit,
    formState: { errors, isDirty, isSubmitting },
  } = form;
  const previousServerKey = useRef(
    JSON.stringify(serverToSettingsValues(initialServer)),
  );
  const [requestError, setRequestError] = useState<string | null>(null);
  const allowed = lifecycleActions(server);

  useEffect(() => {
    const nextValues = serverToSettingsValues(server);
    const nextKey = JSON.stringify(nextValues);
    if (nextKey !== previousServerKey.current && !isDirty) {
      previousServerKey.current = nextKey;
      resetForm(nextValues);
    }
  }, [server, isDirty, resetForm]);

  const stop = useMutation({
    mutationFn: () =>
      apiVoid(`/api/v1/servers/${serverId}/actions`, {
        method: "POST",
        body: jsonBody({ action: "stop" }),
      }),
    onSuccess: () =>
      void queryClient.invalidateQueries({ queryKey: queryKeys.server(serverId) }),
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const reset = useMutation({
    mutationFn: (mode: "world" | "full") => apiVoid(`/api/v1/servers/${serverId}/reset`, {
      method: "POST",
      body: jsonBody({ mode }),
    }),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.server(serverId) }),
        queryClient.invalidateQueries({ queryKey: queryKeys.snapshots(serverId) }),
      ]);
    },
    onError: (error) => setRequestError(errorMessage(error)),
  });

  const save = handleSubmit(async (values) => {
    setRequestError(null);
    try {
      const { subdomain, ...settings } = values;
      const updated = await apiRequest(`/api/v1/servers/${serverId}`, serverSchema, {
        method: "PATCH",
        body: jsonBody({ subdomain, settings }),
      });
      queryClient.setQueryData(queryKeys.server(serverId), updated);
      previousServerKey.current = JSON.stringify(
        serverToSettingsValues(updated),
      );
      resetForm(serverToSettingsValues(updated));
    } catch (error) {
      setRequestError(errorMessage(error));
    }
  });

  const remove = async () => {
    setRequestError(null);
    try {
      await apiVoid(`/api/v1/servers/${serverId}`, { method: "DELETE" });
      await queryClient.invalidateQueries({ queryKey: queryKeys.servers });
      router.replace(afterDeleteHref);
    } catch (error) {
      setRequestError(errorMessage(error));
    }
  };

  const disabled = !allowed.canEdit;

  return (
    <>
      <PageHeader
        title="World settings"
        description="Curated Pumpkin configuration and the server address. Drafts stay local while live server state continues updating."
      />
      {requestError ? (
        <div className="mb-[18px]">
          <Notice tone="danger">{requestError}</Notice>
        </div>
      ) : null}
      {!allowed.canEdit ? (
        <div className="mb-[18px]">
          <Notice tone="warning">
            Configuration is locked while the world is{" "}
            {lifecycle[server.observed_state].label.toLowerCase()}. Your current
            draft is preserved.
          </Notice>
        </div>
      ) : null}

      <div className="grid grid-cols-[minmax(0,1.25fr)_minmax(280px,0.75fr)] items-start gap-[18px] max-[900px]:grid-cols-1">
        <Surface className="p-[clamp(22px,4vw,32px)]">
          <form
            className="grid grid-cols-2 gap-[18px] max-[620px]:grid-cols-1"
            onSubmit={save}
            noValidate
          >
            <div className="col-[1/-1] max-[620px]:col-auto">
              <Field
                label="Address"
                error={errors.subdomain?.message}
                help="Changing the address takes effect the next time the world wakes."
              >
                <input
                  className={controlStyles.input}
                  disabled={disabled}
                  aria-label="Address"
                  {...register("subdomain", {
                    onChange: (event) => {
                      event.target.value = event.target.value
                        .toLowerCase()
                        .replace(/[^a-z0-9-]/g, "");
                    },
                  })}
                />
              </Field>
            </div>
            <div className="col-[1/-1] max-[620px]:col-auto">
              <Field label="MOTD" error={errors.motd?.message}>
                <input
                  className={controlStyles.input}
                  disabled={disabled}
                  aria-label="MOTD"
                  {...register("motd")}
                />
              </Field>
            </div>

            <Field label="Difficulty" error={errors.difficulty?.message}>
              <select
                className={controlStyles.select}
                disabled={disabled}
                aria-label="Difficulty"
                {...register("difficulty")}
              >
                <option>Peaceful</option>
                <option>Easy</option>
                <option>Normal</option>
                <option>Hard</option>
              </select>
            </Field>
            <Field label="Gamemode" error={errors.gamemode?.message}>
              <select
                className={controlStyles.select}
                disabled={disabled}
                aria-label="Gamemode"
                {...register("gamemode")}
              >
                <option>Survival</option>
                <option>Creative</option>
                <option>Adventure</option>
                <option>Spectator</option>
              </select>
            </Field>

            <div className="col-[1/-1] mt-2.5 border-t border-[#ddd4c3] pt-5 max-[620px]:col-auto">
              <h2 className="m-0 font-display text-[19px] leading-[1.2] font-[800]">
                World distances
              </h2>
              <p className="mt-[5px] mb-0 text-xs leading-[1.45] text-ink-soft">
                Higher values expose more of the world but increase runtime
                work.
              </p>
            </div>
            <Field label="View distance" error={errors.view_distance?.message}>
              <input
                className={controlStyles.input}
                type="number"
                min={2}
                max={32}
                disabled={disabled}
                {...register("view_distance", { valueAsNumber: true })}
              />
            </Field>
            <Field
              label="Simulation distance"
              error={errors.simulation_distance?.message}
            >
              <input
                className={controlStyles.input}
                type="number"
                min={2}
                max={32}
                disabled={disabled}
                {...register("simulation_distance", { valueAsNumber: true })}
              />
            </Field>

            <div className="col-[1/-1] grid grid-cols-2 gap-x-[26px] gap-y-2.5 max-[620px]:col-auto max-[620px]:grid-cols-1">
              <Controller
                control={control}
                name="hardcore"
                render={({ field }) => (
                  <SwitchField
                    label="Hardcore"
                    description="Enable single-life world rules."
                    checked={field.value}
                    onCheckedChange={field.onChange}
                    disabled={disabled}
                  />
                )}
              />
              <Controller
                control={control}
                name="whitelist"
                render={({ field }) => (
                  <SwitchField
                    label="Whitelist"
                    description="Only players on the whitelist may join."
                    checked={field.value}
                    onCheckedChange={field.onChange}
                    disabled={disabled}
                  />
                )}
              />
            </div>

            <div className="col-[1/-1] -mx-2 -mb-2 mt-2 flex min-h-[70px] items-center justify-between gap-4 border-t border-[#ddd4c3] px-2 pt-3.5 max-[620px]:col-auto max-[620px]:flex-col max-[620px]:items-stretch max-[620px]:[&>button]:w-full">
              {isDirty ? (
                <span className="flex items-center gap-2 text-xs font-[720] text-[#7c5a25]">
                  <i className="size-2 rounded-[3px] bg-warning" />
                  Unsaved draft
                </span>
              ) : (
                <span className="text-xs text-ink-soft">
                  Everything is saved.
                </span>
              )}
              <Button
                type="submit"
                disabled={!isDirty || disabled || isSubmitting}
              >
                {isSubmitting ? (
                  "Saving…"
                ) : (
                  <>
                    <Save size={16} />
                    Save configuration
                  </>
                )}
              </Button>
            </div>
          </form>
        </Surface>

        <aside className="sticky top-[30px] grid gap-[18px] max-[900px]:static max-[900px]:grid-cols-2 max-[620px]:grid-cols-1">
          <Surface className="p-[22px]">
            <h2 className="m-0 font-display text-xl leading-[1.2] font-[800]">Reset world</h2>
            <p className="mt-2 mb-0 text-[13px] leading-normal text-ink-soft">
              Both options keep the server address and ownership. Players are warned before an online reset; a pre-reset backup is retained.
            </p>
            <div className="mt-[17px] grid gap-2.5 [&_button]:w-full">
              <ConfirmDialog
                disabled={!allowed.canReset || reset.isPending}
                title="Reset world progress?"
                description="This clears every dimension, inventory, advancement, and player progress while preserving server settings, whitelist, bans, and operator access. A pre-reset backup is created first."
                confirmLabel="Reset world"
                onConfirm={() => reset.mutate("world")}
                trigger={<Button variant="secondary" disabled={!allowed.canReset || reset.isPending}><RotateCcw size={15} />World reset</Button>}
              />
              <ConfirmDialog
                danger
                disabled={!allowed.canReset || reset.isPending}
                title="Reset the full server?"
                description="This also restores settings to defaults and clears added whitelist, ban, and operator access. Ownership, address, and derived owner privileges remain. A pre-reset backup is created first."
                confirmLabel="Reset full server"
                onConfirm={() => reset.mutate("full")}
                trigger={<Button variant="danger" disabled={!allowed.canReset || reset.isPending}><RotateCcw size={15} />Full server reset</Button>}
              />
            </div>
            {reset.isPending ? <p className="mb-0 text-xs text-ink-soft">Reset queued. This page will show its current stage.</p> : null}
          </Surface>
          <Surface className="p-[22px]">
            <h2 className="m-0 font-display text-xl leading-[1.2] font-[800]">
              Sleeping-state edits
            </h2>
            <p className="mt-2 mb-0 text-[13px] leading-normal text-ink-soft">
              Address and world settings are only applied while the runtime is
              safely asleep.
            </p>
            <div className="mt-[18px] flex items-center justify-between gap-3 rounded-[9px] bg-[#eee7d8] p-3 text-xs">
              <span>Current state</span>
              <strong className="capitalize">
                {lifecycle[server.observed_state].label}
              </strong>
            </div>
            {!allowed.canEdit ? (
              <Button
                className="mt-[17px] w-full"
                variant="secondary"
                disabled={!allowed.canStop || stop.isPending}
                onClick={() => stop.mutate()}
              >
                <Square size={15} />
                {stop.isPending ? "Stopping…" : "Stop to edit"}
              </Button>
            ) : null}
          </Surface>

          <section className="rounded-md bg-[#f5e2dc] p-[22px] shadow-[inset_0_0_0_1px_#dfb5aa]">
            <h2 className="m-0 font-display text-xl leading-[1.2] font-[800] text-[#7d3029]">
              Delete hosted server
            </h2>
            <p className="mt-2 mb-0 text-[13px] leading-normal text-ink-soft">
              The server must be sleeping. This releases the address and removes
              the hosted server record.
            </p>
            <div className="mt-[17px] [&_button]:w-full">
              <ConfirmDialog
                danger
                disabled={!allowed.canDelete}
                title="Delete this hosted server?"
                description="This is destructive. Kurbis will request deletion of the hosted server and release its address after lifecycle cleanup."
                confirmLabel="Delete server"
                onConfirm={remove}
                trigger={
                  <Button variant="danger" disabled={!allowed.canDelete}>
                    <Trash2 size={16} />
                    Delete server
                  </Button>
                }
              />
            </div>
          </section>
        </aside>
      </div>
    </>
  );
}
