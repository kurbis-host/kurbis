"use client";

import { useFrame } from "@react-three/fiber";
import { useMemo, useRef } from "react";
import * as THREE from "three";
import type { BuriedId, OreId } from "../../../lib/landing/copy";
import { getLanding, landing, scroll, TORCH_COUNT, useLanding } from "../../../lib/landing/store";
import { SPOTS } from "../../../lib/landing/terrain";
import { ambience, Box, damp, hover, hoverHandlers, Sprite, unitBox, type Cell, type Vec3 } from "./kit";

/** Props sit just proud of the front face of the slab. */
const FRONT = 0.52;

const BURIED: { id: BuriedId; at: [number, number]; cells: Cell[] }[] = [
  {
    id: "fossil",
    at: [5.5, -5],
    cells: [
      [-2, -2, 5, 1, "#ece4c6"],
      [-2, -1, 1, 3, "#ece4c6"],
      [2, -1, 1, 3, "#ece4c6"],
      [-2, 2, 5, 1, "#ece4c6"],
      [0, 0, 1, 1, "#d6cba6"],
    ],
  },
  {
    id: "bone",
    at: [11.5, -8.5],
    cells: [
      [-3, 0, 6, 1, "#f5f1e6"],
      [-4, 1, 2, 1, "#f5f1e6"],
      [-4, -1, 2, 1, "#f5f1e6"],
      [2, 1, 2, 1, "#f5f1e6"],
      [2, -1, 2, 1, "#f5f1e6"],
    ],
  },
  {
    id: "sign",
    at: [14.5, -6],
    cells: [
      [0, -3, 1, 3, "#6b4a2c"],
      [-3, 0, 7, 1, "#c99b60"],
      [-3, 1, 7, 1, "#8c643a"],
      [-3, 2, 7, 1, "#c99b60"],
    ],
  },
  {
    id: "sock",
    at: [8, -11.5],
    cells: [
      [-1, 3, 2, 1, "#cf4a32"],
      [-1, 0, 2, 3, "#f2eee6"],
      [-1, -2, 4, 2, "#f2eee6"],
      [3, -2, 1, 2, "#cf4a32"],
    ],
  },
  {
    id: "coal",
    at: [13.5, -12.5],
    cells: [
      [-1, -1, 3, 1, "#2a2a30"],
      [-2, 0, 1, 1, "#2a2a30"],
      [-1, 0, 2, 1, "#2a2a30"],
      [1, 0, 1, 1, "#55555f"],
      [0, 1, 2, 1, "#2a2a30"],
    ],
  },
];

function BuriedThing({ id, at, cells }: (typeof BURIED)[number]) {
  const ref = useRef<THREE.Group>(null);
  const found = useLanding((state) => state.found.includes(id));
  const latest = useLanding((state) => state.lastFound === id);

  useFrame((state, delta) => {
    const group = ref.current;
    if (!group) return;
    const k = damp(delta, 9, scroll.reducedMotion);
    const scale = latest ? 1.5 : found ? 1.15 : 1;
    group.scale.x = group.scale.y += (scale - group.scale.y) * k;
    group.position.z += ((found ? FRONT + 0.35 : FRONT) - group.position.z) * k;
    group.rotation.z = latest && !scroll.reducedMotion ? Math.sin(state.clock.elapsedTime * 5) * 0.12 : 0;
  });

  return (
    <group
      ref={ref}
      position={[at[0], at[1], FRONT]}
      {...hoverHandlers}
      onClick={(event) => {
        event.stopPropagation();
        landing.dig(id);
      }}
    >
      <Sprite cells={cells} unit={0.19} depth={0.3} />
    </group>
  );
}

const TORCHES: [number, number][] = [
  [4.5, -22],
  [7, -20],
  [10, -19.2],
  [13, -20],
  [15.5, -22],
];
/** Each light is fed by the torches around it, so five flames need only three lights. */
const TORCH_LIGHTS = [
  { at: [5.5, -21, 0.4] as Vec3, torches: [0, 1] },
  { at: [10, -20, 0.4] as Vec3, torches: [2] },
  { at: [14.5, -21, 0.4] as Vec3, torches: [3, 4] },
];

function darknessTexture(): THREE.Texture {
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 64;
  const context = canvas.getContext("2d");
  if (context) {
    // Opaque greyscale: an alpha map reads the green channel, not the canvas alpha.
    const gradient = context.createRadialGradient(32, 32, 14, 32, 32, 32);
    gradient.addColorStop(0, "#ffffff");
    gradient.addColorStop(1, "#000000");
    context.fillStyle = gradient;
    context.fillRect(0, 0, 64, 64);
  }
  return new THREE.CanvasTexture(canvas);
}

