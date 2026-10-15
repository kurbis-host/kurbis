"use client";

import { ArrowRight } from "lucide-react";
import { useSyncExternalStore, type ReactNode } from "react";
import { keepSignInProvider, lastSignInProvider, type SignInProvider } from "../lib/last-sign-in";

const providerButton =
  "relative flex min-h-[58px] w-full items-center justify-between gap-4 rounded-[10px] border bg-white px-5 text-left font-[760] text-ink no-underline transition hover:-translate-y-0.5 focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-[#315d3a]";
const idle = "border-[#d8cfbd] shadow-[0_3px_0_#e8dfce] hover:border-[#a39882]";
const last = "border-[#315d3a] shadow-[0_3px_0_#315d3a]";

// The page leaves for the provider on click, so the stored value never changes under it.
const subscribe = () => () => {};

export function SignInProviders() {
  const lastUsed = useSyncExternalStore(subscribe, lastSignInProvider, () => null);
  return (
    <div className="grid gap-3">
      <ProviderLink provider="google" lastUsed={lastUsed} mark={<GoogleMark />}>
        Continue with Google
      </ProviderLink>
      <ProviderLink provider="discord" lastUsed={lastUsed} mark={<DiscordMark />}>
        Continue with Discord
      </ProviderLink>
    </div>
  );
}

function ProviderLink({
  provider,
  lastUsed,
  mark,
  children,
}: {
  provider: SignInProvider;
  lastUsed: SignInProvider | null;
  mark: ReactNode;
  children: ReactNode;
}) {
  const isLast = provider === lastUsed;
  return (
    <a
      className={`${providerButton} ${isLast ? last : idle}`}
      href={`/auth/${provider}/login`}
      onClick={() => keepSignInProvider(provider)}
    >
      <span className="flex items-center gap-3">
        {mark} {children}
      </span>
      {isLast ? (
        <span className="absolute -top-2.5 right-4 rounded-full bg-[#315d3a] px-2.5 py-0.5 text-[11px] leading-[1.5] font-[760] tracking-[0.02em] text-white">
          Last used
        </span>
      ) : null}
      <ArrowRight size={17} aria-hidden="true" />
    </a>
  );
}

function GoogleMark() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="size-5">
      <path fill="#4285F4" d="M21.6 12.23c0-.68-.06-1.34-.18-1.97H12v3.73h5.38a4.6 4.6 0 0 1-2 3.02v2.47h3.24c1.9-1.75 2.98-4.33 2.98-7.25Z" />
      <path fill="#34A853" d="M12 22c2.7 0 4.96-.9 6.62-2.43l-3.24-2.47c-.9.6-2.04.97-3.38.97-2.6 0-4.81-1.76-5.6-4.12H3.06v2.54A10 10 0 0 0 12 22Z" />
      <path fill="#FBBC05" d="M6.4 13.95a6.02 6.02 0 0 1 0-3.9V7.5H3.06a10 10 0 0 0 0 9l3.34-2.55Z" />
      <path fill="#EA4335" d="M12 5.93c1.47 0 2.8.5 3.84 1.52l2.88-2.88C16.95 2.96 14.7 2 12 2a10 10 0 0 0-8.94 5.5L6.4 10.05c.79-2.36 3-4.12 5.6-4.12Z" />
    </svg>
  );
}

function DiscordMark() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="size-5 fill-[#5865f2]">
      <path d="M19.7 5.1a18.4 18.4 0 0 0-4.55-1.4l-.56 1.14a17 17 0 0 0-5.18 0L8.84 3.7a18.4 18.4 0 0 0-4.55 1.4C1.4 9.34.62 13.38 1.01 17.36a18.5 18.5 0 0 0 5.58 2.82l1.2-1.94a12 12 0 0 1-1.89-.9l.46-.36c3.64 1.68 7.6 1.68 11.2 0l.47.36c-.6.35-1.23.65-1.9.9l1.2 1.94a18.5 18.5 0 0 0 5.59-2.82c.47-4.62-.8-8.62-3.22-12.26ZM8.73 14.68c-1.09 0-1.98-.98-1.98-2.18s.87-2.18 1.98-2.18c1.1 0 2 .99 1.98 2.18 0 1.2-.88 2.18-1.98 2.18Zm6.54 0c-1.09 0-1.98-.98-1.98-2.18s.87-2.18 1.98-2.18c1.1 0 2 .99 1.98 2.18 0 1.2-.87 2.18-1.98 2.18Z" />
    </svg>
  );
}
