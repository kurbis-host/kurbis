export interface MockStats {
  awake: number;
  sleeping: number;
  players: number;
  snapshots: number;
}

/** Mocked numbers for the F3 overlay; wobble a little so it looks alive. */
export function mockStats(tick: number): MockStats {
  const wobble = Math.round(Math.sin(tick / 3) * 2);
  return { awake: 14 + wobble, sleeping: 212 - wobble, players: 37 + wobble * 2, snapshots: 1284 + tick };
}
