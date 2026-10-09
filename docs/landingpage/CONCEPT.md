# Kurbis landing page: "Dig down"

Status: concept and copy draft, not implemented.
Art: [`world.png`](world.png), a 320×720 cross-section used as the scroll track.

## Idea

The page is a vertical cross-section of one Minecraft world. The visitor starts in the sky and descends to the Nether. Each layer carries one short, playful beat. The whole interface is a Minecraft-style game HUD (hotbar, XP bar, F3 screen, chat, advancement toasts; see below), with depth shown as the Y coordinate. The Kurbis pumpkin (Kürbis = pumpkin, a jack-o'-lantern is a light you carry into caves) is the guide.

The cave layer is the centrepiece because it is the actual product mechanism: **a server is awake only while someone is connected, and the world is kept as a snapshot while it sleeps.** The page itself behaves that way: leave the tab idle and the cave goes dark.

## Copy voice

Game-native, short, a bit explorer, playful. Light on mechanics; no feature lists, and the sleep mechanism is not repeated in every section.

## Layers

Order: Sky, Surface, Dirt, Cave, Ores, Spawner, Chest, Diamonds, Bedrock, Nether.

### 1. Sky (Y 320), hero

**Your world is waiting below.**

- Plain line: "Free Minecraft servers for you and your friends. Java and Bedrock."
- Pumpkin mascot drops from a cloud and lands in the grass on scroll. Bubble: "Hi. Kürbis means pumpkin."
- Clouds as small signs; hover shows a joke, e.g. "Weather: clear. Phantoms: none." Clicking a cloud makes it rain for a second. Clouds also part around the cursor.
- Sun that turns into a moon on click; stars at night. Moon joke: "Skip night? Just click the bed."
- Scroll cue: "Dig down ↓". Beside it: "Impatient? Jump straight in." (drops to the portal CTA)

### 2. Surface (Y 64)

**The address is up here. The world is down there.**

- Address sign in the grass types out example names: `steve.kurbis.host`, `lena.kurbis.host`, `bigfarm.kurbis.host`. Under it: "You choose the name." and "Claim yours ↓".
- Chat bubbles by the tree:
    - `<Steve> what's the ip`
    - `<You> steve.kurbis.host`
    - `<Steve> that's it?`
    - `<You> that's it`
- Pond: a Java player and a Bedrock player fishing from opposite banks. Caption: "Different editions. Same pond."
- Sugar cane list of zeros: "Ads: 0. Credit cards asked: 0." (A "queues" line was dropped: capacity queuing exists, so it isn't always true.)
- Achievement toast on arrival: "Achievement get: Touched grass."

### 3. Dirt (Y 50)

All four ideas combined:

1. Explorer's journal that appears while scrolling:
    - Day 1. Dug down. Dirt.
    - Day 2. More dirt.
    - Day 3. Still dirt. Morale: high.
    - Price tag in the dirt: "Cost: nothing. Hidden fees: dirt."
2. Block counter ticking up with scroll: "Blocks mined: 47. Nobody's counting." Achievement toast: "Dirt enthusiast."
3. Buried things to click (fossil, bone, old sign, lost sock), one joke line each, e.g. "Someone's coal. Finders keepers."
4. Signpost: "Free. Java and Bedrock. Same address. Keep digging."

### 4. Cave (Y 20), centrepiece

**It only glows when someone's home.**

- Interaction: "A player joins" lights the torches one by one; "Everyone leaves" runs a sped-up idle countdown, a snapshot flash, then darkness. The dark cave stays intact: dark isn't empty.
- Page idle for a set time (sped-up stand-in for 5 minutes, captioned) turns the cave dark; any input relights it.
- Achievement toast: "Achievement get: Found the cave."

### 5. Ores (Y 0)

**Pickaxe's in your cursor.** All ores on one wall; hover or click shows the line. The cursor becomes a pickaxe.

- Redstone, console: Type a command. Things happen.
- Coal, logs: Read what happened while you weren't looking.
- Gold, players: Friends in. Everyone else out.
- Lapis, settings: Survival today. Creative tomorrow.
- Copper, activity: Who did what, and when.

### 6. Spawner, `/ai` (Y -10), own section

**Ask the spawner anything.**

Chat lines on the wall:

- `/ai ask give everyone night vision` → Done now.
- `/ai code add a /home command` → Becomes a real command, no restart.
- `/ai revert` → Oops, undone.

Caption: Operators only. We cover the AI. No sign-in, no keys.

### 7. Chest (Y -20)

**Creeper took the house? Ctrl+Z for the whole world.**

Sublabel: Backups and restore. Opening the chest lists example rows, labelled as an example (e.g. "Today 21:40", "Yesterday 18:02").

### 8. Diamonds (Y -40)

**Checked it. Twice.** Alternative heading: **Don't take our word for it. Press F3.**

Diamonds glow. The section is a debug screen: current platform stats in an F3-style overlay (monospace, left-aligned, sections like a Minecraft debug screen). Pressing F3 toggles it anywhere on the page; here it is open by default, and each line ticks live.

Mocked stats for the mockup (placeholder values, to be backed by a real public-stats endpoint before launch; never ship invented numbers):

```
Kurbis (public stats)                      refresh 2s
Worlds: 1,284 total, 37 awake, 1,247 asleep
Players online: 112 (Java 79, Bedrock 33)
Wakes today: 418, median wake 9.4s
Snapshots stored: 6,902 (latest 12s ago)
Idle shutdown: 5m00s after last player leaves
Region: single region
Uptime (30d): 99.9%
```

- Hover or click a line for a one-line plain explanation (e.g. "Wakes today: times someone knocked on a sleeping world").
- Under the overlay, the checked claim: "Everyone leaves and it's lights-out after 5 minutes. We timed it."
- Mock data is flagged in the build via a `MOCK` constant; a build check fails the release if mocked values are still wired in.

### 9. Bedrock (Y -64)

**Unbreakable. So is our list of limits.**

Optional sublabel: 2 GiB · 5 GiB · one region · one server each. Clicking bedrock bounces off.

### 10. Nether, the CTA

Heading: **Pick a name. That's the whole signup.**

- Input: `[ your-name ].kurbis.host` with live "Free" / "Taken".
- Button: Copy address. After copy: "Copied. Paste it into Minecraft. Go on."
- Small: Java or Bedrock. Same address.
- Portal: **Already in the game? Step through.** Button: Open dashboard. Small: Sign in with Google / Discord.
- Portal shimmer transition; colours shift red. Falling past the bottom loops to the sky.

## The page is a game HUD

All page chrome is a Minecraft-style heads-up display laid over the world, so the visitor "plays" the page in first person while descending. The HUD is the navigation; there is no conventional header or footer nav. Layout and behaviour follow the familiar game HUD; all art, icons and the pixel font are original (no Mojang textures, fonts or logo).

| HUD element            | Position                     | On this page                                                                                                                                                                                                                                                                        |
| ---------------------- | ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Hotbar**             | bottom centre                | Navigation. One slot per layer group, each with an original icon (pumpkin, dirt, torch, pickaxe, spawner, chest, diamond, bedrock, portal). Selected slot follows scroll. Keys 1-9 and click jump to a layer. Hovering a slot shows an item tooltip: layer name plus one lore line. |
| **XP bar**             | above hotbar                 | Scroll progress. The level number is the current layer number. XP orbs fly to the bar when an achievement fires.                                                                                                                                                                    |
| **Hearts and hunger**  | above hotbar, left and right | Decoration with a gag: hearts are full ("nothing here can hurt you"); clicking bedrock costs half a heart that regenerates. Hunger uses pumpkin icons.                                                                                                                              |
| **Crosshair**          | centre                       | Desktop cursor target. Turns into a pickaxe over ores and dirt blocks, with a crack animation while mining. On touch devices the tap replaces it.                                                                                                                                   |
| **Advancement toasts** | top right                    | "Achievement get: Touched grass / Dirt enthusiast / Found the cave". Each states a true fact or a clear joke.                                                                                                                                                                       |
| **Chat**               | bottom left                  | Receives the toasts' text and in-world jokes as chat lines (fading). `T` or `/` opens the input; a few easter-egg commands reply (`/help`, `/kurbis`). Not required for any function.                                                                                               |
| **Boss bar**           | top centre                   | Appears in the cave layer as the idle countdown ("Server naps in 04:12", sped up in the demo) and disappears after.                                                                                                                                                                 |
| **F3 debug screen**    | top left                     | Toggled with `F3` anywhere. Shows XYZ (Y matches the depth meter), FPS, the current layer, and, open by default in the Diamonds layer, the platform stats (see Diamonds). Replaces the old side Y-meter.                                                                            |
| **Inventory** (`E`)    | centre panel                 | Secondary links: Sign in, Open dashboard, Discord, Not affiliated with Mojang or Microsoft.                                                                                                                                                                                         |
| **Game menu** (`Esc`)  | centre panel                 | Back to game, Jump to portal, Options: reduce motion, sound (off by default), night. This is where accessibility toggles live.                                                                                                                                                      |

Rules for the HUD:

- Everything is real, focusable DOM (buttons, a `nav` for the hotbar, a labelled live region for chat). Keyboard shortcuts are shortcuts only; every action has a visible control.
- HUD never covers the layer's content: it hugs the viewport edges, and text over art keeps AA contrast with a pixel shadow.
- Mobile: hotbar stays, shrunk; XP bar stays; hearts, hunger, chat and F3 collapse into the inventory and menu. Crosshair is hidden.
- `prefers-reduced-motion`: no crack animation, particles or orbs; toasts appear instantly; the HUD itself is static.
- Name availability ("Free" / "Taken") in the Nether CTA needs a public check endpoint; confirm the behaviour.

## Technical direction (proposal)

- Next.js 16 App Router, route group `(marketing)`, server component page with the existing `getSessionOrNull` redirect to `/app` for signed-in users.
