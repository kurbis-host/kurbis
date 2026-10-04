import { notFound } from "next/navigation";
import { controlPlaneOrigin } from "../../../../lib/control-plane";

type PublicPreviewStatus = {
  hostname: string;
  commit_sha: string;
  kind: "pull_request" | "branch" | "commit";
  state:
    | "lobby"
    | "queued"
    | "fetching"
    | "patching"
    | "compiling"
    | "packaging"
    | "booting"
    | "ready"
    | "running"
    | "stopping"
    | "failed"
    | "expired";
  expires_at: string;
};

const publicStatusPattern =
  /^(?:pr-[1-9][0-9]*|commit-[a-f0-9]{40}|branch-[a-z0-9-]+-[a-f0-9]{12})\.pumpkin\.kurbis\.host$/;

export const dynamic = "force-dynamic";

export default async function CiPreviewStatusPage({
  params,
}: {
  params: Promise<{ hostname: string }>;
}) {
  const { hostname } = await params;
  if (!publicStatusPattern.test(hostname)) notFound();

  let response: Response;
  try {
    const url = new URL(
      `/public/ci/previews/${encodeURIComponent(hostname)}`,
      controlPlaneOrigin(),
    );
    response = await fetch(url, {
      cache: "no-store",
      signal: AbortSignal.timeout(5000),
    });
  } catch {
    return (
      <main className="mx-auto min-h-screen max-w-3xl px-6 py-16 text-white">
        <p className="text-sm uppercase tracking-[0.2em] text-amber-300">Pumpkin CI preview</p>
        <h1 className="mt-3 text-3xl font-semibold">Status temporarily unavailable</h1>
        <p className="mt-4 break-all text-slate-300">{hostname}</p>
        <p className="mt-6 text-slate-400">Try again shortly. This page exposes status only; it does not provide runtime controls or build logs.</p>
      </main>
    );
  }
  if (response.status === 404) notFound();
  if (!response.ok) {
    return (
      <main className="mx-auto min-h-screen max-w-3xl px-6 py-16 text-white">
        <p className="text-sm uppercase tracking-[0.2em] text-amber-300">Pumpkin CI preview</p>
        <h1 className="mt-3 text-3xl font-semibold">Status temporarily unavailable</h1>
        <p className="mt-4 break-all text-slate-300">{hostname}</p>
        <p className="mt-6 text-slate-400">Try again shortly. This page exposes status only; it does not provide runtime controls or build logs.</p>
      </main>
    );
  }
  const status = (await response.json()) as PublicPreviewStatus;

  return (
    <main className="mx-auto min-h-screen max-w-3xl px-6 py-16 text-white">
      <p className="text-sm uppercase tracking-[0.2em] text-emerald-300">Pumpkin CI preview</p>
      <h1 className="mt-3 text-3xl font-semibold">Build and preview status</h1>
      <dl className="mt-8 grid gap-5 rounded-2xl border border-white/10 bg-white/[0.04] p-6 sm:grid-cols-2">
        <div>
          <dt className="text-sm text-slate-400">State</dt>
          <dd className="mt-1 text-lg font-medium capitalize">{status.state.replaceAll("_", " ")}</dd>
        </div>
        <div>
          <dt className="text-sm text-slate-400">Preview type</dt>
          <dd className="mt-1 text-lg font-medium capitalize">{status.kind.replaceAll("_", " ")}</dd>
        </div>
        <div className="sm:col-span-2">
          <dt className="text-sm text-slate-400">Minecraft address</dt>
          <dd className="mt-1 break-all font-mono text-sm">{status.hostname}</dd>
        </div>
        <div className="sm:col-span-2">
          <dt className="text-sm text-slate-400">Commit</dt>
          <dd className="mt-1 break-all font-mono text-sm">{status.commit_sha}</dd>
        </div>
        <div className="sm:col-span-2">
          <dt className="text-sm text-slate-400">Preview metadata expires</dt>
          <dd className="mt-1">{new Date(status.expires_at).toLocaleString("en-US", { timeZone: "UTC", timeZoneName: "short" })}</dd>
        </div>
      </dl>
      <p className="mt-6 text-sm text-slate-400">This is a read-only status page. It does not expose build logs, capabilities, or runtime controls.</p>
    </main>
  );
}
