/**
 * The set: everything the art pipeline exported, standing where it starts, for one
 * shape of frame. The film (../film) moves it.
 */

import { Color, Group, Mesh, Object3D, PerspectiveCamera, Scene, Vector3, type Material, type ShaderMaterial, type Texture } from "three";
import { Shot } from "../anim/shot";
import { loadJson, loadModels, loadPicture, loadRecords, loadTiles, type Aspect, type FogData, type LayoutData, type NodeData, type SceneData } from "./bundle";
import { flakesMesh } from "./flakes";
import { Lamps } from "./lamps";
import { modelMaterial, readsTint, type Moves, type Pictures } from "./models";
import { linearColor, makeGlobals, type Globals } from "./shading";
import { makeSky } from "./sky";
import { rimGeometry, scrimMesh, textMask, typeGeometry, typeMaterial } from "./type";
import { voxelMaterial, voxelMeshes, type VoxelLook } from "./voxels";

Object3D.DEFAULT_UP.set(0, 0, 1); // the art's axes: Z is up

export interface Stage {
  aspect: Aspect;
  tall: boolean;
  data: SceneData;
  layout: LayoutData;
  scene: Scene;
  camera: PerspectiveCamera;
  globals: Globals;
  shots: Shot[];
  /** Everything with a name: the glb's nodes and what is built here. */
  nodes: Map<string, Object3D>;
  /** Which slot of the shader's haze list each haze has, and each haze as the art built it. */
  hazes: Map<string, number>;
  fog: Map<string, FogData>;
  lamps: Lamps;
  /** How bright the sky is: the film turns it down underground. */
  skyStrength: { value: number };
  pictures: Pictures;
  /**
   * Type set at run time (a name being typed, what the page answers): lines of the block
   * face as one mesh, its origin at the bottom centre of its front. The caller places it
   * and disposes of its geometry when done.
   */
  makeType(lines: readonly string[], options: { palette?: string; cell: number; depth: number; material?: string; day?: number; leading?: number }): Mesh;
  /** Resolves when the parts under the valley and beyond the portal are in too. */
  rest: Promise<void>;
  dispose(): void;
}

/**
 * Daylight from the sky on a face that looks up, and on one that looks sideways, at the
 * sky's full strength. Measured on the previs (the sky alone lighting a pale cloud):
 * Blender spreads the bright horizon over every direction, so the two are close.
 */
export const SKY_UP = new Color(0.25, 0.08, 0.14);
export const SKY_SIDE = new Color(0.19, 0.08, 0.1);
export const SKY_DOWN = new Color(0.28, 0.2, 0.15);

const NO_GLOW = [0, 0, 0];

function place(object: Object3D, node: NodeData) {
  object.name = node.name;
  object.position.set(node.at[0], node.at[1], node.at[2]);
  object.rotation.set(node.turn[0], node.turn[1], node.turn[2], "ZYX"); // Blender's XYZ order
  object.scale.set(node.size[0], node.size[1], node.size[2]);
}

/**
 * Put a haze in one of the shader's slots. `sunlit` is the share of the sun it scatters;
 * `glow` is the light of lamps that stand in it, as it would look through any depth of it.
 */
export function setHaze(globals: Globals, slot: number, fog: FogData, density: number, sunlit: number, glow: readonly number[] = NO_GLOW) {
  globals.uHazeGlow.value[slot].set(glow[0], glow[1], glow[2]);
  globals.uHazeLo.value[slot].set(fog.lo[0], fog.lo[1], fog.lo[2]);
  globals.uHazeHi.value[slot].set(fog.hi[0], fog.hi[1], fog.hi[2]);
  globals.uHazeColor.value[slot].set(fog.color[0], fog.color[1], fog.color[2], density);
  globals.uHazeShape.value[slot].set(fog.anisotropy, fog.fade ? fog.fade[0] : 0, fog.fade ? fog.fade[1] : 0, sunlit);
}

