"use client";

import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Crown, LoaderCircle, Plus, Shield, ShieldCheck, Trash2, UserRoundPlus, UsersRound, UserRoundCheck, X } from "lucide-react";
import type { ReactNode } from "react";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { errorMessage } from "../lib/api/client";
import { formatRelative } from "../lib/format";
import { lifecycleActions, lifecycle } from "../lib/lifecycle";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import { useNow } from "../lib/use-now";
import type { OperatorEntry, OperatorRoster } from "../lib/player-roster";
import { playerMutationSchema, type PlayerMutationValues } from "../lib/schemas";
import {
  Button,
  controlStyles,
  EmptyState,
  Field,
  Notice,
  PageHeader,
  Surface,
  SwitchField,
  useReadOnly,
} from "./ui";

type PlayerPresence = components["schemas"]["PlayerPresence"];
type Server = components["schemas"]["Server"];
type WhitelistEntry = components["schemas"]["WhitelistEntry"];
type WhitelistState = components["schemas"]["Whitelist"];
type JoinRequest = components["schemas"]["JoinRequest"];
type MutationTarget = { name: string };

const listItem =
  "grid grid-cols-[38px_minmax(0,1fr)_auto] items-center gap-3 border-t border-[#ddd4c3] py-[13px]";
const quietAction =
  "inline-flex min-h-9 cursor-pointer items-center justify-center gap-1.5 rounded-sm border-0 bg-[#e0e8ea] px-2.5 text-[10px] font-[850] tracking-[0.04em] text-[#44616a] uppercase disabled:cursor-not-allowed disabled:opacity-45";

function Face({ children }: { children: ReactNode }) {
  return (
    <span className="grid size-9 place-items-center rounded-sm bg-[#d8dfc8] font-[850] text-[#4c6339]">
      {children}
    </span>
  );
}

function Count({ children }: { children: ReactNode }) {
  return (
    <span className="grid h-7 min-w-8 place-items-center rounded-sm bg-[#e7dfcf] px-2 text-xs font-[800]">
      {children}
    </span>
  );
}

function PlayerNameForm({
  label,
  buttonLabel,
  pending,
  disabled,
  onSubmit,
}: {
  label: string;
  buttonLabel: string;
  pending: boolean;
  disabled: boolean;
  onSubmit: (values: PlayerMutationValues) => Promise<void>;
}) {
  const form = useForm<PlayerMutationValues>({
    resolver: zodResolver(playerMutationSchema),
    defaultValues: { name: "" },
  });
  const readOnly = useReadOnly();

  return (
    <form
      className="mt-4 flex items-start gap-2 max-[620px]:flex-col"
      onSubmit={form.handleSubmit(async (values) => {
        try {
          await onSubmit(values);
          form.reset();
        } catch {
          // The owning mutation renders the API error; keep the draft for retry.
        }
      })}
    >
      <Field label={label} error={form.formState.errors.name?.message} help="Java names resolve to the verified profile. Bedrock players can be managed while online or after they have been seen on this server.">
        <input
          className={`${controlStyles.input} min-w-0`}
          aria-label={label}
          autoComplete="off"
          maxLength={64}
          placeholder="Player name"
          disabled={readOnly}
          {...form.register("name")}
        />
      </Field>
      <Button className="mt-[25px] shrink-0 max-[620px]:mt-0 max-[620px]:w-full" type="submit" disabled={pending || disabled}>
        {pending ? <LoaderCircle className="animate-spin" size={16} /> : <Plus size={16} />}
        {pending ? "Adding…" : buttonLabel}
      </Button>
    </form>
  );
}

function hasIdentity(entry: { player_id?: string | null; name: string }, player: PlayerPresence) {
  return (Boolean(entry.player_id) && entry.player_id === player.player_id)
    || entry.name.localeCompare(player.name, undefined, { sensitivity: "accent" }) === 0;
}

function joinAttempts(request: JoinRequest, now: number) {
  const when = `Tried to join ${formatRelative(request.last_attempt_at, now)}`;
  return request.attempts > 1 ? `${when} · ${request.attempts} tries` : when;
}

function addedBy(entry: WhitelistEntry) {
  if (!entry.added_by) return "";
  return ` · added by ${entry.added_by}${entry.added_in_game ? " in game" : ""}`;
}

function isProtectedOperator(entry: OperatorEntry) {
  return entry.is_owner;
}

