/**
 * What the landing page says, once. The film shows these as type in the world and in the
 * game's interface; the page's own HTML (components/landing/mirror.tsx) says the same in
 * reading order, for screen readers, search engines and anyone the film does not reach.
 * A test holds the two together.
 */

export const COPY = {
  sky: {
    title: ["Your world", "is waiting below"],
    caption: ["Free Minecraft servers", "for you and your friends"],
    cue: "Dig down",
  },
  surface: {
    caption: "You choose the name.",
    names: ["steve", "lena", "bigfarm"],
    host: ".kurbis.host",
    editions: "Different editions. Same pond.",
    tags: ["Java", "Bedrock"],
    chat: ["<Steve> what's the ip", "<You> steve.kurbis.host", "<Steve> that's it?", "<You> that's it"],
  },
  dig: {
    facts: [
      ["Cost: nothing.", "No card. No ads."],
      ["2 GB RAM. 5 GB world.", "One server each."],
    ],
    finds: [
      ["Fossil", "Older than your last world."],
      ["Chest", "Someone's coal. Finders keepers."],
      ["Old sign", "It says: Back in 5 minutes."],
      ["Lost sock", "The other one is in the Nether."],
    ],
  },
  cave: {
    awake: ["Sleeps when you leave.", "Wakes when you join."],
    asleep: ["Nobody home. It sleeps.", "Every block saved."],
    /** What the page's own lever says the state is (the film shows it). */
    now: "The server is awake.",
    lever: {
      awake: ["Pull it: everyone leaves,", "and the server sleeps."],
      asleep: ["Pull it: someone joins,", "and the server wakes."],
    },
  },
  dungeon: {
    headline: "Your server. Your rules.",
    ores: [
      ["Console", "Type a command. Things happen."],
      ["Logs", "Read what happened while you weren't looking."],
      ["Players", "Friends in. Everyone else out."],
      ["Settings", "Survival today. Creative tomorrow."],
      ["Activity", "Who did what, and when."],
    ],
    backups: ["Backups", "Creeper took the house? Ctrl+Z for the whole world."],
  },
  spawner: {
    headline: "Ask the spawner anything.",
    small: "Operators only.",
    asks: [
      ["/ai ask give everyone night vision", "Done now."],
      ["/ai code home add a /home command", "Becomes a real command, no restart."],
      ["/ai revert home", "Oops, undone."],
    ],
  },
  nether: {
    headline: "Pick a name.",
    caption: "Step through to your world.",
    shortcut: "Jump to portal",
  },
  toasts: ["Touched grass", "Dirt enthusiast", "Found the cave", "We need to go deeper"],
} as const;
