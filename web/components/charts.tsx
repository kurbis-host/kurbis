"use client";

import { useEffect, useId, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";

/** One series' colour (validated against the paper surface with the dataviz palette check). */
const SERIES = "#2a78d6";
const GRID = "#e6dfcf";
const HEIGHT = 132;
const PAD = { top: 10, right: 8, bottom: 22, left: 44 };

function useWidth() {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.floor(entry.contentRect.width)));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width] as const;
}

/** Round the top of an axis up to a clean number: 1, 2 or 5 times a power of ten. */
function niceMax(value: number) {
  if (value <= 0) return 1;
  const power = 10 ** Math.floor(Math.log10(value));
  return ([1, 2, 5, 10].find((step) => step * power >= value) ?? 10) * power;
}

const compact = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

function ChartFrame({ title, value, children }: { title: string; value?: ReactNode; children: ReactNode }) {
  return (
    <figure className="m-0 min-w-0">
      <figcaption className="mb-1.5 flex items-baseline justify-between gap-3">
        <span className="text-[13px] font-[760] text-ink">{title}</span>
        {value !== undefined ? <span className="text-[13px] font-[700] text-ink-soft">{value}</span> : null}
      </figcaption>
      {children}
    </figure>
  );
}

function Tip({ x, width, children }: { x: number; width: number; children: ReactNode }) {
  const left = Math.min(Math.max(x, 70), Math.max(width - 70, 70));
  return (
    <div
      className="pointer-events-none absolute top-0 z-10 -translate-x-1/2 rounded-sm bg-[#302b25] px-2.5 py-1.5 text-center text-xs whitespace-nowrap text-[#f8f1df] shadow-high"
      style={{ left }}
    >
      {children}
    </div>
  );
}

function YAxis({ max, format, plotWidth, scaleY }: { max: number; format: (value: number) => string; plotWidth: number; scaleY: (value: number) => number }) {
  return (
    <>
      {[0, max / 2, max].map((tick) => (
        <g key={tick}>
          <line x1={PAD.left} x2={PAD.left + plotWidth} y1={scaleY(tick)} y2={scaleY(tick)} stroke={GRID} strokeWidth={1} />
          <text x={PAD.left - 6} y={scaleY(tick) + 4} textAnchor="end" className="fill-[#6f6c62] text-[10px] tabular-nums">{format(tick)}</text>
        </g>
      ))}
    </>
  );
}

export type TimePoint = { at: string; value: number | null };

/**
 * A line over time with a crosshair: the pointer (or the arrow keys) picks the nearest
 * point and the tooltip says when and how much. Points further apart than `gapMs` are
 * not joined, so a stretch nobody sampled shows as a gap.
 */
export function TimeChart({
  title,
  points,
  format,
  axisFormat = (value) => compact.format(value),
  max,
  gapMs,
  emptyText = "No samples in this range.",
}: {
  title: string;
  points: TimePoint[];
  format: (value: number) => string;
  axisFormat?: (value: number) => string;
  /** A fixed top for the axis (20 for TPS, 100 for a share); otherwise the data's. */
  max?: number;
  gapMs: number;
  emptyText?: string;
}) {
  const [ref, width] = useWidth();
  const [hover, setHover] = useState<number | null>(null);
  const titleId = useId();
  const values = points.flatMap((point) => (point.value === null ? [] : [point.value]));
  const latest = [...points].reverse().find((point) => point.value !== null);
  const plotWidth = Math.max(width - PAD.left - PAD.right, 1);
  const plotHeight = HEIGHT - PAD.top - PAD.bottom;
  const times = points.map((point) => new Date(point.at).getTime());
  const start = times[0] ?? 0;
  const end = times.at(-1) ?? 1;
  const top = max ?? niceMax(Math.max(...values, 0));
  const scaleX = (time: number) => PAD.left + (end === start ? plotWidth / 2 : ((time - start) / (end - start)) * plotWidth);
  const scaleY = (value: number) => PAD.top + plotHeight - (Math.min(value, top) / top) * plotHeight;

  const runs: string[] = [];
  let run = "";
  points.forEach((point, index) => {
    const joined = index > 0 && points[index - 1].value !== null && times[index] - times[index - 1] <= gapMs;
    if (point.value === null) {
      if (run) runs.push(run);
      run = "";
      return;
    }
    const xy = `${scaleX(times[index]).toFixed(1)},${scaleY(point.value).toFixed(1)}`;
    if (!joined && run) {
      runs.push(run);
      run = "";
    }
    run += run ? ` L${xy}` : `M${xy}`;
  });
  if (run) runs.push(run);

  function nearest(clientX: number, element: Element) {
    const x = clientX - element.getBoundingClientRect().left;
    let best = 0;
    times.forEach((time, index) => {
      if (Math.abs(scaleX(time) - x) < Math.abs(scaleX(times[best]) - x)) best = index;
    });
    return best;
  }

  const hovered = hover !== null ? points[hover] : null;
  const timeFormat = new Intl.DateTimeFormat(undefined, end - start > 2 * 86_400_000
    ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
    : { hour: "2-digit", minute: "2-digit" });

  return (
    <ChartFrame title={title} value={latest?.value !== undefined && latest.value !== null ? format(latest.value) : "—"}>
      <div ref={ref} className="relative" style={{ height: HEIGHT }}>
        {points.length === 0 || values.length === 0 ? (
          <p className="m-0 grid h-full place-items-center rounded-sm bg-[#f4eee1] text-xs text-ink-soft">{emptyText}</p>
        ) : width > 0 ? (
          <>
            <svg
              width={width}
              height={HEIGHT}
              role="img"
              aria-labelledby={titleId}
              tabIndex={0}
              className="block touch-none outline-none focus-visible:outline-3 focus-visible:outline-focus"
              onPointerMove={(event: PointerEvent<SVGSVGElement>) => setHover(nearest(event.clientX, event.currentTarget))}
              onPointerLeave={() => setHover(null)}
              onBlur={() => setHover(null)}
              onKeyDown={(event: KeyboardEvent<SVGSVGElement>) => {
                if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
                event.preventDefault();
                const step = event.key === "ArrowLeft" ? -1 : 1;
                setHover((current) => Math.min(Math.max((current ?? points.length) + step, 0), points.length - 1));
              }}
            >
              <title id={titleId}>{`${title}: latest ${latest?.value !== null && latest ? format(latest.value) : "unknown"}`}</title>
              <YAxis max={top} format={axisFormat} plotWidth={plotWidth} scaleY={scaleY} />
              {runs.map((path) => (
                <g key={path}>
                  <path d={`${path} V${scaleY(0)} H${path.slice(1).split(",")[0]} Z`} fill={SERIES} opacity={0.1} />
                  <path d={path} fill="none" stroke={SERIES} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
                </g>
              ))}
              <text x={PAD.left} y={HEIGHT - 6} className="fill-[#6f6c62] text-[10px]">{timeFormat.format(start)}</text>
              <text x={PAD.left + plotWidth} y={HEIGHT - 6} textAnchor="end" className="fill-[#6f6c62] text-[10px]">{timeFormat.format(end)}</text>
              {hover !== null && hovered ? (
                <g>
                  <line x1={scaleX(times[hover])} x2={scaleX(times[hover])} y1={PAD.top} y2={PAD.top + plotHeight} stroke="#8d877a" strokeWidth={1} />
                  {hovered.value !== null ? <circle cx={scaleX(times[hover])} cy={scaleY(hovered.value)} r={4} fill={SERIES} stroke="#fffaf0" strokeWidth={2} /> : null}
                </g>
              ) : null}
            </svg>
            {hover !== null && hovered ? (
              <Tip x={scaleX(times[hover])} width={width}>
                <strong className="block text-[13px]">{hovered.value === null ? "No sample" : format(hovered.value)}</strong>
                <span className="text-[#c9c1b1]">{timeFormat.format(times[hover])}</span>
              </Tip>
            ) : null}
          </>
        ) : null}
      </div>
    </ChartFrame>
  );
}

