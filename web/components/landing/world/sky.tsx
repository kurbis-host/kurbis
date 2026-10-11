"use client";

import { useFrame, type RootState } from "@react-three/fiber";
import { useLayoutEffect, useMemo, useRef } from "react";
import * as THREE from "three";
import { CLOUD_JOKES } from "../../../lib/landing/copy";
import { getLanding, landing, scroll } from "../../../lib/landing/store";
import { ambience, Box, damp, hover, unitBox, type Vec3 } from "./kit";

const CLOUDS: { at: Vec3; scale: number; anchored?: boolean }[] = [
  { at: [11, 13, 0], scale: 1, anchored: true },
  { at: [-3, 20, -3], scale: 1.2 },
  { at: [19.5, 10.5, -2], scale: 0.9 },
  { at: [6, 18.5, -4], scale: 1.4 },
  { at: [22, 17, -3], scale: 1.1 },
];

const point = new THREE.Vector3();

/** Where the pointer ray crosses the plane at depth `z`. */
function pointerOnPlane(state: RootState, z: number): THREE.Vector3 {
  point.set(state.pointer.x, state.pointer.y, 0.5).unproject(state.camera).sub(state.camera.position).normalize();
  return point.multiplyScalar((z - state.camera.position.z) / point.z).add(state.camera.position);
}

function Cloud({ index, at, scale, anchored }: { index: number; at: Vec3; scale: number; anchored?: boolean }) {
  const ref = useRef<THREE.Group>(null);
  const push = useRef(0);

  useFrame((state, delta) => {
    const group = ref.current;
    if (!group || anchored) return;
    const drift = scroll.reducedMotion ? 0 : Math.sin(state.clock.elapsedTime * 0.11 + index * 2.1) * 1.4;
    const cursor = pointerOnPlane(state, at[2]);
    const dx = at[0] + drift - cursor.x;
    const distance = Math.hypot(dx, at[1] - cursor.y);
    const target = distance < 4.5 ? Math.sign(dx || 1) * (4.5 - distance) * 0.5 : 0;
    push.current += (target - push.current) * damp(delta, 4, scroll.reducedMotion);
    group.position.x = at[0] + drift + push.current;
  });

  return (
    <group
      ref={ref}
      position={at}
      scale={scale}
      onPointerOver={(event) => {
        event.stopPropagation();
        hover(true);
        landing.joke(CLOUD_JOKES[index % CLOUD_JOKES.length]);
      }}
      onPointerOut={() => hover(false)}
      onClick={(event) => {
        event.stopPropagation();
        landing.rain();
      }}
    >
      <Box at={[0, 0, 0]} size={[4.2, 0.9, 2]} color="#ffffff" />
      <Box at={[-0.5, 0.75, 0]} size={[2.4, 0.7, 1.6]} color="#ffffff" />
      <Box at={[1.4, 0.6, -0.2]} size={[1.2, 0.5, 1.2]} color="#f1f6f8" />
    </group>
  );
}

function SunMoon() {
  const ref = useRef<THREE.Mesh>(null);
  const material = useMemo(() => new THREE.MeshBasicMaterial(), []);
  const sun = useMemo(() => new THREE.Color("#ffd24d"), []);
  const moon = useMemo(() => new THREE.Color("#eef1f8"), []);

  useFrame(() => {
    material.color.lerpColors(sun, moon, ambience.night);
    if (ref.current) ref.current.rotation.z = (1 - ambience.night) * (Math.PI / 4);
  });

  return (
    <mesh
      ref={ref}
      geometry={unitBox}
      material={material}
      position={[15.5, 17.5, -5]}
      scale={[2.6, 2.6, 0.6]}
      onPointerOver={(event) => {
        event.stopPropagation();
        hover(true);
      }}
      onPointerOut={() => hover(false)}
      onClick={(event) => {
        event.stopPropagation();
        landing.toggleNight();
      }}
    />
  );
}

function Stars() {
  const material = useRef<THREE.PointsMaterial>(null);
  const geometry = useMemo(() => {
    const positions = new Float32Array(160 * 3);
    for (let index = 0; index < 160; index += 1) {
      positions[index * 3] = -34 + ((index * 73) % 80);
      positions[index * 3 + 1] = 5 + ((index * 37) % 31) + (index % 3) * 0.4;
      positions[index * 3 + 2] = -9;
    }
    return new THREE.BufferGeometry().setAttribute("position", new THREE.BufferAttribute(positions, 3));
  }, []);

  useFrame(() => {
    if (material.current) material.current.opacity = ambience.night;
  });

  return (
    <points geometry={geometry}>
      <pointsMaterial ref={material} color="#ffffff" size={0.16} transparent opacity={0} depthWrite={false} />
    </points>
  );
}

const DROPS = 140;

function Rain() {
  const ref = useRef<THREE.InstancedMesh>(null);
  const material = useRef<THREE.MeshBasicMaterial>(null);
  const strength = useRef(0);
  const matrix = useMemo(() => new THREE.Matrix4(), []);

  useLayoutEffect(() => {
    if (ref.current) ref.current.visible = false;
  }, []);

  useFrame((state, delta) => {
    const mesh = ref.current;
    if (!mesh || !material.current) return;
    strength.current += ((getLanding().raining ? 1 : 0) - strength.current) * damp(delta, 6);
    mesh.visible = strength.current > 0.02;
    if (!mesh.visible) return;
    material.current.opacity = strength.current * 0.7;
    for (let index = 0; index < DROPS; index += 1) {
      const x = -16 + ((index * 53) % 44) + (index % 5) * 0.2;
      const fall = (state.clock.elapsedTime * 26 + index * 3.7) % 24;
      matrix.setPosition(x, 22 - fall, ((index * 7) % 5) - 2);
      mesh.setMatrixAt(index, matrix);
    }
    mesh.instanceMatrix.needsUpdate = true;
  });

  return (
    <instancedMesh ref={ref} args={[undefined, undefined, DROPS]} frustumCulled={false}>
      <boxGeometry args={[0.05, 0.8, 0.05]} />
      <meshBasicMaterial ref={material} color="#cfe9ff" transparent opacity={0} depthWrite={false} />
    </instancedMesh>
  );
}

export function Sky() {
  return (
    <>
      {CLOUDS.map((cloud, index) => (
        <Cloud key={index} index={index} {...cloud} />
      ))}
      <SunMoon />
      <Stars />
      <Rain />
    </>
  );
}
