export default function Loading() {
  return (
    <div
      className="grid min-h-[50vh] place-items-center content-center gap-[18px] text-ink-soft"
      aria-live="polite"
    >
      <span
        className="size-10 animate-[float-cube_1.6s_ease-in-out_infinite] rounded-sm bg-pumpkin shadow-[inset_-7px_-7px_0_var(--color-pumpkin-dark)]"
        aria-hidden="true"
      />
      <p className="m-0 font-bold">Loading your world…</p>
    </div>
  );
}
