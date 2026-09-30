import { ArrowLeft, ArrowRight, Gamepad2 } from "lucide-react";

const providerButton =
  "flex min-h-[58px] w-full items-center justify-between gap-4 rounded-[10px] border border-[#d8cfbd] bg-white px-5 text-left font-[760] text-ink no-underline shadow-[0_3px_0_#e8dfce] transition hover:-translate-y-0.5 hover:border-[#a39882] focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-[#315d3a]";

export default function LoginPage() {
  return (
    <main className="grid min-h-[calc(100svh-64px)] place-items-center bg-[#f5efdf] px-4 py-12 text-ink">
      <section className="w-full max-w-[440px] rounded-[18px] border border-[#e2d9c7] bg-[#fbf8ef] p-7 shadow-[0_12px_40px_rgba(61,47,28,0.08)] sm:p-9">
        <a
          className="inline-flex items-center gap-2 text-sm font-[700] text-ink-soft no-underline hover:text-ink"
          href="/"
        >
          <ArrowLeft size={16} /> Back to Kurbis
        </a>
        <div className="mt-9 grid size-12 place-items-center rounded-[12px] bg-pumpkin text-[#2d1b11]">
          <Gamepad2 size={23} aria-hidden="true" />
        </div>
        <h1 className="mt-5 mb-0 font-display text-[34px] leading-tight font-[850] tracking-[-0.035em]">
          Sign in to Kurbis
        </h1>
        <p className="mt-2 mb-7 leading-[1.55] text-ink-soft">
          Choose the account you use to manage your Minecraft server.
        </p>
        <div className="grid gap-3">
          <a className={providerButton} href="/auth/google/login">
            <span className="flex items-center gap-3">
              <GoogleMark /> Continue with Google
            </span>
            <ArrowRight size={17} aria-hidden="true" />
          </a>
          <a className={providerButton} href="/auth/discord/login">
            <span className="flex items-center gap-3">
              <DiscordMark /> Continue with Discord
            </span>
            <ArrowRight size={17} aria-hidden="true" />
          </a>
        </div>
        <p className="mt-5 mb-0 rounded-[10px] bg-[#eee7d8] px-4 py-3.5 text-[13px] leading-[1.5] text-[#51483a]">
          Already playing on a Kurbis server? Verified Java and Bedrock players receive a private, one-use sign-in link in chat.
        </p>
        <p className="mt-6 mb-0 text-xs leading-[1.55] text-ink-soft">
          Kurbis receives your provider ID and display name. A verified email is used to link Google and Discord sign-ins.
        </p>
      </section>
    </main>
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
