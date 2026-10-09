"use client";

import { Canvas, useFrame, useThree } from "@react-three/fiber";
import { useLayoutEffect, useMemo, useRef } from "react";
import * as THREE from "three";
import { getLanding, scroll } from "../../../lib/landing/store";
import { ambience, clamp01, damp } from "./kit";
import { Mascot } from "./mascot";
import { Sky } from "./sky";
import { Surface } from "./surface";
import { TerrainMesh } from "./terrain-mesh";
import { Underground } from "./underground";

/** Below this aspect ratio the cards sit at the bottom, so the world is framed higher and from further back. */
const PORTRAIT = 0.9;
/**
 * How far a portrait view looks away from the layer: up in the sky (the hero text is on top there),
 * down everywhere else (so the layer shows above the card).
 */
const PORTRAIT_LIFT = 10;
/** Camera distance times aspect ratio that keeps the interactive strip of the world (x 4 to 19) in view. */
const WIDTH_FIT = 41.5;

function Rig() {
  const scene = useThree((state) => state.scene);
  const ambient = useRef<THREE.AmbientLight>(null);
  const sun = useRef<THREE.DirectionalLight>(null);
  const background = useMemo(() => new THREE.Color(), []);
  const sky = useMemo(() => new THREE.Color(), []);
  const day = useMemo(() => new THREE.Color("#9ad0ea"), []);
  const night = useMemo(() => new THREE.Color("#0e1730"), []);
  const earth = useMemo(() => new THREE.Color("#15110f"), []);

  useLayoutEffect(() => {
    ambience.y = scroll.worldY;
    scene.background = background;
  }, [scene, background]);

  useFrame((state, delta) => {
    const instant = scroll.reducedMotion;
    ambience.y += (scroll.worldY - ambience.y) * damp(delta, 5.5, instant);
    ambience.night += ((getLanding().night ? 1 : 0) - ambience.night) * damp(delta, 3, instant);
    ambience.shake = Math.max(0, ambience.shake - delta * 2.4);

    const camera = state.camera as THREE.PerspectiveCamera;
    const portrait = camera.aspect < PORTRAIT;
    const focusX = portrait ? 10 : 6;
    const lift = portrait ? THREE.MathUtils.lerp(PORTRAIT_LIFT, -PORTRAIT_LIFT, clamp01(scroll.layerF)) : 0;
    ambience.lift += (lift - ambience.lift) * damp(delta, 5.5, instant);
    const focusY = ambience.y + ambience.lift;
    const shake = instant ? 0 : Math.sin(state.clock.elapsedTime * 55) * 0.22 * ambience.shake;
    const sway = instant ? 0 : 1;
    camera.position.set(
      focusX + state.pointer.x * 0.7 * sway,
      focusY + 2.4 + state.pointer.y * 0.4 * sway + shake,
      portrait ? 46 : Math.min(Math.max(WIDTH_FIT / camera.aspect, 25), 46),
    );
    camera.lookAt(focusX, focusY + shake, -1);

    const below = clamp01((-4 - ambience.y) / 10);
    sky.lerpColors(day, night, ambience.night);
    background.lerpColors(sky, earth, below);
    if (ambient.current) {
      ambient.current.intensity = THREE.MathUtils.lerp(THREE.MathUtils.lerp(2.5, 0.9, ambience.night), 1.25, below);
    }
    if (sun.current) {
      sun.current.intensity = THREE.MathUtils.lerp(THREE.MathUtils.lerp(2.2, 0.5, ambience.night), 0.8, below);
    }
  });

  return (
    <>
      <ambientLight ref={ambient} intensity={2.5} />
      <directionalLight ref={sun} intensity={2.2} position={[-8, 18, 14]} />
    </>
  );
}

export default function WorldScene() {
  return (
    <Canvas
      flat
      dpr={[1, 1.75]}
      camera={{ fov: 36, near: 0.5, far: 160, position: [6, 14, 25] }}
      gl={{ antialias: true, powerPreference: "high-performance" }}
      style={{ touchAction: "pan-y" }}
    >
      <Rig />
      <TerrainMesh />
      <Sky />
      <Surface />
      <Mascot />
      <Underground />
    </Canvas>
  );
}