function connectedAt(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? "Connected recently" : `Connected ${date.toLocaleTimeString()}`;
}

export function Players({
  initialServer,
  initialPlayers,
  initialWhitelist,
  renderedAt = Date.now(),
}: {
  initialServer: Server;
  initialPlayers: PlayerPresence[];
  initialWhitelist: WhitelistState;
  renderedAt?: number;
}) {
  const queryClient = useQueryClient();
  const now = useNow(renderedAt);
  const readOnly = useReadOnly();
  const serverId = initialServer.id;
  const [requestError, setRequestError] = useState<string | null>(null);
  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(serverId),
    queryFn: () => browserApi.getServer(serverId),
    initialData: initialServer,
  });
  const { data: players = initialPlayers, isError: playersFailed, error: playersError, refetch: refetchPlayers } = useQuery({
    queryKey: queryKeys.players(serverId),
    queryFn: () => browserApi.getPlayers(serverId),
    initialData: initialPlayers,
  });
  const { data: whitelist = initialWhitelist, isError: whitelistFailed, error: whitelistError, refetch: refetchWhitelist } = useQuery({
    queryKey: queryKeys.whitelist(serverId),
    queryFn: () => browserApi.getWhitelist(serverId),
    initialData: initialWhitelist,
  });
  const { data: operatorData, isLoading: operatorsLoading, isError: operatorsFailed, error: operatorsError, refetch: refetchOperators } = useQuery({
    queryKey: queryKeys.operators(serverId),
    queryFn: () => browserApi.getOperators(serverId),
  });
  const operators = operatorData ?? emptyOperators;

  if (!server) throw new Error("Server disappeared while players were open.");

  const setWhitelist = (next: WhitelistState) => {
    queryClient.setQueryData(queryKeys.whitelist(serverId), next);
    void queryClient.invalidateQueries({ queryKey: queryKeys.whitelist(serverId) });
  };
  const setOperators = (next: OperatorRoster) => {
    queryClient.setQueryData(queryKeys.operators(serverId), next);
    void queryClient.invalidateQueries({ queryKey: queryKeys.operators(serverId) });
  };
  const setEnforced = useMutation({
    mutationFn: (whitelist: boolean) =>
      browserApi.updateServer(serverId, {
        // A null leaves that setting as it is.
        settings: { difficulty: null, gamemode: null, hardcore: null, motd: null, simulation_distance: null, tab_stats: null, view_distance: null, whitelist, whitelist_by_members: null },
      }),
    onMutate: () => setRequestError(null),
    onSuccess: (updated) => {
      queryClient.setQueryData(queryKeys.server(serverId), updated);
      void queryClient.invalidateQueries({ queryKey: queryKeys.whitelist(serverId) });
    },
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const addWhitelist = useMutation({
    mutationFn: (target: MutationTarget) => browserApi.addWhitelistEntry(serverId, target),
    onMutate: () => setRequestError(null),
    onSuccess: setWhitelist,
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const removeWhitelist = useMutation({
    mutationFn: (entry: WhitelistEntry) => browserApi.removeWhitelistEntry(serverId, entry.player_id),
    onMutate: () => setRequestError(null),
    onSuccess: setWhitelist,
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const letIn = useMutation({
    mutationFn: (request: JoinRequest) => browserApi.letInJoinRequest(serverId, request.player_id),
    onMutate: () => setRequestError(null),
    onSuccess: setWhitelist,
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const claim = useMutation({
    mutationFn: (request: JoinRequest) => browserApi.claimJoinRequest(serverId, request.player_id),
    onMutate: () => setRequestError(null),
    onSuccess: (next) => {
      setWhitelist(next);
      void queryClient.invalidateQueries({ queryKey: queryKeys.operators(serverId) });
    },
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const dismiss = useMutation({
    mutationFn: (request: JoinRequest) => browserApi.dismissJoinRequest(serverId, request.player_id),
    onMutate: () => setRequestError(null),
    onSuccess: setWhitelist,
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const answering = letIn.isPending || claim.isPending || dismiss.isPending;
  const addOperator = useMutation({
    mutationFn: (target: MutationTarget) => browserApi.addOperatorEntry(serverId, target),
    onMutate: () => setRequestError(null),
    onSuccess: setOperators,
    onError: (error) => setRequestError(errorMessage(error)),
  });
  const removeOperator = useMutation({
    mutationFn: (entry: OperatorEntry) => browserApi.removeOperatorEntry(serverId, entry.player_id),
    onMutate: () => setRequestError(null),
    onSuccess: setOperators,
    onError: (error) => setRequestError(errorMessage(error)),
  });

  const whitelisted = (player: PlayerPresence) => whitelist.entries.some((entry) => hasIdentity(entry, player));
  const operatorFor = (player: PlayerPresence) => operators.entries.find((entry) => hasIdentity(entry, player));
  const submitWhitelist = (values: PlayerMutationValues) => addWhitelist.mutateAsync({ name: values.name }).then(() => undefined);
  const submitOperator = (values: PlayerMutationValues) => addOperator.mutateAsync({ name: values.name }).then(() => undefined);
  const canChangeSettings = lifecycleActions(server).canChangeAccess;
  const hasQueryError = playersFailed || whitelistFailed || operatorsFailed;

  return (
    <>
      <PageHeader
        title="Players"
        description="See who's online, control who can join, and manage trusted operators—all from one place."
        actions={
          <div className="flex flex-wrap gap-2 text-[11px] font-[780] text-ink-soft">
            <span className="inline-flex min-h-8 items-center gap-1.5 rounded-sm bg-[#e7dfcf] px-2.5"><UsersRound size={14} /> {players.length} online</span>
            <span className="inline-flex min-h-8 items-center gap-1.5 rounded-sm bg-[#e7dfcf] px-2.5"><UserRoundCheck size={14} /> {whitelist.entries.length} allowed</span>
            <span className="inline-flex min-h-8 items-center gap-1.5 rounded-sm bg-[#e7dfcf] px-2.5"><Shield size={14} /> {operators.entries.length} operators</span>
          </div>
        }
      />
      {requestError ? <div className="mb-[18px]"><Notice tone="danger">{requestError}</Notice></div> : null}
      {hasQueryError ? (
        <div className="mb-[18px]">
          <Notice tone="danger">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <span>Some player data couldn't be refreshed: {errorMessage(playersError ?? whitelistError ?? operatorsError)}</span>
              <Button size="small" variant="secondary" readSafe onClick={() => { void refetchPlayers(); void refetchWhitelist(); void refetchOperators(); }}>Retry</Button>
            </div>
          </Notice>
        </div>
      ) : null}
      {server.settings.whitelist && !whitelist.synchronized ? (
        <div className="mb-[18px]">
          <Notice tone="warning">Whitelist changes are waiting for the live runtime to acknowledge revision {whitelist.revision}.</Notice>
        </div>
      ) : null}

      <div className="grid grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)] items-start gap-[18px] max-[920px]:grid-cols-1">
        <Surface className="p-6">
          <div className="flex items-center justify-between gap-3">
            <div>
              <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Online now</h2>
              <p className="mt-1.5 mb-0 text-xs text-ink-soft">Allow access or grant operator status without looking up an ID.</p>
            </div>
            <Count>{players.length}</Count>
          </div>
          {players.length ? (
            <ul className="mt-[18px] mb-0 list-none p-0">
              {players.map((player) => {
                const op = operatorFor(player);
                const locked = op ? isProtectedOperator(op) : false;
                const isWhitelisted = whitelisted(player);
                return (
                  <li className={`${listItem} max-[620px]:grid-cols-[38px_minmax(0,1fr)]`} key={player.player_id}>
                    <Face>{player.name.slice(0, 1).toUpperCase()}</Face>
                    <div className="min-w-0">
                      <strong className="block truncate text-[13px]">{player.name}</strong>
                      <span className="mt-[3px] block text-[11px] text-ink-soft">{connectedAt(player.connected_at)} · {player.edition}</span>
                    </div>
                    <div className="flex flex-wrap justify-end gap-1.5 max-[620px]:col-span-2 max-[620px]:justify-start max-[620px]:pl-[50px]">
                      {isWhitelisted ? (
                        <span className="inline-flex min-h-[28px] items-center gap-1 rounded-[7px] bg-[#d8e6cf] px-2 text-[9px] font-[850] tracking-[0.04em] text-[#46643b] uppercase"><Check size={12} /> Allowed</span>
                      ) : (
                        <button className={quietAction} type="button" disabled={readOnly || addWhitelist.isPending} onClick={() => addWhitelist.mutate({ name: player.name })}>
                          {addWhitelist.isPending && addWhitelist.variables?.name === player.name ? <LoaderCircle className="animate-spin" size={12} /> : <Plus size={12} />}
                          Allow
                        </button>
                      )}
                      {op ? (
                        <span className={`inline-flex min-h-[28px] items-center gap-1 rounded-[7px] px-2 text-[9px] font-[850] tracking-[0.04em] uppercase ${locked ? "bg-[#f1e3c8] text-[#72501f]" : "bg-[#dfeaf0] text-[#2c5265]"}`}>
                          {locked ? <Crown size={12} /> : <ShieldCheck size={12} />}{locked ? "Owner" : "Operator"}
                        </span>
                      ) : (
                        <button className={quietAction} type="button" disabled={readOnly || addOperator.isPending} onClick={() => addOperator.mutate({ name: player.name })}>
                          {addOperator.isPending && addOperator.variables?.name === player.name ? <LoaderCircle className="animate-spin" size={12} /> : <Shield size={12} />}
                          Grant OP
                        </button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          ) : (
            <EmptyState icon={<UsersRound size={22} />} title="Nobody is online" description="Live player presence will appear here. You can still add Java players to either list by name below." />
          )}
        </Surface>

        <div className="grid gap-[18px]">
          {whitelist.requests.length ? (
            <Surface className="p-6">
              <div className="flex items-center justify-between gap-3">
                <div>
                  <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Asked to join</h2>
                  <p className="mt-1.5 mb-0 text-xs text-ink-soft">
                    {whitelist.owner_has_minecraft_account
                      ? "Players the whitelist turned away in the last day."
                      : "Players the whitelist turned away in the last day. If one of them is you, choose That's me: that player is let in and made an operator."}
                  </p>
                </div>
                <Count>{whitelist.requests.length}</Count>
              </div>
              <ul className="mt-[18px] mb-0 list-none p-0">
                {whitelist.requests.map((request) => (
                  <li className={listItem} key={request.player_id}>
                    <Face><UserRoundPlus size={17} /></Face>
                    <div className="min-w-0">
                      <strong className="block truncate text-[13px]">{request.name}</strong>
                      <span className="mt-[3px] block text-[11px] text-ink-soft">{joinAttempts(request, now)}</span>
                    </div>
                    <div className="flex items-center gap-1.5">
                      <button className={quietAction} type="button" disabled={readOnly || answering} onClick={() => letIn.mutate(request)}>
                        {letIn.isPending && letIn.variables?.player_id === request.player_id ? <LoaderCircle className="animate-spin" size={12} /> : <Check size={12} />}
                        Let in
                      </button>
                      {whitelist.owner_has_minecraft_account ? null : (
                        <button className={quietAction} type="button" disabled={readOnly || answering} onClick={() => claim.mutate(request)}>
                          {claim.isPending && claim.variables?.player_id === request.player_id ? <LoaderCircle className="animate-spin" size={12} /> : <Crown size={12} />}
                          That&apos;s me
                        </button>
                      )}
                      <button className="grid size-9 cursor-pointer place-items-center rounded-sm border-0 bg-[#ece5d6] text-ink-soft disabled:cursor-not-allowed disabled:opacity-45" type="button" aria-label={`Dismiss ${request.name}'s request`} disabled={readOnly || answering} onClick={() => dismiss.mutate(request)}>
                        <X size={15} />
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            </Surface>
          ) : null}

          <Surface className="p-6">
            <div className="flex items-center justify-between gap-3">
              <div>
                <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Whitelist</h2>
                <p className="mt-1.5 mb-0 text-xs text-ink-soft">Players allowed to join when enforcement is enabled.</p>
              </div>
              <Count>{whitelist.entries.length}</Count>
            </div>
            <div className="mt-3.5 border-t border-[#ddd4c3] pt-1.5">
              <SwitchField
                label="Only whitelisted players may join"
                description={
                  server.settings.whitelist
                    ? "Everyone else is turned away."
                    : "Off: anyone can join. Entries stay saved for when you turn it on."
                }
                checked={server.settings.whitelist}
                onCheckedChange={(value) => setEnforced.mutate(value)}
                disabled={!canChangeSettings || setEnforced.isPending}
              />
              {canChangeSettings ? null : (
                <p className="mt-1 mb-0 text-xs text-ink-soft">
                  This can be changed while the world is sleeping or online, not while it is {lifecycle[server.observed_state].label.toLowerCase()}.
                </p>
              )}
            </div>
            <PlayerNameForm label="Add by player name" buttonLabel="Add to whitelist" pending={addWhitelist.isPending} disabled={false} onSubmit={submitWhitelist} />
            {whitelist.entries.length ? (
              <ul className="mt-[18px] mb-0 list-none p-0">
                {whitelist.entries.map((entry) => (
                  <li className={listItem} key={entry.player_id}>
                    <Face><UserRoundCheck size={17} /></Face>
                    <div className="min-w-0">
                      <strong className="block truncate text-[13px]">{entry.name}</strong>
                      <span className="mt-[3px] block text-[11px] text-ink-soft">{players.some((player) => hasIdentity(entry, player)) ? "Online now" : "Saved access"}{addedBy(entry)}</span>
                    </div>
                    <button className="grid size-9 cursor-pointer place-items-center rounded-sm border-0 bg-[#f2ddd5] text-[#7a3028] disabled:cursor-not-allowed disabled:opacity-45" type="button" aria-label={`Remove ${entry.name} from whitelist`} disabled={readOnly || removeWhitelist.isPending} onClick={() => removeWhitelist.mutate(entry)}>
                      {removeWhitelist.isPending && removeWhitelist.variables?.name === entry.name ? <LoaderCircle className="animate-spin" size={15} /> : <Trash2 size={15} />}
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <EmptyState icon={<UserRoundCheck size={22} />} title="Whitelist is empty" description="Add a player by name; no UUID lookup is needed in the panel." />
            )}
          </Surface>

          <Surface className="p-6">
            <div className="flex items-center justify-between gap-3">
              <div>
                <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Operators</h2>
                <p className="mt-1.5 mb-0 text-xs text-ink-soft">Grant elevated permissions only to people you trust.</p>
              </div>
              <Count>{operators.entries.length}</Count>
            </div>
            <PlayerNameForm label="Add trusted player by name" buttonLabel="Grant operator" pending={addOperator.isPending} disabled={false} onSubmit={submitOperator} />
            {operatorsLoading ? (
              <p className="mt-3 mb-0 text-xs text-ink-soft" role="status">Loading the operator roster…</p>
            ) : operators.revision !== operators.applied_revision ? (
              <p className="mt-3 mb-0 text-xs text-[#72501f]" role="status">Waiting for operator changes to synchronize with the live server…</p>
            ) : null}
            {operators.entries.length ? (
              <ul className="mt-[18px] mb-0 list-none p-0">
                {operators.entries.map((entry) => {
                  const locked = isProtectedOperator(entry);
                  return (
                    <li className={listItem} key={entry.player_id}>
                      <Face>{locked ? <Crown size={17} /> : <ShieldCheck size={17} />}</Face>
                      <div className="min-w-0">
                        <strong className="block truncate text-[13px]">{entry.name}</strong>
                        <span className="mt-[3px] block text-[11px] text-ink-soft">{players.some((player) => hasIdentity(entry, player)) ? "Online now" : locked ? "Server owner · protected" : "Operator"}</span>
                      </div>
                      {locked ? (
                        <span className="inline-flex min-h-8 items-center gap-1 rounded-[7px] bg-[#f1e3c8] px-2 text-[9px] font-[850] tracking-[0.04em] text-[#72501f] uppercase" title="The server owner's operator access is protected."><Crown size={12} /> Locked</span>
                      ) : (
                        <button className="grid size-9 cursor-pointer place-items-center rounded-sm border-0 bg-[#f2ddd5] text-[#7a3028] disabled:cursor-not-allowed disabled:opacity-45" type="button" aria-label={`Remove ${entry.name} as operator`} disabled={readOnly || removeOperator.isPending} onClick={() => removeOperator.mutate(entry)}>
                          {removeOperator.isPending && removeOperator.variables?.name === entry.name ? <LoaderCircle className="animate-spin" size={15} /> : <Trash2 size={15} />}
                        </button>
                      )}
                    </li>
                  );
                })}
              </ul>
            ) : operatorsLoading ? null : (
              <EmptyState icon={<Shield size={22} />} title="No extra operators" description="The server owner always keeps operator access. Add trusted helpers here when needed." />
            )}
            {operators.entries.length > 0 && operators.entries.every(isProtectedOperator) ? (
              <p className="mt-3 mb-0 text-xs text-ink-soft">No extra operators</p>
            ) : null}
          </Surface>
        </div>
      </div>
    </>
  );
}

const emptyOperators: OperatorRoster = {
  revision: 0,
  applied_revision: 0,
  synchronized: true,
  entries: [],
};
