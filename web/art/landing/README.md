# Landing v2 art workspace

Source art for the scroll-driven landing page (`web/app/(marketing)/page.tsx`; its code is in `web/lib/landing/` and `web/components/landing/`). The look follows the Minecraft trailer references: hard-edged voxels, flat trailer-style textures, golden-hour light, haze, bloom, and chunky extruded type that lives in the world.

The page is one continuous take from the sky through the Nether and home again, 6.8 screens of scroll; then the credits roll, and the page begins again (8.4 screens in all). Scroll drives the camera and the story; life and reactions run in real time on top.

| Path | Purpose |
| --- | --- |
| `scripts/film.py` | The film. `build(session)` makes every set, actor, light and curve for one way of scrolling through the page and `save()` writes `blend/film_<session>.blend`. Run in Blender (Blender MCP, or `blender -b -P scripts/film.py`). Rebuilds from scratch every run. |
| `scripts/render_range.py` | Renders a frame range of a session's blend file headless. Run several ranges side by side. |
| `scripts/label_frames.py` | Burns the part of the journey and the scroll position into rendered frames. |
| `scripts/scan_frames.py` | Finds hard cuts and frozen frames in a rendered clip. |
| `scripts/audit.py` | The legibility audit: every line of type in frame, inside the safe area, large enough, not covered. `film.check()` runs it at every rest. |
| `scripts/sheet.py` | Tiles frames into a contact sheet, to read a stretch of motion as stills. |
| `scripts/export_web.py` | Exports the film for the browser (see "The browser build"). |
| `scripts/web_frames.mjs`, `scripts/compare_web.py` | Capture the page's frames at given scroll positions, and lay them beside the previs's. |
| `scripts/gate0.py` | The earlier look-dev stills (superseded). Uses `kurbis/world_stills.py`. |
| `scripts/kurbis/` | The toolkit (see below). |
| `ref/` | Reference material and the third-party texture pack. Git-ignored. |
| `blend/`, `export/`, `renders/` | Generated. Git-ignored. |

## Toolkit (`scripts/kurbis`)

