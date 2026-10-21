"use client";

import * as Switch from "@radix-ui/react-switch";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, LoaderCircle, Puzzle, RefreshCw, Search, Store, Trash2, Upload } from "lucide-react";
import { useDeferredValue, useRef, useState, type ReactNode } from "react";
import { errorMessage } from "../lib/api/client";
import type { components } from "../lib/api/generated";
import { browserApi } from "../lib/api/operations";
import { formatBytes, formatCount } from "../lib/format";
import { queryKeys } from "../lib/query";
import { Button, ConfirmDialog, EmptyState, Notice, PageHeader, StatusLabel, Surface, controlStyles, useReadOnly } from "./ui";

type MarketPlugin = components["schemas"]["MarketPlugin"];
type Server = components["schemas"]["Server"];
type ServerPlugin = components["schemas"]["ServerPlugin"];
type ServerPlugins = components["schemas"]["ServerPlugins"];

const listItem = "grid grid-cols-[38px_minmax(0,1fr)_auto] items-start gap-3 border-t border-[#ddd4c3] py-[14px]";

export const pluginStates: Record<ServerPlugin["state"], { label: string; tone: "good" | "progress" | "danger" | "quiet" }> = {
  loaded: { label: "Running", tone: "good" },
  pending: { label: "Waiting to load", tone: "progress" },
  failed: { label: "Failed to load", tone: "danger" },
  disabled: { label: "Turned off", tone: "quiet" },
};

/** Why the installed list may not be what the server runs yet. */
export function syncNotice(plugins: ServerPlugins, server: Server) {
  if (plugins.synchronized) return null;
  return server.observed_state === "online"
    ? "Loading the latest plugin changes into the running server…"
    : "The server loads these plugins when it next starts.";
}

function Tile({ icon, children }: { icon?: string | null; children: ReactNode }) {
  return icon ? (
    // eslint-disable-next-line @next/next/no-img-element -- market icons are remote and small.
    <img className="size-9 rounded-sm bg-[#e7dfcf] object-cover" src={icon} alt="" loading="lazy" referrerPolicy="no-referrer" />
  ) : (
    <span className="grid size-9 place-items-center rounded-sm bg-[#d8dfc8] font-[850] text-[#4c6339]">{children}</span>
  );
}