function Cave() {
  const { cave } = SPOTS;
  const levels = useRef<number[]>(Array.from({ length: TORCH_COUNT }, () => 0));
  const flames = useRef<(THREE.Mesh | null)[]>([]);
  const lights = useRef<(THREE.PointLight | null)[]>([]);
  const dark = useRef<THREE.MeshBasicMaterial>(null);
  const flash = useRef<THREE.MeshBasicMaterial>(null);
  const flashLevel = useRef(0);
  const lastCave = useRef(getLanding().cave);
  const player = useRef<THREE.Group>(null);
  const mask = useMemo(darknessTexture, []);
  const unlit = useMemo(() => new THREE.Color("#3b3a38"), []);
  const fire = useMemo(() => new THREE.Color("#ffcc66"), []);

  useFrame((state, delta) => {
    const current = getLanding();
    const k = damp(delta, 7, scroll.reducedMotion);
    let total = 0;
    for (let index = 0; index < TORCH_COUNT; index += 1) {
      levels.current[index] += ((current.torches[index] ? 1 : 0) - levels.current[index]) * k;
      total += levels.current[index];
      const flame = flames.current[index];
      if (flame) {
        (flame.material as THREE.MeshBasicMaterial).color.lerpColors(unlit, fire, levels.current[index]);
        const flicker = scroll.reducedMotion ? 1 : 1 + Math.sin(state.clock.elapsedTime * 11 + index * 2) * 0.08 * levels.current[index];
        flame.scale.setScalar(0.24 * flicker);
      }
    }
    TORCH_LIGHTS.forEach((group, index) => {
      const light = lights.current[index];
      if (!light) return;
      const level = group.torches.reduce((sum, torch) => sum + levels.current[torch], 0) / group.torches.length;
      light.intensity = level * 42;
    });

    if (current.cave === "snapshot" && lastCave.current !== "snapshot") flashLevel.current = 1;
    lastCave.current = current.cave;
    flashLevel.current = Math.max(0, flashLevel.current - delta * 1.8);
    if (flash.current) flash.current.opacity = flashLevel.current * 0.75;
    if (dark.current) dark.current.opacity = (1 - total / TORCH_COUNT) * 0.82;

    if (player.current) {
      const here = current.cave === "awake" ? 1 : 0;
      player.current.scale.setScalar(player.current.scale.x + (here - player.current.scale.x) * k);
    }
  });

  return (
    <>
      {TORCHES.map(([x, y], index) => (
        <group key={index} position={[x, y, -1.3]}>
          <Box at={[0, -0.35, 0]} size={[0.14, 0.55, 0.14]} color="#6b4a2c" />
          <mesh
            ref={(mesh) => {
              flames.current[index] = mesh;
            }}
            geometry={unitBox}
            scale={0.24}
          >
            <meshBasicMaterial color="#3b3a38" />
          </mesh>
        </group>
      ))}
      {TORCH_LIGHTS.map((group, index) => (
        <pointLight
          key={index}
          ref={(light) => {
            lights.current[index] = light;
          }}
          position={group.at}
          color="#ffb060"
          intensity={0}
          distance={11}
          decay={1.5}
        />
      ))}
      <group ref={player} position={[12.5, cave.floor + 0.5, -0.8]} scale={0}>
        <Box at={[0, 0.3, 0]} size={[0.46, 0.6, 0.3]} color="#3b4a6b" />
        <Box at={[0, 0.92, 0]} size={[0.56, 0.64, 0.32]} color="#3f713e" />
        <Box at={[0, 1.47, 0]} size={[0.46, 0.46, 0.4]} color="#e6b98f" />
      </group>
      <mesh position={[cave.x, cave.y, FRONT + 0.1]} scale={[cave.rx * 2 + 7, cave.ry * 2 + 5, 1]}>
        <planeGeometry />
        <meshBasicMaterial ref={dark} color="#07070a" alphaMap={mask} transparent opacity={0.82} depthWrite={false} />
      </mesh>
      <mesh position={[cave.x, cave.y, FRONT + 0.15]} scale={[cave.rx * 2 + 7, cave.ry * 2 + 5, 1]}>
        <planeGeometry />
        <meshBasicMaterial ref={flash} color="#ffffff" alphaMap={mask} transparent opacity={0} depthWrite={false} />
      </mesh>
    </>
  );
}

const ORE_SHAPE: [number, number, number, number, 0 | 1][] = [
  [-1, -1, 2, 2, 0],
  [1, 0, 1, 1, 1],
  [-2, 0, 1, 1, 0],
  [0, 1, 1, 1, 1],
  [1, -2, 1, 1, 0],
];