export type Column = { label: string; value: number };

/** One column per category (a day, a week), each its own hover and focus target. */
export function ColumnChart({
  title,
  columns,
  format,
  axisFormat = (value) => compact.format(value),
  total,
}: {
  title: string;
  columns: Column[];
  format: (value: number) => string;
  axisFormat?: (value: number) => string;
  /** Shown beside the title, e.g. the sum over the columns. */
  total?: ReactNode;
}) {
  const [ref, width] = useWidth();
  const [hover, setHover] = useState<number | null>(null);
  const plotWidth = Math.max(width - PAD.left - PAD.right, 1);
  const plotHeight = HEIGHT - PAD.top - PAD.bottom;
  const top = niceMax(Math.max(...columns.map((column) => column.value), 0));
  const slot = plotWidth / Math.max(columns.length, 1);
  const barWidth = Math.max(Math.min(slot - 2, 24), 1);
  const scaleY = (value: number) => PAD.top + plotHeight - (value / top) * plotHeight;
  const x = (index: number) => PAD.left + index * slot + (slot - barWidth) / 2;

  return (
    <ChartFrame title={title} value={total}>
      <div ref={ref} className="relative" style={{ height: HEIGHT }}>
        {width > 0 ? (
          <>
            <svg width={width} height={HEIGHT} role="group" aria-label={title} className="block">
              <YAxis max={top} format={axisFormat} plotWidth={plotWidth} scaleY={scaleY} />
              {columns.map((column, index) => {
                const height = Math.max(scaleY(0) - scaleY(column.value), column.value > 0 ? 1 : 0);
                const radius = Math.min(4, barWidth / 2, height);
                const left = x(index);
                const y = scaleY(0) - height;
                const d = `M${left},${scaleY(0)} V${y + radius} Q${left},${y} ${left + radius},${y} H${left + barWidth - radius} Q${left + barWidth},${y} ${left + barWidth},${y + radius} V${scaleY(0)} Z`;
                return (
                  <g
                    key={column.label}
                    tabIndex={0}
                    role="img"
                    aria-label={`${column.label}: ${format(column.value)}`}
                    className="outline-none focus-visible:[&>rect]:stroke-focus"
                    onPointerEnter={() => setHover(index)}
                    onPointerLeave={() => setHover(null)}
                    onFocus={() => setHover(index)}
                    onBlur={() => setHover(null)}
                  >
                    <rect x={PAD.left + index * slot} y={PAD.top} width={slot} height={plotHeight} fill="transparent" strokeWidth={2} />
                    {height > 0 ? <path d={d} fill={SERIES} opacity={hover === null || hover === index ? 1 : 0.55} /> : null}
                  </g>
                );
              })}
              {columns.length > 0 ? (
                <>
                  <text x={PAD.left} y={HEIGHT - 6} className="fill-[#6f6c62] text-[10px]">{columns[0].label}</text>
                  <text x={PAD.left + plotWidth} y={HEIGHT - 6} textAnchor="end" className="fill-[#6f6c62] text-[10px]">{columns.at(-1)!.label}</text>
                </>
              ) : null}
            </svg>
            {hover !== null ? (
              <Tip x={x(hover) + barWidth / 2} width={width}>
                <strong className="block text-[13px]">{format(columns[hover].value)}</strong>
                <span className="text-[#c9c1b1]">{columns[hover].label}</span>
              </Tip>
            ) : null}
          </>
        ) : null}
      </div>
    </ChartFrame>
  );
}
