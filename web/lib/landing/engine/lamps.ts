/**
 * The lamps of the set. The art exports every one of them (about sixty); the film sets
 * how bright each is, and each frame the brightest near the camera are handed to the
 * shaders.
 *
 * Blender counts a point lamp of P watts as P / 4pi^2 of light on a facing surface a
 * block away (before the surface's own colour), an area lamp as P / 4pi straight ahead,
 * and a sun of strength S as S / pi.
 */

import { Color, Vector3, type Object3D } from "three";
import type { LightData } from "./bundle";
import { KEYS, LAMPS, type Globals } from "./shading";

const POINT = 1 / (4 * Math.PI * Math.PI);
const AREA = 1 / (4 * Math.PI);

export interface Lamp {
  readonly name: string;
  readonly kind: LightData["kind"];
  readonly color: Color;
  /** Its strength as the art built it. */
  readonly base: number;
  /** Its strength now (the film's to set). */
  energy: number;
  /** Where it is, under `parent` if it has one. */
  readonly at: Vector3;
  readonly parent: Object3D | null;
  /** Suns: the way to them. Area lamps: the way they point. */
  readonly toward: Vector3;
  readonly size: number;
}

export class Lamps {
  readonly byName = new Map<string, Lamp>();
  private readonly list: Lamp[] = [];
  private readonly where = new Vector3();
  private readonly scored: { lamp: Lamp; score: number; x: number; y: number; z: number }[] = [];

  constructor(lights: readonly LightData[], nodes: ReadonlyMap<string, Object3D>) {
    for (const light of lights) {
      const toward = new Vector3(0, 0, -1);
      if (light.toward) toward.set(light.toward[0], light.toward[1], light.toward[2]);
      if (light.kind === "sun") toward.negate();
      const lamp: Lamp = {
        name: light.name,
        kind: light.kind,
        color: new Color(light.color[0], light.color[1], light.color[2]),
        base: light.energy,
        energy: 0,
        at: new Vector3(light.at[0], light.at[1], light.at[2]),
        parent: light.parent ? (nodes.get(light.parent) ?? null) : null,
        toward,
        size: light.size ?? light.radius ?? 0,
      };
      this.list.push(lamp);
      this.byName.set(lamp.name, lamp);
    }
  }

  get(name: string): Lamp {
    const lamp = this.byName.get(name);
    if (!lamp) throw new Error(`landing: no lamp called ${name}`);
    return lamp;
  }

  /** A lamp's strength as the art built it (0 for one that is not there). */
  base(name: string): number {
    return this.byName.get(name)?.base ?? 0;
  }

  /** Every lamp whose name begins so. */
  named(prefix: string): Lamp[] {
    return this.list.filter((lamp) => lamp.name.startsWith(prefix));
  }

  /** Set a lamp's strength; unknown names are ignored (a lamp may exist in one frame shape only). */
  set(name: string, energy: number) {
    const lamp = this.byName.get(name);
    if (lamp) lamp.energy = energy;
  }

  /** Hand the shaders the sun, the key suns and the lamps that matter most from `eye`. */
  gather(globals: Globals, eye: Vector3) {
    let keys = 0;
    this.scored.length = 0;
    for (const key of globals.uKeyColor.value) key.set(0, 0, 0);
    for (const lamp of this.list) {
      if (lamp.kind === "sun") {
        if (lamp.name === "Sun") {
          globals.uSunDir.value.copy(lamp.toward);
          globals.uSunColor.value.copy(lamp.color).multiplyScalar(lamp.energy / Math.PI);
          globals.uSunHaze.value.copy(lamp.color).multiplyScalar(lamp.energy);
        } else if (lamp.energy > 0 && keys < KEYS) {
          globals.uKeyDir.value[keys].copy(lamp.toward);
          globals.uKeyColor.value[keys].set(lamp.color.r, lamp.color.g, lamp.color.b).multiplyScalar(lamp.energy / Math.PI);
          keys += 1;
        }
        continue;
      }
      if (lamp.energy <= 0) continue;
      const where = this.where.copy(lamp.at);
      if (lamp.parent) lamp.parent.localToWorld(where);
      const reach = lamp.energy * (lamp.kind === "area" ? AREA : POINT);
      this.scored.push({ lamp, score: reach / (where.distanceToSquared(eye) + 16), x: where.x, y: where.y, z: where.z });
    }
    this.scored.sort((a, b) => b.score - a.score);
    for (let i = 0; i < LAMPS; i += 1) {
      const entry = this.scored[i];
      if (!entry) {
        globals.uLampColor.value[i].set(0, 0, 0, 0);
        continue;
      }
      const { lamp } = entry;
      const area = lamp.kind === "area";
      const reach = lamp.energy * (area ? AREA : POINT);
      const least = area ? Math.max((lamp.size * lamp.size) / 4, 0.25) : 0.09;
      globals.uLampAt.value[i].set(entry.x, entry.y, entry.z, least);
      globals.uLampColor.value[i].set(lamp.color.r * reach, lamp.color.g * reach, lamp.color.b * reach, area ? 1 : 0);
      globals.uLampAim.value[i].copy(lamp.toward);
    }
  }
}
