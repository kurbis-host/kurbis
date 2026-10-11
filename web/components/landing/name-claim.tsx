"use client";

import { Check, Copy } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { ROOT_DOMAIN } from "../../lib/landing/constants";
import { checkName, validateName, type NameStatus } from "../../lib/landing/name-check";

const ORBS = [0, 1, 2, 3, 4, 5, 6, 7];

/** `[ your-name ].kurbis.host` with a live availability check. */
export function NameClaim() {
  const inputId = useId();
  const statusId = useId();
  const [name, setName] = useState("");
  const [status, setStatus] = useState<NameStatus>("empty");
  const [copied, setCopied] = useState(false);
  const problem = validateName(name);

  useEffect(() => {
    setCopied(false);
    if (!name) {
      setStatus("empty");
      return;
    }
    if (validateName(name)) {
      setStatus("invalid");
      return;
    }
    setStatus("checking");
    const controller = new AbortController();
    const timer = setTimeout(() => {
      checkName(name, controller.signal).then(setStatus, () => undefined);
    }, 250);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [name]);

  const address = `${name}.${ROOT_DOMAIN}`;

  async function copy() {
    try {
      await navigator.clipboard.writeText(address);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="relative">
      <label htmlFor={inputId} className="sr-only">
        Server name
      </label>
      <div className="flex items-center rounded-[10px] border-2 border-ink bg-white font-mono text-[16px] focus-within:outline-3 focus-within:outline-offset-2 focus-within:outline-focus">
        <input
          id={inputId}
          value={name}
          onChange={(event) => setName(event.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "").slice(0, 32))}
          placeholder="your-name"
          autoComplete="off"
          autoCapitalize="none"
          spellCheck={false}
          aria-describedby={statusId}
          aria-invalid={status === "invalid" || status === "taken"}
          className="min-h-12 w-0 min-w-0 flex-1 rounded-l-[8px] bg-transparent px-3 text-right font-bold text-ink outline-none placeholder:font-normal placeholder:text-stone-dark"
        />
        <span className="shrink-0 pr-3 text-ink-soft">.{ROOT_DOMAIN}</span>
      </div>

      <p id={statusId} role="status" className="mt-2 mb-0 min-h-[22px] text-[14px] font-[700]">
        {status === "invalid" ? <span className="text-danger">{problem}</span> : null}
        {status === "checking" ? <span className="text-ink-soft">Checking…</span> : null}
        {status === "free" ? <span className="text-success">Free. {address} is yours for the taking.</span> : null}
        {status === "taken" ? <span className="text-danger">Taken. Someone got there first.</span> : null}
      </p>

      <div className="mt-2 flex flex-wrap items-center gap-3">
        <span className="relative">
          <a
            href="/login"
            className="inline-flex min-h-12 items-center rounded-[9px] bg-pumpkin px-[18px] font-[800] text-[#25170f] no-underline shadow-[0_5px_0_var(--color-pumpkin-dark)]"
          >
            Claim it
          </a>
          {status === "free" ? (
            <span key={name} aria-hidden="true" className="pointer-events-none absolute top-1/2 left-1/2">
              {ORBS.map((orb) => (
                <span
                  key={orb}
                  className="absolute size-2.5 animate-[xp-orb_0.9s_ease-in_both] rounded-full bg-[#b6f24a] shadow-[0_0_8px_2px_rgba(160,240,60,0.8)]"
                  style={
                    {
                      "--orb-x": `${Math.round(Math.cos(orb * 0.8) * 110)}px`,
                      "--orb-y": `${-50 - (orb % 3) * 28}px`,
                      animationDelay: `${orb * 45}ms`,
                    } as React.CSSProperties
                  }
                />
              ))}
            </span>
          ) : null}
        </span>
        <button
          type="button"
          onClick={copy}
          disabled={status !== "free"}
          className="inline-flex min-h-12 cursor-pointer items-center gap-2 rounded-[9px] border-0 bg-[#eee7d8] px-[18px] font-[800] text-ink shadow-[0_5px_0_#cfc5ae] disabled:cursor-not-allowed disabled:opacity-50"
        >
          {copied ? <Check size={17} aria-hidden="true" /> : <Copy size={17} aria-hidden="true" />} Copy address
        </button>
      </div>
      <p role="status" className="mt-3 mb-0 min-h-[20px] text-[14px] text-ink-soft">
        {copied ? "Copied. Paste it into Minecraft. Go on." : "Java or Bedrock. Same address."}
      </p>

    </div>
  );
}
