"use client";

import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Gamepad2 } from "lucide-react";
import { ApiError, errorMessage } from "../../../lib/api/client";
import { browserApi } from "../../../lib/api/operations";

type PageState = "reading" | "redeeming" | "error";

export default function MinecraftLoginPage() {
  const started = useRef(false);
  const inFlight = useRef(false);
  const [token, setToken] = useState<string | null>(null);
  const [state, setState] = useState<PageState>("reading");
  const [message, setMessage] = useState("Checking your Minecraft sign-in link…");

  useEffect(() => {
    if (started.current) return;
    started.current = true;

    const fragment = new URLSearchParams(window.location.hash.slice(1));
    const loginToken = fragment.get("token");
    window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    if (!loginToken) {
      setState("error");
      setMessage("This sign-in link is missing its token. Reconnect to the server to get a fresh link.");
      return;
    }
    setToken(loginToken);
    void redeem(loginToken);
  }, []);

  async function redeem(loginToken: string) {
    if (inFlight.current) return;
    inFlight.current = true;
    setState("redeeming");
    setMessage("Signing in to Kurbis…");
    try {
      await browserApi.redeemMinecraft(loginToken);
      window.location.replace("/app");
    } catch (error) {
      setState("error");
      setMessage(error instanceof ApiError && error.status === 400
        ? "This sign-in link is invalid, expired, or already used."
        : errorMessage(error));
    } finally {
      inFlight.current = false;
    }
  }

  return (
    <main className="grid min-h-[calc(100svh-64px)] place-items-center bg-[#f5efdf] px-4 py-12 text-ink">
      <section className="w-full max-w-[440px] rounded-[18px] border border-[#e2d9c7] bg-[#fbf8ef] p-7 shadow-[0_12px_40px_rgba(61,47,28,0.08)] sm:p-9">
        <a
          className="inline-flex items-center gap-2 text-sm font-[700] text-ink-soft no-underline hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-[#315d3a]"
          href="/login"
        >
          <ArrowLeft size={16} /> Sign-in options
        </a>
        <div className="mt-9 grid size-12 place-items-center rounded-[12px] bg-pumpkin text-[#2d1b11]">
          <Gamepad2 size={23} aria-hidden="true" />
        </div>
        <h1 className="mt-5 mb-0 font-display text-[34px] leading-tight font-[850] tracking-[-0.035em]">
          Minecraft sign-in
        </h1>
        <p className="mt-3 mb-0 min-h-[3.2em] leading-[1.55] text-ink-soft" role="status" aria-live="polite">
          {message}
        </p>
        <div className="mt-7 flex flex-wrap gap-3">
          {state === "error" && token ? (
            <button
              className="inline-flex min-h-11 cursor-pointer items-center justify-center rounded-sm bg-pumpkin px-4 font-[760] text-[#25170f] shadow-[0_5px_0_var(--color-pumpkin-dark),var(--shadow-low)] hover:bg-[#eb7a2d] focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-[#315d3a]"
              onClick={() => void redeem(token)}
            >
              Try again
            </button>
          ) : null}
          {state === "error" ? (
            <a
              className="inline-flex min-h-11 items-center justify-center rounded-sm px-4 font-[760] text-ink underline decoration-[#a39882] underline-offset-4 hover:text-[#5f4734] focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-[#315d3a]"
              href="/login"
            >
              Use Google or Discord
            </a>
          ) : null}
        </div>
      </section>
    </main>
  );
}