/** Before a thing that shares its material is drawn, the material is told how much of this one is there. */
function ownTint(this: Object3D, _renderer: unknown, _scene: unknown, _camera: unknown, _geometry: unknown, material: Material) {
  const shader = material as ShaderMaterial;
  shader.uniforms.uTint.value = this.userData.tint ?? 1;
  shader.uniformsNeedUpdate = true;
}

export async function loadStage(aspect: Aspect): Promise<Stage> {
  const [data, layout] = await Promise.all([loadJson<SceneData>("scene.json"), loadJson<LayoutData>(`layout.${aspect}.json`)]);
  const globals = makeGlobals();
  const scene = new Scene();
  const nodes = new Map<string, Object3D>();
  const disposables: { dispose(): void }[] = [];

  // Haze: the film switches each on where it belongs (film/grade.ts).
  const hazes = new Map<string, number>();
  const fog = new Map<string, FogData>(layout.fog.map((item) => [item.name, item]));

  const [tiles, tilesEmit, atlas, atlasEmit] = await Promise.all([
    loadTiles("atlas_color.png", data.atlas),
    loadTiles("atlas_emit.png", data.atlas),
    loadPicture("atlas_color.png"),
    loadPicture("atlas_emit.png"),
  ]);
  globals.uTiles.value = tiles;
  globals.uTilesEmit.value = tilesEmit;
  disposables.push(tiles, tilesEmit, atlas, atlasEmit);

  const sky = makeSky(globals, data.sky);
  scene.add(sky);
  const skyStrength = (sky.material as ShaderMaterial).uniforms.uStrength as { value: number };

  // The big voxel meshes.
  const looks: Record<string, VoxelLook> = {
    terrain: { emit: 4 },
    leaves: { emit: 4, cut: 0.5, wind: 0.075 },
    water: { emit: 0, alpha: 0.74, flat: [0.03, 0.22, 0.8], waves: [[1.0, 0.045], [-25, 0.02]], shimmer: true },
    nether: { emit: 1.5, crawl: true },
  };
  const voxelMaterials = new Map<string, ShaderMaterial>();
  const addRecords = async (file: string, look: string) => {
    const records = await loadRecords(file);
    let material = voxelMaterials.get(look);
    if (!material) {
      material = voxelMaterial(globals, looks[look]);
      voxelMaterials.set(look, material);
      disposables.push(material);
    }
    const group = voxelMeshes(file, records, material);
    if (look === "water") group.renderOrder = 10;
    scene.add(group);
    nodes.set(file, group);
  };

  // Pictures the small models need, then the models themselves.
  const named = new Map<string, Texture>();
  const pictures: Pictures = { atlas, atlasEmit, named };
  const loadNamed = Promise.all(
    Object.entries(data.textures).map(async ([key, file]) => {
      const picture = await loadPicture(`tex/${file}`);
      named.set(key, picture);
      disposables.push(picture);
    }),
  );
  const [models] = await Promise.all([loadModels(aspect), loadNamed, addRecords("valley.terrain", "terrain"), addRecords("valley.leaves", "leaves"), addRecords("valley.water", "water")]);

  const materials = new Map<string, Material>();
  models.traverse((object) => {
    nodes.set(object.name, object);
    const mesh = object as Mesh;
    if (!mesh.isMesh) return;
    const name = layout.materials[object.name] ?? "";
    const note = data.materials[name];
    let material = materials.get(name);
    if (!material && note) {
      const said = typeof object.userData.kurbis === "string" ? (JSON.parse(object.userData.kurbis) as Moves) : {};
      material = modelMaterial(globals, note, pictures, said);
      material.name = name;
      materials.set(name, material);
      disposables.push(material);
    }
    if (material) mesh.material = material;
    else mesh.visible = false;
    if (readsTint(note)) {
      mesh.userData.tint = layout.colors[object.name]?.[note?.kind === "crack" ? 0 : 3] ?? (note?.kind === "crack" ? 0 : 1);
      mesh.onBeforeRender = ownTint;
      if (note?.kind === "fading" && mesh.userData.tint <= 0) mesh.visible = false; // nothing of it yet: the film brings it in
    }
  });
  scene.add(models);

  // What is built here rather than loaded: type, its rims and scrims, and the flakes.
  const face = data.glyphs.block;
  const typeMaterials = new Map<string, ShaderMaterial>();
  const typeLook = (name: string, day: number) => {
    const key = `${name}:${day}`;
    let material = typeMaterials.get(key);
    if (!material) {
      material = typeMaterial(globals, data.materials[name]?.glow ?? 0.55, day);
      typeMaterials.set(key, material);
      disposables.push(material);
    }
    return material;
  };
  const rim = linearColor(data.materials.Rim?.rgb ?? [0.13, 0.04, 0.07]).toArray();
  const built: [Object3D, NodeData][] = [];
  for (const node of layout.nodes) {
    const note = node.note;
    if (!note) continue;
    let object: Object3D | null = null;
    if (note.kind === "type" && note.lines && note.cell && note.depth) {
      const palette = data.palettes[note.palette ?? "gold"];
      object = new Mesh(typeGeometry(textMask(face, note.lines, note.layout), palette, note.cell, note.depth), typeLook(note.material ?? "Type", 1));
    } else if (note.kind === "rim" && note.lines && note.cell) {
      object = new Mesh(rimGeometry(textMask(face, note.lines, note.layout), note.cell, note.width ?? 0.2, rim), typeLook("Rim", 1));
    } else if (note.kind === "scrim" && note.width && note.height) {
      object = scrimMesh(note.width, note.height, note.feather ?? 0.9, note.strength ?? 0.6);
      object.renderOrder = 5;
    } else if (note.kind === "flakes") {
      object = flakesMesh(globals, note, data.materials[node.material ?? ""], node.color?.[3] ?? 1);
      object.renderOrder = 6;
      disposables.push((object as Mesh).material as ShaderMaterial);
    }
    if (!object) continue;
    place(object, node);
    nodes.set(node.name, object);
    built.push([object, node]);
    const mesh = object as Mesh;
    if (mesh.geometry) disposables.push(mesh.geometry);
  }
  for (const [object, node] of built) (node.parent ? (nodes.get(node.parent) ?? scene) : scene).add(object);
  // Type below ground has no daylight on it.
  scene.updateMatrixWorld(true);
  const where = new Vector3();
  for (const [object, node] of built) {
    const mesh = object as Mesh;
    if (node.note?.kind === "scrim" || node.note?.kind === "flakes" || mesh.getWorldPosition(where).z > data.valley_floor) continue;
    mesh.material = typeLook(node.note?.kind === "rim" ? "Rim" : (node.note?.material ?? "Type"), 0);
  }

  const camera = new PerspectiveCamera(40, 16 / 9, 0.08, 1400);
  const shots = layout.shots.map((shot) => new Shot(shot));
  const lamps = new Lamps(layout.lights, nodes);

  // The rest of the world follows without holding up the hero.
  const rest = Promise.all([
    addRecords("under.terrain", "terrain"),
    addRecords("under.water", "water"),
    addRecords(`dungeon.${aspect}`, "terrain"),
    addRecords("nether.terrain", "nether"),
  ]).then(() => undefined);

  return {
    aspect,
    tall: aspect === "tall",
    data,
    layout,
    scene,
    camera,
    globals,
    shots,
    nodes,
    hazes,
    fog,
    lamps,
    skyStrength,
    pictures,
    makeType(lines, { palette = "cream", cell, depth, material = "Type", day = 0, leading = 3 }) {
      return new Mesh(typeGeometry(textMask(face, lines, { leading }), data.palettes[palette], cell, depth), typeLook(material, day));
    },
    rest,
    dispose() {
      for (const item of disposables) item.dispose();
      scene.traverse((object) => (object as Mesh).geometry?.dispose());
    },
  };
}

export type { Group };
