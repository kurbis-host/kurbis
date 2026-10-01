import Link from "next/link";

export default function ServerRemoved() {
  return <main className="grid min-h-screen place-items-center bg-canvas px-4 py-12 text-ink"><section className="w-full max-w-[480px] rounded-[18px] border border-[#e2d9c7] bg-paper p-8 shadow-[var(--shadow-low)]"><h1 className="m-0 font-display text-3xl font-[850]">Server removal requested</h1><p className="mt-3 leading-relaxed text-ink-soft">This server session no longer has access. Kurbis will finish removing the server through its lifecycle service.</p><Link className="mt-5 inline-flex min-h-11 items-center rounded-sm bg-pumpkin px-4 font-[760] text-[#25170f] no-underline hover:bg-[#eb7a2d] focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-focus" href="/">Kurbis home</Link></section></main>;
}
