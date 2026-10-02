"use client";

import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2, UserRoundCheck, UsersRound } from "lucide-react";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { errorMessage } from "../lib/api/client";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import {
  whitelistFormSchema,
  type WhitelistFormValues,
} from "../lib/schemas";
import {
  Button,
  controlStyles,
  EmptyState,
  Field,
  Notice,
  PageHeader,
  Surface,
} from "./ui";

type PlayerPresence = components["schemas"]["PlayerPresence"];
type Server = components["schemas"]["Server"];
type WhitelistEntry = components["schemas"]["WhitelistEntry"];
type WhitelistState = components["schemas"]["Whitelist"];

const listItem =
  "grid grid-cols-[38px_minmax(0,1fr)_auto] items-center gap-3 border-t border-[#ddd4c3] py-[13px]";

function Face({ children }: { children: React.ReactNode }) {
  return (
    <span className="grid size-9 place-items-center rounded-sm bg-[#d8dfc8] font-[850] text-[#4c6339]">
      {children}
    </span>
  );
}

export function Players({
  initialServer,
  initialPlayers,
  initialWhitelist,
}: {
  initialServer: Server;
  initialPlayers: PlayerPresence[];
  initialWhitelist: WhitelistState;
}) {
  const queryClient = useQueryClient();
  const serverId = initialServer.id;
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(serverId),
    queryFn: () => browserApi.getServer(serverId),
    initialData: initialServer,
  });
  const { data: players = initialPlayers } = useQuery({
    queryKey: queryKeys.players(serverId),
    queryFn: () => browserApi.getPlayers(serverId),
    initialData: initialPlayers,
  });
  const { data: whitelist = initialWhitelist } = useQuery({
    queryKey: queryKeys.whitelist(serverId),
    queryFn: () => browserApi.getWhitelist(serverId),
    initialData: initialWhitelist,
  });
  const [requestError, setRequestError] = useState<string | null>(null);
  const form = useForm<WhitelistFormValues>({
    resolver: zodResolver(whitelistFormSchema),
    defaultValues: { player_id: "", name: "" },
  });

  if (!server) throw new Error("Server disappeared while players were open.");

  const add = useMutation({
    mutationFn: ({ player_id, name }: WhitelistFormValues) =>
      browserApi.addWhitelistEntry(serverId, { player_id, name }),
    onMutate: () => setRequestError(null),
    onSuccess: () => {
      form.reset();
      void queryClient.invalidateQueries({ queryKey: queryKeys.whitelist(serverId) });
    },
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const remove = useMutation({
    mutationFn: (entry: WhitelistEntry) =>
      browserApi.removeWhitelistEntry(serverId, entry.player_id),
    onMutate: () => setRequestError(null),
    onSuccess: () =>
      void queryClient.invalidateQueries({ queryKey: queryKeys.whitelist(serverId) }),
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const whitelistedIds = new Set(whitelist.entries.map((entry) => entry.player_id));

  return (
    <>
      <PageHeader
        title="Players"
        description="See who is online now and manage the persistent whitelist that follows the world across workers."
      />
      {requestError ? (
        <div className="mb-[18px]">
          <Notice tone="danger">{requestError}</Notice>
        </div>
      ) : null}
      {!server.settings.whitelist ? (
        <div className="mb-[18px]">
          <Notice>
            Whitelist enforcement is currently off. Entries remain saved and
            will be applied when you enable it in Settings.
          </Notice>
        </div>
      ) : null}

      <div className="grid grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)] items-start gap-[18px] max-[820px]:grid-cols-1">
        <Surface className="p-6">
          <div className="flex items-center justify-between gap-3">
            <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">
              Online now
            </h2>
            <span className="grid h-7 min-w-8 place-items-center rounded-sm bg-[#e7dfcf] text-xs font-[800]">
              {players.length}
            </span>
          </div>
          {players.length ? (
            <ul className="mt-[18px] mb-0 list-none p-0">
              {players.map((player) => (
                <li className={listItem} key={player.player_id}>
                  <Face>{player.name.slice(0, 1).toUpperCase()}</Face>
                  <div>
                    <strong className="block text-[13px]">{player.name}</strong>
                    <span className="mt-[3px] block text-[11px] text-ink-soft">
                      Connected{" "}
                      {new Date(player.connected_at).toLocaleTimeString()}
                    </span>
                  </div>
                  {whitelistedIds.has(player.player_id) ? (
                    <span className="inline-flex min-h-[26px] items-center rounded-[7px] bg-[#d8e6cf] px-2 text-[9px] font-[850] tracking-[0.06em] text-[#46643b] uppercase">
                      Allowed
                    </span>
                  ) : (
                    <button
                      className="min-h-9 cursor-pointer rounded-sm border-0 bg-[#e0e8ea] px-2.5 text-[10px] font-[850] tracking-[0.04em] text-[#44616a] uppercase disabled:cursor-not-allowed disabled:opacity-45"
                      disabled={add.isPending}
                      onClick={() => add.mutate({ player_id: player.player_id, name: player.name })}
                    >
                      Allow {player.edition}
                    </button>
                  )}
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState
              icon={<UsersRound size={22} />}
              title="Nobody is online"
              description="Player presence will update here through the server event stream."
            />
          )}
        </Surface>

        <Surface className="p-6">
          <div className="flex items-center justify-between gap-3">
            <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">
              Whitelist
            </h2>
            <span className="grid h-7 min-w-8 place-items-center rounded-sm bg-[#e7dfcf] text-xs font-[800]">
              {whitelist.entries.length}
            </span>
          </div>
          <p className="mt-3.5 mb-0 text-xs leading-[1.45] text-ink-soft">
            {!server.settings.whitelist
              ? "Saved list, currently not enforced."
              : server.observed_state === "online" && !whitelist.synchronized
                ? `Enabled. Applying revision ${whitelist.revision} to the live runtime…`
                : "Enabled and synchronized with the runtime."}
          </p>
          <form
            className="mt-[18px] grid grid-cols-2 gap-2 max-[620px]:grid-cols-1 max-[620px]:[&>button]:w-full"
            onSubmit={form.handleSubmit((values) => add.mutate(values))}
          >
            <Field
              label="Whitelist player name"
              error={form.formState.errors.name?.message}
            >
              <input
                className={`${controlStyles.input} min-w-0`}
                placeholder="Player name"
                {...form.register("name")}
              />
            </Field>
            <Field
              label="Whitelist player UUID"
              error={form.formState.errors.player_id?.message}
              help="Use the authenticated Minecraft UUID for players who are not currently online."
            >
              <input
                className={`${controlStyles.input} min-w-0 font-mono text-xs`}
                placeholder="00000000-0000-0000-0000-000000000000"
                {...form.register("player_id")}
              />
            </Field>
            <Button className="col-span-2 max-[620px]:col-auto" type="submit" disabled={add.isPending}>
              <Plus size={16} />
              Add player
            </Button>
          </form>

          {whitelist.entries.length ? (
            <ul className="mt-[18px] mb-0 list-none p-0">
              {whitelist.entries.map((entry) => (
                <li className={listItem} key={entry.player_id}>
                  <Face>
                    <UserRoundCheck size={17} />
                  </Face>
                  <div>
                    <strong className="block text-[13px]">{entry.name}</strong>
                    <span className="mt-[3px] block text-[11px] text-ink-soft">
                      Saved whitelist entry
                    </span>
                  </div>
                  <button
                    className="grid size-9 cursor-pointer place-items-center rounded-sm border-0 bg-[#f2ddd5] text-[#7a3028] disabled:cursor-not-allowed disabled:opacity-45"
                    aria-label={`Remove ${entry.name} from whitelist`}
                    disabled={remove.isPending}
                    onClick={() => remove.mutate(entry)}
                  >
                    <Trash2 size={15} />
                  </button>
                </li>
              ))}
            </ul>
          ) : (
            <EmptyState
              icon={<UserRoundCheck size={22} />}
              title="Whitelist is empty"
              description="Add a player name to persist access across restarts and worker moves."
            />
          )}
        </Surface>
      </div>
    </>
  );
}
