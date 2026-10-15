"use client";

import * as Tooltip from "@radix-ui/react-tooltip";
import { useQuery } from "@tanstack/react-query";
import { Copy } from "lucide-react";
import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { errorMessage } from "../lib/api/client";
import { browserApi } from "../lib/api/operations";
import { queryKeys } from "../lib/query";
import { subdomainSchema } from "../lib/schemas";
import { typedSubdomain } from "../lib/subdomain";
import { controlStyles, Field, Notice, PageHeader, Surface } from "./ui";

/** How long typing rests before the name is looked up. */
const LOOKUP_PAUSE_MS = 400;
/** How often the page checks whether the join has linked a server yet. */
const LINK_POLL_MS = 4000;

/**
 * Signing in claims nothing. A server name is claimed by the first verified
 * player who joins its address in Minecraft; the lobby then hands them a link
 * that joins the new server to the account signed in here.
 */
export function ClaimServer({ serverDomain }: { serverDomain: string }) {
  const router = useRouter();
  const [name, setName] = useState("");
  const [lookedUp, setLookedUp] = useState("");
  const [copied, setCopied] = useState(false);
  const problem = name ? subdomainSchema.safeParse(name).error?.issues[0]?.message : undefined;
  const valid = Boolean(name) && !problem;

  useEffect(() => {
    if (!valid) return;
    const timer = window.setTimeout(() => setLookedUp(name), LOOKUP_PAUSE_MS);
    return () => window.clearTimeout(timer);
  }, [name, valid]);

  const lookup = useQuery({
    queryKey: queryKeys.serverAddress(lookedUp),
    queryFn: () => browserApi.getServerAddress(lookedUp),
    enabled: valid && lookedUp === name,
    staleTime: 5_000,
  });
  const { data: account } = useQuery({
    queryKey: queryKeys.account,
    queryFn: browserApi.getAccount,
    refetchInterval: LINK_POLL_MS,
  });
  const linkedWorld = account?.worlds.find((world) => world.is_default) ?? account?.worlds[0];
  useEffect(() => {
    if (linkedWorld) router.replace(`/manage/${encodeURIComponent(linkedWorld.id)}`);
  }, [linkedWorld, router]);

  const address = `${valid ? name : "your-name"}.${serverDomain}`;
  const status = !name
    ? null
    : problem
      ? { tone: "danger" as const, text: problem }
      : lookup.isError
        ? { tone: "danger" as const, text: errorMessage(lookup.error) }
        : !lookup.data || lookup.data.subdomain !== name
          ? { tone: "quiet" as const, text: `Checking ${address}…` }
          : lookup.data.available
            ? { tone: "good" as const, text: `${lookup.data.address} is free. Join it in Minecraft to claim it.` }
            : { tone: "danger" as const, text: `${lookup.data.address} is taken. Joining it takes you to that server instead, so pick another name.` };

  return (
    <>
      <PageHeader
        title="Claim your server"
        description="A server name is claimed by joining it in Minecraft. Pick a name, then join its address: your first join creates the server."
      />
      <div className="grid grid-cols-[minmax(0,1.2fr)_minmax(290px,0.8fr)] items-start gap-5 max-[930px]:grid-cols-1">
        <Surface className="grid gap-5 p-[clamp(22px,4vw,34px)]">
          <Field label="Server name" help="3–32 lowercase letters, digits, or dashes.">
            <input
              className={controlStyles.input}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              placeholder="friends-world"
              value={name}
              onChange={(event) => setName(typedSubdomain(event.target.value))}
            />
          </Field>
          <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 rounded-[9px] bg-[#eee7d8] px-3.5 py-[13px]">
            <code className="min-w-0 overflow-hidden text-sm text-ellipsis" title={address} aria-label="Server address">
              {address}
            </code>
            <Tooltip.Root>
              <Tooltip.Trigger asChild>
                <button
                  className="grid size-9 cursor-pointer place-items-center rounded-[7px] border-0 bg-paper text-[#4e4b43] disabled:cursor-not-allowed disabled:opacity-50"
                  aria-label="Copy server address"
                  disabled={!valid}
                  onClick={async () => {
                    await navigator.clipboard.writeText(address);
                    setCopied(true);
                    window.setTimeout(() => setCopied(false), 1200);
                  }}
                >
                  <Copy size={15} />
                </button>
              </Tooltip.Trigger>
              <Tooltip.Portal>
                <Tooltip.Content sideOffset={6}>{copied ? "Copied" : "Copy address"}</Tooltip.Content>
              </Tooltip.Portal>
            </Tooltip.Root>
          </div>
          <p
            className={`m-0 min-h-[1.45em] text-[13px] leading-[1.45] ${
              status?.tone === "danger" ? "text-danger" : status?.tone === "good" ? "text-success" : "text-ink-soft"
            }`}
            role="status"
            aria-live="polite"
          >
            {status?.text}
          </p>
          <Notice>
            Picking a name here does not reserve it. The first player to join a free address claims it, so join soon.
          </Notice>
        </Surface>

        <aside className="sticky top-8 max-[930px]:static">
          <section className="rounded-md bg-[#302b25] p-6 text-[#f8f0df] shadow-low">
            <h2 className="m-0 font-display text-[22px] leading-[1.15] font-[800]">How claiming works</h2>
            <ol className="mt-5 mb-0 grid list-none gap-[17px] p-0">
              {[
                `Join ${address} from Minecraft Java or Bedrock.`,
                "Your first join creates the server and starts it. Kurbis sends you a link in chat.",
                "Open that link in this browser. It links the server and your Minecraft player to this account.",
                "This page opens your server panel as soon as the link is used.",
              ].map((text, index) => (
                <li
                  className="grid grid-cols-[34px_1fr] items-start gap-[11px] text-[13px] leading-[1.45] text-[#cec5b5]"
                  key={index}
                >
                  <span className="grid size-8 place-items-center rounded-sm bg-[#4c443a] font-[800] text-[#f1a25f]">
                    {index + 1}
                  </span>
                  <span>{text}</span>
                </li>
              ))}
            </ol>
          </section>
        </aside>
      </div>
    </>
  );
}
