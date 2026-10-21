import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ColumnChart, TimeChart } from "../../components/charts";

/** jsdom lays nothing out; report a fixed width the way a real ResizeObserver would. */
class SizedResizeObserver {
  constructor(private callback: ResizeObserverCallback) {}
  observe(target: Element) {
    this.callback([{ target, contentRect: { width: 480 } } as unknown as ResizeObserverEntry], this as unknown as ResizeObserver);
  }
  unobserve() {}
  disconnect() {}
}

beforeEach(() => vi.stubGlobal("ResizeObserver", SizedResizeObserver));
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const count = (value: number) => `${value} servers`;

describe("TimeChart", () => {
  it("says so when there are no points", () => {
    render(<TimeChart title="Servers online" points={[]} format={count} gapMs={60_000} />);
    expect(screen.getByText("No samples in this range.")).toBeVisible();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByText("—")).toBeVisible();
  });

  it("treats points without values as empty and accepts its own empty text", () => {
    render(
      <TimeChart
        title="Average TPS"
        points={[{ at: "2026-10-21T10:00:00Z", value: null }, { at: "2026-10-21T10:01:00Z", value: null }]}
        format={count}
        gapMs={60_000}
        emptyText="No TPS reported."
      />,
    );
    expect(screen.getByText("No TPS reported.")).toBeVisible();
  });

  it("draws the line, names the latest value and moves a crosshair with the arrow keys", () => {
    const points = [
      { at: "2026-10-21T10:00:00Z", value: 2 },
      { at: "2026-10-21T10:01:00Z", value: 5 },
      { at: "2026-10-21T10:02:00Z", value: null },
      { at: "2026-10-21T10:03:00Z", value: 3 },
    ];
    render(<TimeChart title="Servers online" points={points} format={count} gapMs={90_000} />);
    const chart = screen.getByRole("img", { name: "Servers online: latest 3 servers" });
    expect(chart).toHaveAttribute("width", "480");
    // The null splits the series into two runs, each a filled area plus a line.
    expect(chart.querySelectorAll("path[stroke]")).toHaveLength(2);

    fireEvent.keyDown(chart, { key: "ArrowLeft" });
    expect(screen.getByText("3 servers", { selector: "strong" })).toBeVisible();
    fireEvent.keyDown(chart, { key: "ArrowLeft" });
    expect(screen.getByText("No sample")).toBeVisible();
    fireEvent.blur(chart);
    expect(screen.queryByText("No sample")).not.toBeInTheDocument();
  });

  it("does not join points further apart than the gap", () => {
    const points = [
      { at: "2026-10-21T10:00:00Z", value: 1 },
      { at: "2026-10-21T10:01:00Z", value: 2 },
      { at: "2026-10-21T11:00:00Z", value: 3 },
    ];
    render(<TimeChart title="Players" points={points} format={count} gapMs={90_000} />);
    expect(screen.getByRole("img").querySelectorAll("path[stroke]")).toHaveLength(2);
  });
});

describe("ColumnChart", () => {
  const columns = [
    { label: "Oct 19", value: 4 },
    { label: "Oct 20", value: 0 },
    { label: "Oct 21", value: 12 },
  ];

  it("renders one focusable column per item, labelled with its value", () => {
    render(<ColumnChart title="Signups" columns={columns} format={(value) => `${value} signups`} total="16" />);
    const group = screen.getByRole("group", { name: "Signups" });
    const bars = within(group).getAllByRole("img");
    expect(bars.map((bar) => bar.getAttribute("aria-label"))).toEqual([
      "Oct 19: 4 signups",
      "Oct 20: 0 signups",
      "Oct 21: 12 signups",
    ]);
    for (const bar of bars) expect(bar).toHaveAttribute("tabindex", "0");
    // A zero column keeps its hit target but draws no bar.
    expect(bars[1].querySelector("path")).toBeNull();
    expect(bars[0].querySelector("path")).not.toBeNull();
    expect(screen.getByText("16")).toBeVisible();
  });

  it("shows a tooltip for the focused column", () => {
    render(<ColumnChart title="Signups" columns={columns} format={(value) => `${value} signups`} />);
    const bar = screen.getByRole("img", { name: "Oct 21: 12 signups" });
    fireEvent.focus(bar);
    expect(screen.getByText("12 signups")).toBeVisible();
    fireEvent.blur(bar);
    expect(screen.queryByText("12 signups")).not.toBeInTheDocument();
  });

  it("draws nothing until it knows its width", () => {
    vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
    render(<ColumnChart title="Signups" columns={columns} format={String} />);
    expect(screen.queryByRole("group")).not.toBeInTheDocument();
    expect(screen.getByText("Signups")).toBeVisible();
  });
});
