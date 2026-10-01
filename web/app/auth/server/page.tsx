"use client";

import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Gamepad2 } from "lucide-react";

type State = "reading" | "redeeming" | "error";

export default function ServerLoginPage() {
  const started = useRef(false);
  const [token, setToken] = useState<string | null>(null);
  const [state, setState] = useState<State>("reading");
  const [message, setMessage] = useState("Checking your server access link…");
  const [retryable, setRetryable] = useState(false);

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    const token = new URLSearchParams(window.location.hash.slice(1)).get("token");
    window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    if (!token) {
      setState("error");
      setMessage("This link has no access token. Run /kurbis login in game to create another.");
      return;
    }
    setToken(token);
    void redeem(token);
  }, []);

  async function redeem(value: string) {
    setState("redeeming");
    setRetryable(false);
    setMessage("Opening the server panel…");
    try {
      const response = await fetch("/auth/server/redeem", {
        method: "POST",
        credentials: "same-origin",
        headers: { accept: "application/json", "content-type": "application/json" },
        body: JSON.stringify({ token: value }),
      });
      if (!response.ok) {
        let detail = "This link is invalid, expired, or has already been used.";
        try {
          const payload = (await response.json()) as { message?: string };
          if (payload.message) detail = payload.message;
        } catch {}
        if (response.status === 400 || response.status === 401) {
          setState("error");
          setMessage(`${detail}. Join the server and run /kurbis login to request a fresh link.`);
          return;
        }
        throw new Error(detail);
      }
      const result = (await response.json()) as { server_id?: string };
      if (!result.server_id || !/^[0-9a-f-]{36}$/i.test(result.server_id)) {
        setState("error");
        setMessage("The server access response was invalid. Join the server and run /kurbis login to request a fresh link.");
        return;
      }
      window.location.replace(`/manage/${encodeURIComponent(result.server_id)}`);
    } catch (error) {
      setState("error");
      setRetryable(true);
      setMessage(error instanceof Error ? error.message : "Could not open the server panel. Check your connection and try again.");
    }
  }

  return (
    <main className="grid min-h-[calc(100svh-64px)] place-items-center bg-canvas px-4 py-12 text-ink">
      <section className="w-full max-w-[440px] rounded-[18px] border border-[#e2d9c7] bg-paper p-7 shadow-[var(--shadow-low)] sm:p-9">
        <a className="inline-flex min-h-10 items-center gap-2 text-sm font-[700] text-ink-soft underline decoration-[#a39882] underline-offset-4 hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-focus" href="/">
          <ArrowLeft size={16} /> Kurbis home
        </a>
        <div className="mt-9 grid size-12 place-items-center rounded-[12px] bg-pumpkin text-[#2d1b11]"><Gamepad2 size={23} aria-hidden="true" /></div>
        <h1 className="mt-5 mb-0 font-display text-[34px] leading-tight font-[850] tracking-[-0.035em]">Server access</h1>
        <p className="mt-3 mb-0 min-h-[3.2em] leading-[1.55] text-ink-soft" role="status" aria-live="polite">{message}</p>
        {state === "error" && retryable && token ? <button className="mt-6 inline-flex min-h-11 cursor-pointer items-center justify-center rounded-sm bg-pumpkin px-4 font-[760] text-[#25170f] shadow-[0_5px_0_var(--color-pumpkin-dark),var(--shadow-low)] hover:bg-[#eb7a2d] focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-focus" onClick={() => void redeem(token)}>Try again</button> : null}
        <p className="mt-7 mb-0 text-xs leading-relaxed text-ink-soft">This link opens one hosted server panel. It does not sign in to a Kurbis account.</p>
      </section>
    </main>
  );
}