export function Plugins({ initialServer, initialPlugins }: { initialServer: Server; initialPlugins: ServerPlugins }) {
  const queryClient = useQueryClient();
  const readOnly = useReadOnly();
  const serverId = initialServer.id;
  const [requestError, setRequestError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const query = useDeferredValue(search.trim());
  const fileInput = useRef<HTMLInputElement>(null);

  const { data: server = initialServer } = useQuery({
    queryKey: queryKeys.server(serverId),
    queryFn: () => browserApi.getServer(serverId),
    initialData: initialServer,
  });
  const { data: plugins = initialPlugins } = useQuery({
    queryKey: queryKeys.plugins(serverId),
    queryFn: () => browserApi.getPlugins(serverId),
    initialData: initialPlugins,
  });
  const market = useQuery({
    queryKey: queryKeys.pluginMarketSearch(serverId, query),
    queryFn: () => browserApi.getMarketPlugins(serverId, query || undefined),
    placeholderData: (previous) => previous,
  });

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.plugins(serverId) });
  };
  const mutation = <T,>(run: (input: T) => Promise<unknown>) => ({
    mutationFn: run,
    onMutate: () => setRequestError(null),
    onSuccess: refresh,
    onError: (error: unknown) => setRequestError(errorMessage(error)),
  });
  const install = useMutation(mutation((marketPluginId: number) => browserApi.installMarketPlugin(serverId, marketPluginId)));
  const upload = useMutation(mutation((file: File) => browserApi.uploadPlugin(serverId, file)));
  const toggle = useMutation(mutation((plugin: ServerPlugin) => browserApi.setPluginEnabled(serverId, plugin.id, !plugin.enabled)));
  const remove = useMutation(mutation((plugin: ServerPlugin) => browserApi.removePlugin(serverId, plugin.id)));

  const listings = market.data?.plugins ?? [];
  const listingFor = (plugin: ServerPlugin) => listings.find((listing) => listing.id === plugin.market_plugin_id);
  const full = plugins.plugins.length >= plugins.max_plugins;
  const notice = syncNotice(plugins, server);

  function chooseFile(file: File | undefined) {
    if (!file) return;
    if (file.size > plugins.max_plugin_bytes) {
      setRequestError(`${file.name} is larger than ${formatBytes(plugins.max_plugin_bytes)}.`);
      return;
    }
    upload.mutate(file);
  }

  return (
    <>
      <PageHeader
        title="Plugins"
        description="Add features to your server with plugins from the Pumpkin market, or upload your own. Changes load into a running server without a restart."
        actions={
          <span className="inline-flex min-h-8 items-center gap-1.5 rounded-sm bg-[#e7dfcf] px-2.5 text-[11px] font-[780] text-ink-soft">
            <Puzzle size={14} /> {plugins.plugins.length} of {plugins.max_plugins} installed
          </span>
        }
      />
      {requestError ? <div className="mb-[18px]"><Notice tone="danger">{requestError}</Notice></div> : null}
      {notice ? <div className="mb-[18px]"><Notice tone="warning">{notice}</Notice></div> : null}

      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-start gap-[18px] max-[920px]:grid-cols-1">
        <div className="grid gap-[18px]">
          <Surface className="p-6">
            <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Installed</h2>
            <p className="mt-1.5 mb-0 text-xs text-ink-soft">Turning a plugin off unloads it and keeps its saved data.</p>
            {plugins.plugins.length ? (
              <ul className="mt-[18px] mb-0 list-none p-0">
                {plugins.plugins.map((plugin) => {
                  const state = pluginStates[plugin.state];
                  const listing = listingFor(plugin);
                  const update = listing && listing.version !== plugin.version ? listing : null;
                  return (
                    <li className={listItem} key={plugin.id}>
                      <Tile icon={listing?.icon_url}>{plugin.name.slice(0, 1).toUpperCase()}</Tile>
                      <div className="min-w-0">
                        <strong className="block truncate text-[13px]">{plugin.name}{plugin.version ? <span className="font-[600] text-ink-soft"> {plugin.version}</span> : null}</strong>
                        <span className="mt-[3px] flex flex-wrap items-center gap-x-2 text-[11px] text-ink-soft">
                          <StatusLabel tone={state.tone}>{state.label}</StatusLabel>
                          <span>{plugin.source === "market" ? "From the market" : "Uploaded"} · {formatBytes(plugin.size_bytes)}</span>
                        </span>
                        {plugin.error ? <p className="mt-1.5 mb-0 text-[11px] leading-[1.4] break-words text-danger">{plugin.error}</p> : null}
                        {update ? (
                          <Button className="mt-2" size="small" variant="secondary" disabled={install.isPending} onClick={() => install.mutate(update.id)}>
                            {install.isPending && install.variables === update.id ? <LoaderCircle className="animate-spin" size={14} /> : <RefreshCw size={14} />}
                            Update to {update.version}
                          </Button>
                        ) : null}
                      </div>
                      <div className="flex items-center gap-2">
                        <Switch.Root
                          className="group h-[25px] w-11 shrink-0 cursor-pointer rounded-full border-0 bg-[#a7a292] p-[3px] transition-[background-color] duration-150 data-[state=checked]:bg-grass disabled:cursor-not-allowed disabled:opacity-45"
                          checked={plugin.enabled}
                          disabled={readOnly || toggle.isPending}
                          onCheckedChange={() => toggle.mutate(plugin)}
                          aria-label={`${plugin.enabled ? "Turn off" : "Turn on"} ${plugin.name}`}
                        >
                          <Switch.Thumb className="block size-[19px] rounded-[6px] bg-[#fffdf8] shadow-[0_2px_5px_rgba(0,0,0,0.22)] transition-transform duration-150 group-data-[state=checked]:translate-x-[19px]" />
                        </Switch.Root>
                        <ConfirmDialog
                          disabled={readOnly}
                          trigger={
                            <button className="grid size-9 cursor-pointer place-items-center rounded-sm border-0 bg-[#f2ddd5] text-[#7a3028] disabled:cursor-not-allowed disabled:opacity-45" type="button" aria-label={`Remove ${plugin.name}`}>
                              <Trash2 size={15} />
                            </button>
                          }
                          title={`Remove ${plugin.name}?`}
                          description="The server unloads it and deletes it. Its saved data stays, so installing it again picks up where it left off."
                          confirmLabel="Remove plugin"
                          danger
                          onConfirm={() => remove.mutateAsync(plugin).then(() => undefined)}
                        />
                      </div>
                    </li>
                  );
                })}
              </ul>
            ) : (
              <EmptyState icon={<Puzzle size={22} />} title="No plugins yet" description="Install one from the market or upload a .wasm plugin you built." />
            )}
          </Surface>

          <Surface className="p-6">
            <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Upload a plugin</h2>
            <p className="mt-1.5 mb-0 text-xs leading-[1.45] text-ink-soft">
              A Pumpkin plugin is a WebAssembly component built for <code>wasm32-wasip2</code>, up to {formatBytes(plugins.max_plugin_bytes)}. It can read and write its own data folder and make web requests; it cannot read the server&apos;s environment.
            </p>
            <input
              ref={fileInput}
              className="hidden"
              type="file"
              accept=".wasm,application/wasm"
              aria-label="Plugin file"
              onChange={(event) => {
                chooseFile(event.target.files?.[0]);
                event.target.value = "";
              }}
            />
            <Button className="mt-4" variant="secondary" disabled={upload.isPending || full} onClick={() => fileInput.current?.click()}>
              {upload.isPending ? <LoaderCircle className="animate-spin" size={16} /> : <Upload size={16} />}
              {upload.isPending ? "Uploading…" : "Choose a .wasm file"}
            </Button>
          </Surface>
        </div>

        <Surface className="p-6">
          <div className="flex items-center justify-between gap-3">
            <div>
              <h2 className="m-0 font-display text-[21px] leading-[1.2] font-[800]">Market</h2>
              <p className="mt-1.5 mb-0 text-xs text-ink-soft">
                Free plugins from <a className="text-sky-deep" href="https://market.pumpkinmc.org/" target="_blank" rel="noreferrer">market.pumpkinmc.org</a>.
              </p>
            </div>
            <Store className="text-earth" size={22} />
          </div>
          <label className="relative mt-4 block">
            <Search className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-ink-soft" size={16} />
            <input
              className={`${controlStyles.input} pl-9`}
              type="search"
              aria-label="Search the market"
              placeholder="Search plugins"
              maxLength={64}
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          </label>
          {market.isError ? (
            <div className="mt-4"><Notice tone="danger">The market couldn&apos;t be reached: {errorMessage(market.error)}</Notice></div>
          ) : market.isPending ? (
            <p className="mt-4 mb-0 text-xs text-ink-soft" role="status">Loading the market…</p>
          ) : listings.length ? (
            <ul className="mt-[6px] mb-0 list-none p-0">
              {listings.map((listing) => (
                <MarketListing
                  key={listing.id}
                  listing={listing}
                  pending={install.isPending && install.variables === listing.id}
                  disabled={install.isPending || (full && !listing.installed_plugin_id)}
                  onInstall={() => install.mutate(listing.id)}
                />
              ))}
            </ul>
          ) : (
            <EmptyState icon={<Search size={22} />} title="Nothing found" description="No free plugin matches that search." />
          )}
        </Surface>
      </div>
    </>
  );
}