const ORE_SPOTS: { id: OreId; at: [number, number]; colors: [string, string] }[] = [
  { id: "redstone", at: [5.5, -31.5], colors: ["#e03a2c", "#ff7a66"] },
  { id: "coal", at: [9.5, -35.5], colors: ["#26262b", "#4b4b55"] },
  { id: "gold", at: [12.5, -31], colors: ["#f0bf32", "#ffe58a"] },
  { id: "lapis", at: [14.5, -35], colors: ["#2c53c4", "#6f8fec"] },
  { id: "copper", at: [7, -38.5], colors: ["#d67f4c", "#7fc7a8"] },
];

function Ore({ id, at, colors }: (typeof ORE_SPOTS)[number]) {
  const ref = useRef<THREE.Group>(null);
  const active = useLanding((state) => state.ore === id);
  const cells = useMemo<Cell[]>(() => ORE_SHAPE.map(([x, y, width, height, tone]) => [x, y, width, height, colors[tone]]), [colors]);

  useFrame((state, delta) => {
    const group = ref.current;
    if (!group) return;
    const k = damp(delta, 10, scroll.reducedMotion);
    group.scale.setScalar(group.scale.x + ((active ? 1.35 : 1) - group.scale.x) * k);
    group.position.z += ((active ? FRONT + 0.3 : FRONT) - group.position.z) * k;
  });

  return (
    <group
      ref={ref}
      position={[at[0], at[1], FRONT]}
      onPointerOver={(event) => {
        event.stopPropagation();
        hover(true);
        landing.setOre(id);
      }}
      onPointerOut={() => hover(false)}
      onClick={(event) => {
        event.stopPropagation();
        landing.setOre(id);
      }}
    >
      <Sprite cells={cells} unit={0.36} depth={0.4} glow={active} />
    </group>
  );
}

const CAGE_BARS: { at: Vec3; size: Vec3 }[] = [-0.85, 0.85].flatMap((a) =>
  [-0.85, 0.85].flatMap((b) => [
    { at: [a, 0, b] as Vec3, size: [0.14, 1.84, 0.14] as Vec3 },
    { at: [0, a, b] as Vec3, size: [1.84, 0.14, 0.14] as Vec3 },
    { at: [a, b, 0] as Vec3, size: [0.14, 0.14, 1.84] as Vec3 },
  ]),
);

function Spawner() {
  const { spawner } = SPOTS;
  const core = useRef<THREE.Mesh>(null);
  const light = useRef<THREE.PointLight>(null);

  useFrame((state) => {
    const time = scroll.reducedMotion ? 0 : state.clock.elapsedTime;
    if (core.current) {
      core.current.rotation.y = time * 1.4;
      core.current.rotation.x = time * 0.6;
    }
    if (light.current) light.current.intensity = 30 + Math.sin(time * 9) * 5;
  });

  return (
    <group position={[spawner.x, spawner.bottom - 0.5 + 0.92, -0.5]}>
      {CAGE_BARS.map((bar, index) => (
        <Box key={index} at={bar.at} size={bar.size} color="#2d3340" />
      ))}
      <mesh ref={core} geometry={unitBox} scale={0.62}>
        <meshBasicMaterial color="#ff8a3d" />
      </mesh>
      <pointLight ref={light} color="#ff8a3d" intensity={30} distance={9} decay={1.5} />
    </group>
  );
}

function Chest() {
  const { chest } = SPOTS;
  const lid = useRef<THREE.Group>(null);
  const light = useRef<THREE.PointLight>(null);
  const open = useRef(0);

  useFrame((_, delta) => {
    open.current += ((getLanding().chestOpen ? 1 : 0) - open.current) * damp(delta, 8, scroll.reducedMotion);
    if (lid.current) lid.current.rotation.x = -open.current * 1.25;
    if (light.current) light.current.intensity = open.current * 28;
  });

  return (
    <group
      position={[chest.x, chest.bottom - 0.5, -0.4]}
      {...hoverHandlers}
      onClick={(event) => {
        event.stopPropagation();
        landing.toggleChest();
      }}
    >
      <Box at={[0, 0.4, 0]} size={[1.5, 0.8, 1]} color="#a9692c" />
      <Box at={[0, 0.4, 0.01]} size={[1.52, 0.14, 1.02]} color="#5c3a1c" />
      <Box at={[0, 0.72, 0]} size={[1.3, 0.12, 0.8]} color="#ffd65a" glow />
      <group ref={lid} position={[0, 0.8, -0.5]}>
        <Box at={[0, 0.2, 0.5]} size={[1.5, 0.4, 1]} color="#bd7a36" />
        <Box at={[0, 0.12, 1.02]} size={[0.24, 0.3, 0.08]} color="#e9d9a8" />
      </group>
      <pointLight ref={light} color="#ffd27a" intensity={0} distance={7} decay={1.5} position={[0, 1.4, 0.8]} />
    </group>
  );
}

