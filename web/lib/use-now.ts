import { useEffect, useState } from "react";

/**
 * The current time for relative labels. It starts at the server render time
 * so hydration matches, then ticks on the client.
 */
export function useNow(renderedAt: number, intervalMs = 30_000) {
  const [now, setNow] = useState(renderedAt);
  useEffect(() => {
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(timer);
  }, [intervalMs]);
  return now;
}
