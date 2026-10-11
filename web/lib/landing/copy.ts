export const CLOUD_JOKES = [
  "Weather: clear. Phantoms: none.",
  "Forecast: 100% chance of blocks.",
  "This cloud has no opinion on creepers.",
];

export const SUN_JOKE = "Skip night? Just click the bed.";

export const ADDRESS_EXAMPLES = ["steve", "lena", "bigfarm"];

export const CHAT_LINES: { who: "Steve" | "You"; text: string }[] = [
  { who: "Steve", text: "what's the ip" },
  { who: "You", text: "steve.kurbis.host" },
  { who: "Steve", text: "that's it?" },
  { who: "You", text: "that's it" },
];

export const JOURNAL = [
  "Day 1. Dug down. Dirt.",
  "Day 2. More dirt.",
  "Day 3. Still dirt. Morale: high.",
];

export const BURIED = [
  { id: "fossil", label: "Fossil", line: "Fossil. Older than your last world." },
  { id: "bone", label: "Bone", line: "A bone. Somebody's very good boy was here." },
  { id: "sign", label: "Old sign", line: "Old sign. It says: “Back in 5 minutes”." },
  { id: "sock", label: "Lost sock", line: "A lost sock. The other one is in the Nether." },
  { id: "coal", label: "Coal", line: "Someone's coal. Finders keepers." },
] as const;

export type BuriedId = (typeof BURIED)[number]["id"];

export const ORES = [
  { id: "redstone", label: "Redstone", topic: "Console", line: "Type a command. Things happen." },
  { id: "coal", label: "Coal", topic: "Logs", line: "Read what happened while you weren't looking." },
  { id: "gold", label: "Gold", topic: "Players", line: "Friends in. Everyone else out." },
  { id: "lapis", label: "Lapis", topic: "Settings", line: "Survival today. Creative tomorrow." },
  { id: "copper", label: "Copper", topic: "Activity", line: "Who did what, and when." },
] as const;

export type OreId = (typeof ORES)[number]["id"];

export const SPAWNER_LINES = [
  { cmd: "/ai ask give everyone night vision", reply: "Done now." },
  { cmd: "/ai code add a /home command", reply: "Becomes a real command, no restart." },
  { cmd: "/ai revert", reply: "Oops, undone." },
];

export const BACKUP_ROWS = [
  { when: "Today 21:40", note: "Before the creeper" },
  { when: "Yesterday 18:02", note: "Nice house" },
  { when: "Mon 09:15", note: "Fresh world" },
];