const DIAMOND_SPOTS: [number, number][] = [
  [5, -59.5],
  [13, -62.5],
  [14.5, -58.5],
  [8.5, -64],
];

function Diamonds() {
  const material = useMemo(() => new THREE.MeshBasicMaterial(), []);
  const light = useRef<THREE.PointLight>(null);
  const dim = useMemo(() => new THREE.Color("#3fc4c8"), []);
  const bright = useMemo(() => new THREE.Color("#c8ffff"), []);

  useFrame((state) => {
    const pulse = scroll.reducedMotion ? 0.5 : 0.5 + Math.sin(state.clock.elapsedTime * 2.2) * 0.5;
    material.color.lerpColors(dim, bright, pulse);
    if (light.current) light.current.intensity = 26 + pulse * 22;
  });

  return (
    <>
      {DIAMOND_SPOTS.map(([x, y], spot) => (
        <group key={spot} position={[x, y, FRONT]} rotation={[0, 0, spot * 1.3]}>
          {ORE_SHAPE.map(([cx, cy, width, height], index) => (
            <mesh
              key={index}
              geometry={unitBox}
              material={material}
              position={[(cx + width / 2) * 0.26, (cy + height / 2) * 0.26, 0]}
              scale={[width * 0.26, height * 0.26, 0.36]}
            />
          ))}
        </group>
      ))}
      <pointLight ref={light} color="#7ff3f0" intensity={30} distance={13} decay={1.5} position={[10.5, -61, 2.4]} />
    </>
  );
}

/** An invisible sheet over the bedrock band: clicking it just bounces off. */
function Bedrock() {
  const bumps = useLanding((state) => state.bumps);
  const seen = useRef(bumps);

  useFrame(() => {
    if (seen.current !== bumps) {
      seen.current = bumps;
      ambience.shake = 1;
    }
  });

  return (
    <mesh
      position={[4, -69.5, FRONT]}
      scale={[70, 5, 1]}
      {...hoverHandlers}
      onClick={(event) => {
        event.stopPropagation();
        landing.bumpBedrock();
      }}
    >
      <planeGeometry />
      <meshBasicMaterial transparent opacity={0} depthWrite={false} />
    </mesh>
  );
}

function Portal() {
  const { netherCavern } = SPOTS;
  const film = useRef<THREE.MeshBasicMaterial>(null);
  const light = useRef<THREE.PointLight>(null);
  const base = netherCavern.floor + 1;

  useFrame((state) => {
    const time = scroll.reducedMotion ? 0 : state.clock.elapsedTime;
    if (film.current) film.current.opacity = 0.74 + Math.sin(time * 2.6) * 0.16;
    if (light.current) light.current.intensity = 34 + Math.sin(time * 2.6) * 10;
  });

  return (
    <group position={[netherCavern.x, base, -0.5]}>
      <Box at={[0, 0, 0]} size={[4, 1, 0.9]} color="#1b1626" glow />
      <Box at={[0, 4, 0]} size={[4, 1, 0.9]} color="#1b1626" glow />
      <Box at={[-1.5, 2, 0]} size={[1, 3, 0.9]} color="#241d33" glow />
      <Box at={[1.5, 2, 0]} size={[1, 3, 0.9]} color="#241d33" glow />
      <mesh position={[0, 2, 0]} scale={[2, 3, 1]}>
        <planeGeometry />
        <meshBasicMaterial ref={film} color="#b874ff" transparent opacity={0.7} side={THREE.DoubleSide} depthWrite={false} />
      </mesh>
      <pointLight ref={light} color="#a45cff" intensity={34} distance={10} decay={1.5} position={[0, 2, 2.4]} />
      <pointLight color="#ff7a2a" intensity={70} distance={16} decay={1.5} position={[-7, -3, 2]} />
    </group>
  );
}

export function Underground() {
  return (
    <>
      {BURIED.map((thing) => (
        <BuriedThing key={thing.id} {...thing} />
      ))}
      <Cave />
      {ORE_SPOTS.map((ore) => (
        <Ore key={ore.id} {...ore} />
      ))}
      <Spawner />
      <Chest />
      <Diamonds />
      <Bedrock />
      <Portal />
    </>
  );
}
