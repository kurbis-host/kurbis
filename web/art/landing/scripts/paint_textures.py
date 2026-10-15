"""Paint Kurbis's own textures (see paint/) into web/art/landing/textures.

    python3 scripts/paint_textures.py [sheet.png]

Writes every texture the landing page draws, under the path the toolkit reads it by
(core.TEX), and with a path given, a contact sheet of them all at eight times size.
Plain Python (numpy and Pillow): no Blender needed. Run export_web.py afterwards to
carry the pictures into the page's bundle.
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent))

from paint import PAINTED, paint  # noqa: E402
from paint import entities, nether, plants, things, terrain  # noqa: E402,F401  (they register their painters)

OUT = Path(__file__).resolve().parents[1] / "textures"

# What the landing page draws, and nothing else: the blocks seen in its worlds, the
# pictures its models and interface name, and the cracks of a block being dug. (The last
# two lines are for the pitch's additions: toast icons, the present, the chicken jockey,
# Halloween pumpkins, and the skins hud.py draws the bed and shield icons from.)
USED = """
block/grass_block_top block/grass_block_side block/dirt block/sand block/moss_block
block/stone block/andesite block/gravel block/tuff block/cobblestone block/mossy_cobblestone
block/deepslate block/deepslate_top block/bedrock
block/coal_ore block/iron_ore block/copper_ore block/gold_ore block/redstone_ore block/lapis_ore block/diamond_ore
block/deepslate_diamond_ore block/deepslate_gold_ore block/deepslate_redstone_ore
block/oak_log block/oak_log_top block/spruce_log block/spruce_log_top block/oak_planks
block/oak_leaves block/flowering_azalea_leaves block/cherry_leaves
block/obsidian block/crying_obsidian block/blackstone block/blackstone_top block/polished_blackstone_bricks
block/gilded_blackstone block/nether_bricks block/netherrack block/magma block/basalt_side block/basalt_top
block/glowstone block/lava_still block/nether_portal
block/bone_block_side block/bone_block_top block/crafting_table_side block/crafting_table_front block/crafting_table_top
block/furnace_side block/furnace_front_on block/furnace_top block/bookshelf block/barrel_side block/barrel_top block/barrel_bottom
block/pumpkin_side block/pumpkin_top block/jack_o_lantern block/redstone_lamp block/redstone_lamp_on
block/short_grass block/sugar_cane block/lily_pad block/pink_petals block/cornflower block/dandelion block/oxeye_daisy block/poppy
block/cave_vines_lit block/cave_vines_lit_e block/torch block/torch_e block/lantern block/lantern_e block/lever
block/rail block/rail_corner block/spawner
block/destroy_stage_0 block/destroy_stage_1 block/destroy_stage_2 block/destroy_stage_3 block/destroy_stage_4
block/destroy_stage_5 block/destroy_stage_6 block/destroy_stage_7 block/destroy_stage_8 block/destroy_stage_9
item/diamond item/iron_pickaxe item/fishing_rod_cast item/ender_pearl
entity/chest/normal entity/minecart/minecart entity/signs/oak entity/strider/strider entity/fish/salmon
entity/player/wide/steve entity/player/slim/alex
gui/sprites/toast/advancement
block/end_stone block/carved_pumpkin item/leather_boots item/leather_boots_overlay item/minecart
entity/chest/christmas entity/chicken/chicken_temperate entity/zombie/zombie_baby entity/bed/red entity/shield/shield_base_nopattern
"""


def sheet(pictures, path, scale=8, across=10):
    cell = 16 * scale
    rows = -(-len(pictures) // across)
    out = Image.new("RGBA", (across * (cell + 8), rows * (cell + 22)), (58, 58, 66, 255))
    draw = ImageDraw.Draw(out)
    for i, (name, canvas) in enumerate(pictures):
        r, c = divmod(i, across)
        x, y = c * (cell + 8), r * (cell + 22)
        data = np.round(np.clip(canvas.px, 0, 1) * 255).astype(np.uint8)
        if data.shape[0] > data.shape[1] and data.shape[1] == 16:
            data = data[:16]  # a strip of frames: the first
        im = Image.fromarray(data, "RGBA")
        fit = min(cell / im.width, cell / im.height)
        im = im.resize((max(1, int(im.width * fit)), max(1, int(im.height * fit))), Image.NEAREST)
        out.alpha_composite(im, (x, y))
        draw.text((x, y + cell + 4), name.split("/")[-1][:22], fill=(255, 255, 255, 255))
    out.save(path)


def main():
    used = [f"{name}.png" for name in USED.split()]
    missing = [path for path in used if path not in PAINTED]
    if missing:
        raise SystemExit(f"no painter for: {', '.join(missing)}")
    pictures = []
    for path in used:
        canvas = paint(path)
        canvas.save(OUT / path)
        pictures.append((path, canvas))
    stale = sorted(str(f.relative_to(OUT)) for f in OUT.rglob("*.png") if str(f.relative_to(OUT)) not in used)
    for name in stale:
        (OUT / name).unlink()
    if len(sys.argv) > 1:
        sheet(pictures, sys.argv[1])
    print(f"painted {len(used)} textures into {OUT}" + (f", removed {len(stale)} no longer used" if stale else ""))


if __name__ == "__main__":
    main()
