# Landing v2 art workspace

Source art for the scroll-driven landing page (`web/app/(marketing)/v2/`). The look follows the Minecraft trailer references: hard-edged voxels, flat trailer-style textures, golden-hour light, haze, bloom, and chunky extruded type that lives in the world.

Scenes are finished one at a time. A scene is not started until the one before it is signed off.

| Path | Purpose |
| --- | --- |
| `scripts/gate0.py` | Builds the scene and renders stills. Run in Blender (Blender MCP, or `blender -b -P scripts/gate0.py`). Rebuilds from scratch every run. |
| `scripts/kurbis/` | The toolkit (see below). |
| `ref/` | Reference material and the third-party texture pack. Git-ignored. |
| `blend/`, `export/`, `renders/` | Generated. Git-ignored while the POC uses pack textures. |

## Toolkit (`scripts/kurbis`)

- `blocks.py`: texture atlas (colour + emission), block registry, the shared voxel material.
- `voxel.py`: numpy mesher. A 3D id grid becomes culled, atlas-mapped quads.
- `world.py`: the world generator. Autumn valley on top, cut open at `y = 0` to show the ground, shaft and cave.
- `font.py`: "Kurbis Block", our own voxel display face. All copy is in-world type built from it.
- `mascot.py`: the pumpkin and its faces (our own pixel art).
- `boxmodel.py`: box-UV models: players (rigid parts with elbow and knee pivots), held items, torch, lantern, chest, plant sprites.
- `scenery.py`: clouds, drifting leaves, the snapshot diorama.
- `look.py`: sky, lights, haze, camera, compositor (bloom, vignette), render settings.

## Status

All seven scenes exist as finished stills, each in landscape (1920×1080) and portrait (1080×1920), in `renders/`. `renders/contact_sheet.png` shows them together. Nothing is animated yet and nothing runs in the browser yet.

| Scene | Still | What it says |
| --- | --- | --- |
| 1. Hero | `hero` | Your world is waiting below. Free Minecraft servers for you and your friends. |
| 2. Landing | `landing` | steve.kurbis.host: you choose the name. Java and Bedrock friends. |
| 3. Dig | `dig` | Cost: nothing. Hidden fees: dirt. The explorer's journal. |
| 4. Cave awake | `cave_awake` | It only glows when someone's home. |
| 5. Cave asleep | `cave_asleep` | Nobody home? Every block is saved, as one small snapshot. |
| 6. Treasure room | `vault` | Console, players, backups, settings, /ai. |
| 7. Portal | `portal` | Pick a name. Step through to your world. |

To render one: run `gate0.py` in Blender (it builds everything), then `render("<scene>")` or `render("<scene>_portrait")`.

## Scale and terrain rules

- A block is 1 m. Players are exactly two blocks tall, the pumpkin exactly one. Every scene with a player keeps a single-block object (barrel, crafting table, ore) at the same depth so the eye can count.
- Ground is never terraced or straight: it rolls in single-block steps along wandering contours. Only the spot the cast stands on is level.
- Shafts the pumpkin has not dug yet are plugged per scene (`undug(...)`), so earlier scenes show the ground whole.

## Rules

- **Textures and skins are a stand-in.** The POC reads the "Trailers Vision" pack from `ref/pack`, including its Steve and Alex skins. Nothing derived from it may be committed or shipped; our own textures and characters replace it once the look is approved.
- **Copy is in-world.** Text is voxel type in the scene, not DOM. The page still needs a hidden text mirror for screen readers and search, and a real `<input>` behind the name claim.
- **Tone mapping is Standard.** AgX washes the palette out. The three.js side should use no tone mapping or Neutral to match.
- **glTF contract.** Named nodes, rigid parts under pivots, one atlas material per group.

## Blender 5.2 notes

- The compositor is a node group on `scene.compositing_node_group`; Glare's type is a menu socket.
- World volumes block the sun in EEVEE, so haze is a box object with a scatter material.
- Do not call `bpy.ops.wm.read_factory_settings` in a live MCP session; it can unload the add-on.
