"use client";

import { useFrame } from "@react-three/fiber";
import { useMemo, useRef } from "react";
import * as THREE from "three";
import { scroll } from "../../../lib/landing/store";
import { SPOTS } from "../../../lib/landing/terrain";
import { ambience, Box, clamp01, damp, unitBox, type Vec3 } from "./kit";

const CLOUD_PERCH: Vec3 = [11, 14.35, 0];
/** Fraction of the sky layer scrolled before the pumpkin lets go of its cloud. */
const DROP_START = 0.15;
const DROP_LENGTH = 0.7;
/** Layer position at which it leaves the grass and starts following the camera down. */
const FOLLOW_START = 1.45;

const FACE: { at: Vec3; size: Vec3 }[] = [
  { at: [-0.38, 0.2, 0.77], size: [0.3, 0.3, 0.06] },
  { at: [0.38, 0.2, 0.77], size: [0.3, 0.3, 0.06] },
  { at: [0, -0.32, 0.77], size: [0.86, 0.2, 0.06] },
  { at: [-0.5, -0.18, 0.77], size: [0.14, 0.14, 0.06] },
  { at: [0.5, -0.18, 0.77], size: [0.14, 0.14, 0.06] },
];

/**
 * The Kurbis pumpkin. It drops from its cloud as the visitor leaves the sky,
 * lands in the grass, then rides down the edge of the view as a lantern.
 */
export function Mascot() {
  const ref = useRef<THREE.Group>(null);
  const light = useRef<THREE.PointLight>(null);
  const target = useMemo(() => new THREE.Vector3(), []);
  const face = useMemo(() => new THREE.MeshBasicMaterial(), []);
  const carved = useMemo(() => new THREE.Color("#3a2110"), []);
  const lit = useMemo(() => new THREE.Color("#ffd98a"), []);

  useFrame((state, delta) => {
    const group = ref.current;
    if (!group) return;
    const layerF = scroll.layerF;
    const camera = state.camera as THREE.PerspectiveCamera;
    const time = state.clock.elapsedTime;

    const drop = clamp01((layerF - DROP_START) / DROP_LENGTH);
    const landed = SPOTS.mascotLanding.y + 0.65;
    target.set(
      THREE.MathUtils.lerp(CLOUD_PERCH[0], SPOTS.mascotLanding.x, drop),
      THREE.MathUtils.lerp(CLOUD_PERCH[1], landed, drop * drop),
      0,
    );
    const sinceLanding = layerF - (DROP_START + DROP_LENGTH);
    if (sinceLanding > 0 && sinceLanding < 0.45) {
      target.y += Math.abs(Math.sin(sinceLanding * 14)) * 0.6 * (1 - sinceLanding / 0.45);
    }

    const follow = THREE.MathUtils.smoothstep(layerF, FOLLOW_START, FOLLOW_START + 0.5);
    if (follow > 0) {
      const halfHeight = Math.tan(THREE.MathUtils.degToRad(camera.fov / 2)) * (camera.position.z - 1.4);
      const portrait = camera.aspect < 0.9;
      const bob = scroll.reducedMotion ? 0 : Math.sin(time * 1.6) * 0.22;
      target.x = THREE.MathUtils.lerp(target.x, camera.position.x + halfHeight * camera.aspect - 1.9, follow);
      target.y = THREE.MathUtils.lerp(target.y, scroll.worldY + (portrait ? 1.5 : 1.2) + bob, follow);
      target.z = 1.4 * follow;
    }

    group.position.lerp(target, damp(delta, 8, scroll.reducedMotion));
    group.rotation.z = drop < 1 ? -drop * Math.PI * 2 : 0;
    group.rotation.y = follow * (-0.35 + Math.sin(time * 0.9) * 0.12);

    const glow = Math.max(ambience.night, clamp01((2 - ambience.y) / 8));
    face.color.lerpColors(carved, lit, glow);
    if (light.current) light.current.intensity = glow * 26;
  });

  return (
    <group ref={ref} position={CLOUD_PERCH}>
      <Box at={[0, 0, 0]} size={[1.6, 1.3, 1.5]} color="#ec7a2b" />
      <Box at={[-0.42, 0, 0.02]} size={[0.12, 1.3, 1.52]} color="#cd5c1c" />
      <Box at={[0.42, 0, 0.02]} size={[0.12, 1.3, 1.52]} color="#cd5c1c" />
      <Box at={[0, 0.69, 0]} size={[1.2, 0.1, 1.1]} color="#d9681f" />
      <Box at={[0.06, 0.9, 0]} size={[0.26, 0.36, 0.26]} color="#5e7d38" />
      {FACE.map((part, index) => (
        <mesh key={index} geometry={unitBox} material={face} position={part.at} scale={part.size} />
      ))}
      <pointLight ref={light} color="#ffb765" intensity={0} distance={9} decay={1.6} position={[0, 0, 1.6]} />
    </group>
  );
}
