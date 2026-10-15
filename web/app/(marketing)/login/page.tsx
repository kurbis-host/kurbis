import { ArrowLeft, Gamepad2 } from "lucide-react";
import { SignInProviders } from "../../../components/sign-in-providers";

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
        <SignInProviders />
        <p className="mt-5 mb-0 rounded-[10px] bg-[#eee7d8] px-4 py-3.5 text-[13px] leading-[1.5] text-[#51483a]">
          Already playing on a Kurbis server? Verified Java and Bedrock players receive a private, one-use sign-in link in chat.
        </p>
        <p className="mt-6 mb-0 text-xs leading-[1.55] text-ink-soft">
          Kurbis receives your provider ID and display name. A verified email is used to link Google and Discord sign-ins.{" "}
          <a className="font-[700] text-ink-soft" href="/privacy">
            Privacy
          </a>
          {" · "}
          <a className="font-[700] text-ink-soft" href="/impressum">
            Impressum
          </a>
        </p>
      </section>
    </main>
  );
}
