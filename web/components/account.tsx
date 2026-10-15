"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Gamepad2, Link2, LogOut, MonitorSmartphone, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { errorMessage } from "../lib/api/client";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { formatBytes } from "../lib/format";
import { queryKeys } from "../lib/query";
import { Button, Notice, PageHeader, Surface } from "./ui";

type AccountData = components["schemas"]["Account"];
type Server = components["schemas"]["Server"];
type Session = components["schemas"]["Session"];

const limitCard = "rounded-[10px] bg-[#eee7d8] p-4";
const secondaryLink =
  "inline-flex min-h-11 items-center justify-center gap-2 rounded-sm bg-paper px-4 text-[13px] font-[760] text-ink no-underline shadow-[inset_0_0_0_1px_#c6bdab,0_4px_12px_rgba(60,47,30,0.07)] transition-colors hover:bg-[#fffdf7] focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-[#315d3a]";

export function Account({
  initialSession,
  initialServers,
  initialAccount,
}: {
  initialSession: Session;
  initialServers: Server[];
  initialAccount: AccountData;
}) {
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const [worldNotice, setWorldNotice] = useState<string | null>(null);
  const { data: session = initialSession } = useQuery({
    queryKey: queryKeys.session,
    queryFn: browserApi.getSession,
    initialData: initialSession,
  });
  const { data: servers = initialServers } = useQuery({
    queryKey: queryKeys.servers,
    queryFn: browserApi.getServers,
    initialData: initialServers,
  });
  const { data: account = initialAccount } = useQuery({
    queryKey: queryKeys.account,
    queryFn: browserApi.getAccount,
    initialData: initialAccount,
  });
  const revokeSession = useMutation({
    mutationFn: (id: string) => browserApi.revokeAccountSession(id),
    onSuccess: async (_, id) => {
      const revoked = account.sessions.find((item) => item.id === id);
      if (revoked?.current) {
        window.location.assign("/login");
        return;
      }
      await queryClient.invalidateQueries({ queryKey: queryKeys.account });
    },
    onError: (error) => setActionError(errorMessage(error)),
  });
  const setDefaultWorld = useMutation({
    mutationFn: (id: string) => browserApi.setDefaultWorld(id),
    onSuccess: async (_, id) => {
      const selected = account.worlds.find((world) => world.id === id);
      setWorldNotice(selected ? `${selected.subdomain} is now the default world.` : "Default world updated.");
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.account }),
        queryClient.invalidateQueries({ queryKey: queryKeys.servers }),
      ]);
    },
    onError: (error) => setActionError(errorMessage(error)),
  });
  const ownedWorldIds = new Set(account.worlds.map((world) => world.id));
  const ownedServers = servers.filter((item) => ownedWorldIds.has(item.id));
  const server = ownedServers.find((item) => item.is_default) ?? ownedServers[0] ?? null;

  async function logout() {
    setActionError(null);
    try {
      await browserApi.logout();
      window.location.assign("/");
    } catch (error) {
      setActionError(errorMessage(error));
    }
  }

  return (
    <>
      <PageHeader
        title="Account"
        description="Manage sign-in methods, active browser sessions, and the limits on your Kurbis account."
      />
      {actionError ? <div className="mb-[18px]"><Notice tone="danger">{actionError}</Notice></div> : null}
      <div className="grid grid-cols-[minmax(0,1fr)_minmax(280px,0.7fr)] gap-[18px] max-[760px]:grid-cols-1">
        <Surface className="p-[26px]">
          <div className="flex items-center gap-4">
            <div className="grid size-[58px] shrink-0 place-items-center rounded-md bg-[#302b25] font-display text-2xl leading-none font-[850] text-[#f0a362]">
              {session.user.display_name.slice(0, 1).toUpperCase()}
            </div>
            <div className="min-w-0">
              <h2 className="m-0 break-words font-display text-[22px] leading-[1.2] font-[800]">
                {session.user.display_name}
              </h2>
              <p className="mt-[5px] mb-0 break-all text-ink-soft">
                {session.user.email ?? "No verified email linked"}
              </p>
            </div>
          </div>

          <div className="mt-6 border-t border-[#ddd4c3]">
            {[
              ["Account status", session.user.account_status],
              ["Account ID", session.user.id],
              ["Hosted world", server?.java_address ?? "Not created yet"],
            ].map(([label, value]) => (
              <div
                className="flex min-h-[54px] items-center justify-between gap-[18px] border-b border-[#ddd4c3] text-[13px] max-[480px]:flex-col max-[480px]:items-start max-[480px]:gap-1 max-[480px]:py-3"
                key={label}
              >
                <span className="text-ink-soft">{label}</span>
                <strong className="[overflow-wrap:anywhere] text-right max-[480px]:text-left">
                  {value}
                </strong>
              </div>
            ))}
          </div>
          <div className="mt-6">
            <Button variant="secondary" onClick={() => void logout()}>
              <LogOut size={16} />
              Sign out
            </Button>
          </div>
        </Surface>

        <Surface className="p-[26px]">
          <h2 className="m-0 font-display text-[22px] leading-[1.2] font-[800]">
            Service limits
          </h2>
          <div className="mt-5 grid grid-cols-2 gap-2.5 max-[480px]:grid-cols-1">
            <div className={limitCard}>
              <strong className="block font-display text-[21px] leading-[1.1] font-[800]">
                {formatBytes(session.limits.memory_bytes)}
              </strong>
              <span className="mt-[5px] block text-[11px] text-ink-soft">memory</span>
            </div>
            <div className={limitCard}>
              <strong className="block font-display text-[21px] leading-[1.1] font-[800]">
                {formatBytes(session.limits.storage_bytes)}
              </strong>
              <span className="mt-[5px] block text-[11px] text-ink-soft">storage</span>
            </div>
            <div className={limitCard}>
              <strong className="block font-display text-[21px] leading-[1.1] font-[800]">
                {Math.round(session.limits.idle_seconds / 60)} min
              </strong>
              <span className="mt-[5px] block text-[11px] text-ink-soft">idle sleep</span>
            </div>
            <div className={limitCard}>
              <strong className="block font-display text-[21px] leading-[1.1] font-[800]">
                {session.limits.player_limit ?? "No set cap"}
              </strong>
              <span className="mt-[5px] block text-[11px] text-ink-soft">player limit</span>
            </div>
          </div>
          <div className="mt-[18px] rounded-[10px] bg-[#dfe7d2] p-4 text-[13px] leading-normal text-[#4a593e]">
            {server
              ? "Your hosted world keeps its address and backups when you link another sign-in method."
              : "This account has no hosted world yet. Join a free address in Minecraft to claim one."}
          </div>
        </Surface>
      </div>

      <div className="mt-[18px] grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-[18px] max-[900px]:grid-cols-1">
        <Surface className="p-[26px]">
          <div className="flex flex-wrap items-start justify-between gap-4">
            <div>
              <h2 className="m-0 font-display text-[22px] leading-[1.2] font-[800]">Linked identities</h2>
              <p className="mt-2 mb-0 max-w-[50ch] text-[13px] leading-[1.5] text-ink-soft">
                Each identity can sign in to this account. Verified email matches are linked automatically.
              </p>
            </div>
            <ShieldCheck className="mt-1 shrink-0 text-[#55704a]" size={20} aria-hidden="true" />
          </div>
          <ul className="mt-5 list-none border-t border-[#ddd4c3] p-0">
            {account.identities.map((identity) => (
              <li key={identity.id} className="flex min-w-0 items-center gap-3 border-b border-[#ddd4c3] py-3.5">
                <div className="grid size-10 shrink-0 place-items-center rounded-[10px] bg-[#eee7d8] text-earth">
                  {identity.provider.startsWith("minecraft") ? <Gamepad2 size={18} /> : <Link2 size={18} />}
                </div>
                <div className="min-w-0 flex-1">
                  <p className="m-0 font-[760]">{identityLabel(identity.provider)}</p>
                  <p className="mt-1 mb-0 break-words text-[12px] text-ink-soft">
                    {identity.email
                      ? `${identity.email}${identity.email_verified ? " · verified email" : ""}`
                      : identity.display_name}
                  </p>
                </div>
              </li>
            ))}
          </ul>
          <div className="mt-5 flex flex-wrap gap-2.5">
            <a className={secondaryLink} href="/auth/google/link">
              <Link2 size={15} /> Link another Google account
            </a>
            <a className={secondaryLink} href="/auth/discord/link">
              <Link2 size={15} /> Link another Discord account
            </a>
          </div>
        </Surface>

        <Surface className="p-[26px]">
          <div className="flex items-center gap-3">
            <MonitorSmartphone size={20} className="shrink-0 text-earth" aria-hidden="true" />
            <div>
              <h2 className="m-0 font-display text-[22px] leading-[1.2] font-[800]">Browser sessions</h2>
              <p className="mt-2 mb-0 text-[13px] leading-[1.5] text-ink-soft">
                Revoke a session to sign that browser out. Sessions stay active until revoked or its browser data is cleared.
              </p>
            </div>
          </div>
          <ul className="mt-5 list-none border-t border-[#ddd4c3] p-0">
            {account.sessions.map((browserSession) => (
              <li key={browserSession.id} className="flex min-w-0 flex-wrap items-center justify-between gap-3 border-b border-[#ddd4c3] py-3.5">
                <div className="min-w-0">
                  <p className="m-0 font-[760]">
                    {browserSession.current ? "This browser" : "Browser session"}
                    {browserSession.current ? <span className="ml-2 text-[11px] font-[650] text-[#55704a]">Current</span> : null}
                  </p>
                  <p className="mt-1 mb-0 text-[12px] text-ink-soft">
                    Last active <time dateTime={browserSession.last_seen_at}>{formatDate(browserSession.last_seen_at)}</time>
                  </p>
                </div>
                <Button
                  size="small"
                  variant="danger"
                  disabled={revokeSession.isPending}
                  onClick={() => {
                    setActionError(null);
                    revokeSession.mutate(browserSession.id);
                  }}
                >
                  {revokeSession.isPending ? "Revoking…" : "Revoke"}
                </Button>
              </li>
            ))}
          </ul>
          {account.sessions.length === 0 ? (
            <p className="mt-4 mb-0 text-[13px] text-ink-soft">No active browser sessions.</p>
          ) : null}
        </Surface>
      </div>

      <Surface className="mt-[18px] p-[26px]">
        <h2 className="m-0 font-display text-[22px] leading-[1.2] font-[800]">Worlds on this account</h2>
        <p className="mt-2 mb-0 max-w-[66ch] text-[13px] leading-[1.5] text-ink-soft">
          Your panel manages the selected default world. Identity merges keep every existing world; choose another here to manage it.
        </p>
        {worldNotice ? <p className="mt-3 mb-0 text-[13px] font-[700] text-[#3f713e]" role="status">{worldNotice}</p> : null}
        <ul className="mt-5 list-none border-t border-[#ddd4c3] p-0">
          {account.worlds.map((world) => (
            <li key={world.id} className="flex min-w-0 flex-wrap items-center justify-between gap-3 border-b border-[#ddd4c3] py-3.5">
              <div className="min-w-0">
                <p className="m-0 break-all font-[760]">{world.subdomain}</p>
                <p className="mt-1 mb-0 text-[12px] text-ink-soft">{world.observed_state}</p>
              </div>
              {world.is_default ? (
                <span className="rounded-full bg-[#dfe7d2] px-3 py-2 text-[12px] font-[750] text-[#3f713e]">Default world</span>
              ) : (
                <Button
                  size="small"
                  variant="secondary"
                  disabled={setDefaultWorld.isPending}
                  onClick={() => {
                    setActionError(null);
                    setWorldNotice(null);
                    setDefaultWorld.mutate(world.id);
                  }}
                >
                  {setDefaultWorld.isPending ? "Switching…" : "Make default"}
                </Button>
              )}
            </li>
          ))}
        </ul>
        {account.worlds.length === 0 ? (
          <p className="mt-4 mb-0 text-[13px] text-ink-soft">No active worlds are linked to this account.</p>
        ) : null}
        {account.worlds.length > 0 ? <a className={`${secondaryLink} mt-4`} href={`/manage/${server?.id ?? account.worlds[0].id}`}>Open default world</a> : null}
      </Surface>
    </>
  );
}

function identityLabel(provider: string) {
  switch (provider) {
    case "google": return "Google";
    case "discord": return "Discord";
    case "minecraft_java": return "Minecraft Java Edition";
    case "minecraft_bedrock": return "Minecraft Bedrock Edition";
  }
}

function formatDate(value: string) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}
