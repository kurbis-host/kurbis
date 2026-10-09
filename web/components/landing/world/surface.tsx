"use client";

import { SPOTS } from "../../../lib/landing/terrain";
import { Box } from "./kit";

/** Top face of the ground blocks around the pond. */
const GROUND = 0.5;

function Fisher({ x, facing, shirt }: { x: number; facing: 1 | -1; shirt: string }) {
  return (
    <group position={[x, GROUND, 0]}>
      <Box at={[0, 0.3, 0]} size={[0.46, 0.6, 0.3]} color="#3b4a6b" />
      <Box at={[0, 0.92, 0]} size={[0.56, 0.64, 0.32]} color={shirt} />
      <Box at={[0, 1.47, 0]} size={[0.46, 0.46, 0.4]} color="#e6b98f" />
      <group position={[facing * 0.3, 1.05, 0.1]} rotation={[0, 0, -facing * 0.95]}>
        <Box at={[0, 0.8, 0]} size={[0.07, 1.6, 0.07]} color="#6b4a2c" />
      </group>
      <Box at={[facing * 1.6, 0.85, 0.1]} size={[0.03, 2.1, 0.03]} color="#f4f4f4" />
      <Box at={[facing * 1.6, -0.2, 0.1]} size={[0.16, 0.16, 0.16]} color="#e0483a" />
    </group>
  );
}

const CANES: { dx: number; z: number; height: number }[] = [
  { dx: 4.7, z: -1, height: 2.4 },
  { dx: 5.2, z: -0.4, height: 3.1 },
  { dx: 5.1, z: -1.6, height: 1.8 },
];

/** Two anglers sharing one pond (Java on the left bank, Bedrock on the right) and the sugar cane beside them. */
export function Surface() {
  const pond = SPOTS.pond.x;
  return (
    <>
      <Fisher x={pond - 4} facing={1} shirt="#3f713e" />
      <Fisher x={pond + 4} facing={-1} shirt="#b5532f" />
      {CANES.map((cane, index) => (
        <group key={index} position={[pond + cane.dx, GROUND, cane.z]}>
          <Box at={[0, cane.height / 2, 0]} size={[0.22, cane.height, 0.22]} color="#84c552" />
          <Box at={[0, cane.height * 0.45, 0]} size={[0.26, 0.08, 0.26]} color="#5d9a38" />
          <Box at={[0, cane.height * 0.8, 0]} size={[0.26, 0.08, 0.26]} color="#5d9a38" />
        </group>
      ))}
    </>
  );
}