function MarketListing({ listing, pending, disabled, onInstall }: { listing: MarketPlugin; pending: boolean; disabled: boolean; onInstall: () => void }) {
  const installed = listing.installed_version !== null && listing.installed_version !== undefined;
  const current = installed && listing.installed_version === listing.version;
  return (
    <li className={listItem}>
      <Tile icon={listing.icon_url}>{listing.name.slice(0, 1).toUpperCase()}</Tile>
      <div className="min-w-0">
        <strong className="block truncate text-[13px]">{listing.name}{listing.version ? <span className="font-[600] text-ink-soft"> {listing.version}</span> : null}</strong>
        <span className="mt-[3px] block truncate text-[11px] text-ink-soft">
          {[listing.developer, listing.category, `${formatCount(listing.downloads)} downloads`].filter(Boolean).join(" · ")}
        </span>
        {listing.description ? <p className="mt-1.5 mb-0 line-clamp-3 text-xs leading-[1.45] text-ink-soft">{listing.description}</p> : null}
      </div>
      {current ? (
        <span className="inline-flex min-h-8 items-center rounded-[7px] bg-[#d8e6cf] px-2 text-[9px] font-[850] tracking-[0.04em] text-[#46643b] uppercase">Installed</span>
      ) : (
        <Button size="small" variant={installed ? "secondary" : "primary"} disabled={disabled} onClick={onInstall} aria-label={`${installed ? "Update" : "Install"} ${listing.name}`}>
          {pending ? <LoaderCircle className="animate-spin" size={14} /> : installed ? <RefreshCw size={14} /> : <Download size={14} />}
          {installed ? "Update" : "Install"}
        </Button>
      )}
    </li>
  );
}
