"use client";

import type { ThreeElements } from "@react-three/fiber";
import * as THREE from "three";

export type Vec3 = [number, number, number];

/** One cell of a flat pixel sprite: x, y, width, height (in sprite pixels) and colour. */
export type Cell = readonly [number, number, number, number, string];

export const unitBox = new THREE.BoxGeometry(1, 1, 1);

type BoxProps = Omit<ThreeElements["mesh"], "position" | "scale"> & {
  at: Vec3;
  size: Vec3 | number;
  color: string;
  /** Unlit, so it reads as a light source. */
  glow?: boolean;
};

export function Box({ at, size, color, glow, ...props }: BoxProps) {
  return (
    <mesh geometry={unitBox} position={at} scale={size} {...props}>
      {glow ? <meshBasicMaterial color={color} /> : <meshLambertMaterial color={color} />}
    </mesh>
  );
}

/** Extrudes a pixel sprite into boxes, centred on the group origin. */
export function Sprite({ cells, unit = 0.2, depth = 0.3, glow }: { cells: readonly Cell[]; unit?: number; depth?: number; glow?: boolean }) {
  return (
    <>
      {cells.map(([x, y, width, height, color], index) => (
        <Box
          key={index}
          at={[(x + width / 2) * unit, (y + height / 2) * unit, 0]}
          size={[width * unit, height * unit, depth]}
          color={color}
          glow={glow}
        />
      ))}
    </>
  );
}

/** Marks the page as hovering something clickable in the 3D world. */
export function hover(on: boolean) {
  document.documentElement.classList.toggle("landing-hover", on);
}

export const hoverHandlers = {
  onPointerOver: (event: { stopPropagation: () => void }) => {
    event.stopPropagation();
    hover(true);
  },
  onPointerOut: () => hover(false),
};

/** Smoothed scene-wide values, written by the camera rig once per frame. */
export const ambience = { night: 0, y: 12, shake: 0, lift: 0 };

/** Frame-rate independent smoothing factor. */
export function damp(delta: number, speed: number, instant = false): number {
  return instant ? 1 : 1 - Math.exp(-delta * speed);
}

export function clamp01(value: number): number {
  return Math.min(Math.max(value, 0), 1);
}