- `blocks.py`: texture atlas (colour + emission), block registry, the shared voxel material.
- `voxel.py`: numpy mesher. A 3D id grid becomes culled, atlas-mapped quads.
- `world.py`: the valley, cut open at `y = 0`: the short dig on the cut face, the cave the cut face opens, the cart tunnel behind it, the homestead's cavern (still `dungeon` in `SPOTS`), and the slab of ground that hides the cut until the slice.
- `sets.py`: the Nether, its own grid placed far from the valley.
- `anim.py`: the timeline. See "Two clocks" below.
- `life.py`: what moves by itself. Wind, water swell and drifting flakes are geometry-node modifiers on the scene clock (previs only; the browser does them as shaders). Fishing and leaping fish are keyed loops.
- `hud.py`: the Minecraft-style interface, drawn on cards fixed in front of the lens in interface pixels (480 across): chat lines, toasts, tooltips, buttons, and the previs pointer. Also the red bed as the game's inventory draws it (`bed`), the browser's toast icon for the cave falling asleep, and the shield (`shield`, at twice the interface's scale, as the game draws a model), the icon for turning Herobrine away at the homestead's gate: the pack has no picture of either.
- `font.py`: "Kurbis Block", our own voxel display face, for copy in the world (with a "%" for the splash); and a small five-by-seven pixel face with lower case, for the interface, whose g, j, p, q and y hang a row below the line.
- `mascot.py`: the pumpkin and its faces (our own pixel art).
- `boxmodel.py`: box-UV models: players (rigid parts with elbow and knee pivots), held items, a pumpkin to wear on the head (`pumpkin`: carved, or lit), torch, lantern, lectern, chest, cart, lever, sign, rails, fish, plant sprites, and mobs as the game models them (creeper, strider, chicken, baby zombie, and `seat` for a rider).
- `scenery.py`: clouds, drifting leaves, rain, ripple rings, the snapshot block, the scrim behind type.
- `look.py`: sky, lights, haze, camera, full-frame covers, compositor (bloom, vignette, lens wobble), render settings.
- `export.py`: what the browser gets: the worlds as grids of blocks with the suns' shadows on them, small models as a glb, and what the toolkit noted about each thing (`core.note`). It also holds the meshing and light baking the page repeats (`faces`, `levels`, `light`): the reference the page's own is tested against.

## Two clocks

Positions on the timeline are scroll positions in screens (one screen is one viewport height of scroll). `PART` in `film.py` says where each part of the journey begins.

Every animated value is a function of the scroll position and of real time. `anim.py` collects them and bakes them for one `Session`, which is one way of scrolling through the page: `scroll` never stops, `visit` rests at every stop, `idle` and `play` cover the first two stops left alone and played with. A session can also name the moments its visitor does something (`sky_click`, `pond_click`, ...); the pointer and the world's answers exist only in sessions that have them.

| Kind | Driven by | Helpers | Used for |
| --- | --- | --- | --- |
| Scrubbed | scroll position | `ramp`, `switch`, `show`, `scrub`, `light_ramp`, `Shot`, `Actor` moves | the camera, the pumpkin's journey, mood lights, the falling slab |
| Triggered | starts at a scroll position (or at a moment), then plays in real time | `pop`, `unpop`, `after`, `light_window` | type popping in, the lever's sequences, what the homestead's gate and plugins do |
| Live | real time | `live`, `cycle`, an `Actor` at rest (breathing, blinks, glances, a jump when clicked), `life.Fisher` | life at the stops, reactions to the visitor |

A sequence written in seconds can also be played by the scroll: the clock `("scrub", rate)` begins at a scroll position and counts `rate` seconds per screen. The cave uses this: the same waking sequence is scrubbed when the pumpkin arrives (so it is done before the stop, however fast the page is scrolled) and triggered when the lever is pulled.

Scroll maps onto the timeline linearly: no snapping, no sticky zones. A stop is a stretch where the camera's own move slows to a drift; it never stands still while the page scrolls. `build()` reports the scroll budget and any stretch where scrolling would move nothing.

## The journey

| Scroll | Part | Camera | What happens | Alive, and to touch |
| --- | --- | --- | --- | --- |
| 0.00 | **Sky** (stop) | Slow drift toward the pumpkin's cloud | The pumpkin sleeps on its cloud over the valley | Zs rise, clouds and leaves drift, the title floats. Hover the sleeper: it opens an eye and follows. Click it: it jumps. Click a cloud: rain. Type a free name: the title screen's splash, "NOW WITH 100% MORE PUMPKIN!" (and so at the Nether's field). Stay a long while and someone like Steve, with blank shining eyes, stands far below at the treeline looking up; point at him and he is gone, once a visit |
| 0.25 | Dive | Sinks with the falling pumpkin, upright, through a cloud that whites out the frame | It wakes, shuffles off the cloud and falls to the meadow | |
| 1.15 | **Surface** (stop) | Low, behind the landing spot; slow slide | The address and two friends on different editions at one pond. Where the pumpkin will dig, a sign says the game's first rule: never dig straight down | Wind in everything that grows, swell on the pond, both friends fishing, a fish leaping, the name retyping, chat (down to the friend asking if it is their own SMP now). Now and then a friend fishes up a boot, and says so. In one visit in four, after a few seconds' rest, a chicken jockey runs by behind the pond and Steve shouts it. Click the pond, a friend (they say a line of their own, in a bubble), the tree (punched, it drops a log, and "Getting Wood"), the pumpkin, the jockey; point at the sign |
| 1.45 | Slice and dig | Pulls back and drops as the ground in front breaks away, then sinks along the cut face | It stomps beside the sign, the slab falls (in the tall frame the sign stands on it, and goes too), it digs straight down, a jog, and down past two bands of facts | Blocks crack ahead of it. A fossil, a chest, an old sign and a lost sock say what they are |
| 2.75 | Cave arrival | Down with it into the dark cave, then in through the open front | It reaches the glowing snapshot block: the world unfolds, torches fire, friends join | Scrubbed: complete by the stop |
| 3.25 | **Cave** (stop) | Slow slide past the lever | Sleeps when you leave, wakes when you join | Torches flicker, Steve mines, Alex hops. **Pull the lever**: friends leave, torches die, the pumpkin sleeps, the world is saved as one block, the headline says there are no phantoms either, and "Sweet Dreams"; pull again and it is back (too soon, and the action bar says the game's "You may not rest now; there are monsters nearby"). Left alone, the pumpkin goes and does it itself |
| 3.55 | Cart | Chases the cart north, round a banked turn and down a slope | It hops into the cart and rides to the homestead | The action bar says "Press Left Shift to Dismount" (Shift: the pumpkin tries); in the tunnel, "On A Rail" |
| 4.05 | **Homestead** (stop) | Rests on the yard three times, one view for each feature | A cavern where daylight falls down a shaft onto a house in a fenced yard. Three features in turn: friends in, everyone else out; plugins; turn back time | The friends come in and the gate shuts on a stranger: Herobrine. Left alone, he is turned away ("Not Today, Thank You"); click him and he is let in, every light goes out but his eyes, and he leaves the game. The lectern's book installs plugins that change the yard (lanterns, fireflies, fireworks) as dusk falls. At night a creeper blows the house up (the chat says so, as the game prints a death), and scrolling on turns time back, block by block, the backup's year counting down to 2011 |
| 4.75 | Portal | Push into the frame; the swirl fills the screen and the lens wobbles | It jumps into the frame and the portal lights | |
| 5.55 | **Nether** (stop) | Out and forward along the bridge to the second frame | The name field over an unlit portal | Embers rise, the lava breathes, a strider walks. Type a name: it is free, the frame lights; Enter, and the pumpkin leaps through. The dig's lost sock has its other one here, on the deck by the frame: point at it |
| 6.25 | **Home** (stop) | Through the second frame, back over the meadow, and down to the wall of maps, where it drifts to the end of the film | The valley made whole: the visitor's address over a plank wall hung with six maps in item frames, which the page draws live from the service's figures (servers made, servers and players online, CPU, RAM, TPS, each over a graph of the last hours or days: `film/maps.ts`), and the questions people ask in the lectern's book | Each map's red dot blinks while its figures are fresh; the pointer on a map shows them exactly. The book turns its pages; its arrows (or a click on the lectern) turn them by hand |
| 6.80 | Credits (the browser's) | Home's last view, gone dark | The page's own story rolls up the dark, as the game's credits do after the End, moved by the scroll | Its links can be followed: Pumpkin's places, then the page's own (Kurbis on GitHub, and the legal pages). Where it rests, "The End" arrives as an advancement: "Or the beginning?". Scrolled on, the page begins again under the film's cover, in the sky, where the pumpkin opens its eyes for a moment |

Anywhere on the page (the browser's alone):

- Names that do in either name field what they do in the game (`film/eggs.ts`): dinnerbone and grumm stand the name, and the visitor in a friend's place, on their heads; jeb runs the letters through the wool colours; herobrine is "Removed." first; creeper's letters hiss, blow up and come back.
- The days the game marks, by the visitor's calendar (`film/dates.ts`): on Halloween the friends wear pumpkins (lit in the dark) and the title has the game's "OOoooOOOoooo! Spooky!"; from 24 to 26 December every chest is a present.
- The game's keys: F1 puts the interface away, F2 saves a screenshot named as the game names one, F3 is the debug screen and F3+B its hitboxes (which also show what can be clicked). "/" opens the chat: `/help`, `/time set day|night`, `/weather clear|rain`, `/stop`, `/kill` (the death screen) and `/seed` do what they do in the game (`film/chat.ts`, `commands.ts`, `weather.ts`).
- What the visitor makes happen is told as the game tells it: a toast (`film/toasts.ts`), or the action bar for what the game would refuse (`ui.actionBar`). Nametags, speech bubbles and tooltips keep under the bar along the top; a nametag whose head goes up under it is not drawn, as the frame would cut it.
- An address with nothing at it gets the game's "Connection Lost" screen (`web/app/not-found.tsx`).

Rules that keep it one take:

- The camera changes set only while the frame is fully covered (the portal swirl). `film.py` plays two `Shot`s back to back; the jump sits under that cover. The cloud white-out, the dig, the cave and the cart ride are all one real space.
- Nothing is a timed cut or a simulation, so it holds when scrubbed slowly or backwards. Where the camera travels through something that moves (the falling slab), that motion is scrubbed too, so the camera can never catch up with it.
- Only moves, turns, scales, show/hide and light levels are animated, because those carry over to the browser.

Status: previs 2. Every stop is alive and has its main interaction, in two frames: `build(session, "wide")` and `build(session, "tall")` (9:16; `P(wide, tall)` in `film.py` picks each layout, and tall blends are saved as `film_<session>_tall.blend`). The tall frame re-breaks the type and restages the surface on a diagonal; the world itself is the same in both frames. Clips in `renders/previs2/`: `scroll.mp4` (the scroll alone, about 14 s), `visit.mp4` (a whole visit with a rest and the visitor's hand at every stop, about 71 s), `visit_tall.mp4` (the same visit in the tall frame), and per-stop clips `idle.mp4`, `play.mp4`, `cave.mp4`, `cave_demo.mp4`.

The audit also measures contrast (`scripts/audit_run.py` runs it headless at full size). Gold type on the sunset measures about 1.6:1 against the sky. The hero title and the address carry a thin dark rim (`font.keyline`, switched by `HELP` in `film.py`), which separates the letters from the glow but does not change that figure: whether it is enough is still open.

Not yet done here: a copy pass and final-quality passes. The smaller toys (torches, the chest and the diamonds in the cave, the pickaxe pointer) were built in the browser only.

Previewing a clip:

```bash
B=/Applications/Blender.app/Contents/MacOS/Blender
$B -b --factory-startup blend/film_scroll.blend -P scripts/render_range.py -- 1 347 renders/previs2/scroll 0.5 12
python3 scripts/label_frames.py renders/previs2/scroll renders/previs2/scroll_labelled export/session_scroll.json
ffmpeg -y -framerate 24 -i renders/previs2/scroll_labelled/%04d.jpg -c:v libx264 -pix_fmt yuv420p -crf 18 renders/previs2/scroll.mp4
```

## The browser build

The page plays this film in the browser. From here on the page's own code is the source of truth for motion and behaviour: `web/lib/landing/film/` is a port of `film.py` (one module per part of the journey) on a port of `anim.py` (`web/lib/landing/anim/`), drawn by `web/lib/landing/engine/`. Blender supplies the world, the layout and reference samples.

```bash
B=/Applications/Blender.app/Contents/MacOS/Blender
$B -b --factory-startup -P scripts/export_web.py    # about 20 seconds
```

This stages the `visit` session in both frames (nothing is baked) and writes:

| Where | What |
| --- | --- |
| `web/public/landing/valley.grid.gz`, `nether.grid.gz` | The worlds as grids of blocks, one byte a cell: the valley with what lies under it and the homestead's cavern (6.7 million cells in 97 KB), and the Nether (1.1 million in 39 KB). After the valley's cells come the two suns' shadows on its faces (124 KB more). See "What the page builds itself" |
| `web/public/landing/dungeon.tall.patch.gz` | Only if the tall frame's world ever differs from the wide frame's: the box of cells that differ, laid into the valley's grid before it is built. Both frames share one world now, so none is written |
| `web/public/landing/small.pack.gz` | Every small file as one: the pictures from the pack (`tex/`, `hud/`, the two atlases) and the slab's blocks (`slab.grid`; the page meshes its pieces itself) |
| `web/public/landing/models.<frame>.glb.gz` | Everything else small, with names and parents, without pictures: the page gives each material its texture by the material's name |
| `web/public/landing/scene.json`, `layout.<frame>.json` | The sky, palettes, glyphs and the film's constants; and per frame the camera's shots, the type (as text and place: the page builds the letters), lamps, haze boxes and places the sets worked out |
| `web/lib/landing/engine/version.ts` | The bundle's version: a hash over all of its files. The page asks for every file by it (`?v=...`), and `next.config.ts` lets a browser keep what is asked for that way for good |
| `web/public/gui/dirt.png` | Not the film's: a picture of the pack the site's own HTML shows, as a plain file beside the bundle (`GUI` in `export_web.py`). The pack's `block/dirt.png`, which the 404 page (`web/app/not-found.tsx`) tiles and darkens as the game's old menu background |
| `web/tests/unit/landing/fixtures/reference.<frame>.json` | The camera and the pumpkin sampled every 0.02 screens, and what the type says. Numbers and our own copy only, so it is committed: `parity.test.ts` holds the page's timeline to it, `copy.test.ts` the page's text |
| `web/tests/unit/landing/fixtures/world.json` | A digest of every part of the world as built here: its faces and every byte of their light. `world.test.ts` holds the page's building to it |

The files named `.gz` are written through gzip once, at level 9, and sent as they are: a rule in `web/next.config.ts` has the server say so (`Content-Encoding: gzip`), and the browser unpacks them on the way in. The page reads plain bytes and unpacks nothing. Neither the server nor Cloudflare in front of it would pack such bytes by itself (both go by content type, and pack text), and what is packed per request comes out larger: 110 to 118 KB for the valley's cells, against 97. Whatever serves the bundle must send that header: without it the film cannot read its files, and the page is its HTML.

`web/public/landing/` is committed, so the page plays wherever it is deployed. It is made from the pack: see "Rules". Where the bundle is missing or cannot load, the page is its own HTML: the same copy in reading order, a real lever button and a real name field (`web/components/landing/mirror.tsx`). That is also what a browser without WebGL2 gets, and what screen readers and search engines read while the film plays.

### What the page builds itself

The page draws the world as block faces: 28 bytes each (where, which side, which tile, and for each corner daylight, block glow and two suns' shadows), sorted into 32-block cubes to cull by. There are 270,000 of them in the valley's grid alone. They used to be exported ready-made, 1.5 MB packed. Now the page gets the blocks and works the faces out, and the film's data went from 1.85 MB in 58 requests to 0.41 MB in 6.

`web/lib/landing/world/` does there what `faces`, `levels` and `light` in `kurbis/export.py` do here, and comes to the same bytes (the arithmetic is rounded to 32-bit floats at every step, as numpy's is). Change one and the other must follow: `world.test.ts` says when they have parted. Blocks nobody can ever see are written as one kind (`export.uniform`), which halves the packed grid and changes no face.

The suns' shadows are the one part of the light the page does not work out. Marching a ray from every corner toward each sun is two thirds of the work of baking the valley, so `export.shadows` sends the result with the grid: two bytes for each face a sun can reach, in the order the page finds faces, 124 KB packed. The page lays them in (`layShadows`), and refuses shadows that are not for the faces it found.

What is left is about 0.2 s of work on a fast laptop. It is done in a worker (`world/worker.ts`), with a second one flooding the grid with light while the first finds the faces, which brings it to 0.14 s.

None of this waits for the page's code. A few lines of script at the top of the HTML (`lib/landing/boot.ts`) start the worker and ask for every other file the first frame needs, as the browser reads the page's first bytes; the page's code, when it has arrived and run, takes over what is under way (`engine/world.ts`, `engine/early.ts`). For that the worker is a file of its own, `public/world.worker.js`, bundled from `lib/landing/world/` with `pnpm run build:worker` and committed (`world.test.ts` says when it is out of date). The film's code is part of the page's first scripts, so it downloads alongside. Against starting everything from the page's code, the first frame comes at 0.34 s instead of 0.46 s on this machine, 1.06 s instead of 1.19 s at 9 Mbit/s, 4.65 s instead of 4.94 s at 1.6 Mbit/s, and on slow cores 1.5 s instead of 1.8 s. (The table below was measured before this.)

Measured on production builds: from the navigation to the film's first frame, on a first visit and on a repeat visit.

| Device, connection | Faces sent (before) | Everything worked out in the page | Shadows sent (now) |
| --- | --- | --- | --- |
| This machine, no limit | 0.55 s, then 0.48 s | 0.46 s, then 0.42 s | 0.45 s, then 0.42 s |
| This machine, 9 Mbit/s, 60 ms | 2.8 s, then 1.2 s | 1.2 s, then 0.46 s | 1.2 s, then 0.42 s |
| This machine, 1.6 Mbit/s, 150 ms | 12.0 s, then 2.3 s | 4.5 s, then 0.55 s | 5.1 s, then 0.46 s |
| Slow cores, no limit | 1.6 s, then 1.3 s | 2.2 s, then 1.8 s | 1.9 s, then 1.5 s |
| Slow cores, 9 Mbit/s, 60 ms | 3.5 s, then 1.9 s | 2.9 s, then 2.0 s | 2.8 s, then 1.6 s |

("Slow cores" is the whole browser at background priority, which on this Mac means the efficiency cores at low speed. The middle column is commit `78f633df`: it sent 0.31 MB and marched the rays in four workers.)

So sending the shadows costs 0.1 MB, which is half a second on the slowest line, and saves a slow device a third of a second and two workers. `node art/landing/scripts/measure_loading.mjs <origin>...` measures all of this, and the page marks how its own loading went (`performance.getEntriesByType("mark")`, the names beginning `landing:`).

To look at the page beside the previs, with `pnpm dev` running in `web/`:

```bash
node art/landing/scripts/web_frames.mjs http://localhost:3000 /tmp/frames wide    # and again with: tall
python3 art/landing/scripts/compare_web.py <previs frames> /tmp/frames sheet.jpg wide
```

(The previs frames are stills pulled from `renders/previs2/visit.mp4` and `visit_tall.mp4`, named `<frame>_<scroll position>.jpg`.)

Where the browser differs from the previs on purpose: light is baked per block corner plus a short list of live lamps, haze is worked out per pixel from the same boxes, there is no motion blur, and a triggered piece is taken back when the scroll returns before its start. A visitor who asked for less motion gets a film that stands at the stops and steps between them under a short dip to dark.

## Scale and terrain rules

- A block is 1 m. Players are exactly two blocks tall, the pumpkin exactly one. Every scene with a player keeps a single-block object (barrel, crafting table, ore) at the same depth so the eye can count.
- Ground is never terraced or straight: it rolls in single-block steps along wandering contours. Only the spot the cast stands on is level.
- The tunnel is plugged block by block until the pumpkin digs it, and the cut face is hidden behind a slab of ground until the slice.
- The pumpkin is one block in every scene, so in the hero its cloud sits closer to the camera than in the old still (same picture, half the distance).

## Type rules

- Copy is floating extruded type: bright face, darker sides. Where the backdrop is busy, a `scenery.scrim` card (soft, dark, see-through) sits just behind the letters.
- Type on a wall is flush with it, on a rectangle kept free of relief.
- Signs use dark `ink` letters on a board that glows a little, so they read in a dim room.

## Rules

- **Textures and skins are a stand-in.** The POC reads the "Trailers Vision" pack from `ref/pack`, including its Steve and Alex skins. The pack itself stays out of the repository. The bundle exported from it (`web/public/landing/`) is committed and deployed as a proof of concept, by the owner's decision of 2026-10-12; our own textures and characters replace it once the look is approved.
- **Copy is in-world.** Text is voxel type in the scene, not DOM. The page still needs a hidden text mirror for screen readers and search, and a real `<input>` behind the name claim.
- **Tone mapping is Standard.** AgX washes the palette out. The three.js side should use no tone mapping or Neutral to match.
- **Export contract.** Named nodes, rigid parts under pivots, one atlas material per group. What the page must know about a thing beyond its geometry is said with `core.note` and exported with it.

## Blender 5.2 notes

- The compositor is a node group on `scene.compositing_node_group`; Glare's type is a menu socket.
- World volumes block the sun in EEVEE, so haze is a box object with a scatter material.
- Do not call `bpy.ops.wm.read_factory_settings` in a live MCP session; it can unload the add-on.
- Curves are written straight to F-curves with `action.fcurve_ensure_for_datablock`, which also makes the action's slot.
- A geometry-nodes modifier's inputs are set as `getattr(modifier.properties.inputs, identifier).value`; the old `modifier[identifier]` is gone.
