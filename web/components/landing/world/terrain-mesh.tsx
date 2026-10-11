"use client";

import { useLayoutEffect, useMemo, useRef } from "react";
import * as THREE from "three";
import { generateTerrain, type BlockBatch, type BlockType } from "../../../lib/landing/terrain";
import { unitBox } from "./kit";

const PALETTE: Record<BlockType, string> = {
  grass: "#7fae4d",
  dirt: "#86603f",
  stone: "#93928b",
  deep: "#575a63",
  bedrock: "#2e2f36",
  nether: "#8a3030",
  sand: "#ead9a4",
  water: "#4d93d6",
  leaf: "#4f9440",
  log: "#73502f",
  moss: "#75906a",
  lava: "#ff8421",
};

/**
 * A greyscale tile multiplied by each block's colour: a lit top-left edge, a
 * shaded bottom-right edge and a few flecks. This is our own block look.
 */
function tileTexture(): THREE.Texture {
  const size = 16;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = size;
  const context = canvas.getContext("2d");
  if (context) {
    context.fillStyle = "#e4e4e4";
    context.fillRect(0, 0, size, size);
    context.fillStyle = "#ffffff";
    context.fillRect(0, 0, size, 1);
    context.fillRect(0, 0, 1, size);
    context.fillStyle = "#b9b9b9";
    context.fillRect(0, size - 1, size, 1);
    context.fillRect(size - 1, 0, 1, size);
    const flecks: [number, number, number, string][] = [
      [3, 4, 2, "#d2d2d2"],
      [10, 3, 2, "#f4f4f4"],
      [6, 9, 3, "#d6d6d6"],
      [12, 11, 2, "#cccccc"],
      [3, 12, 2, "#f0f0f0"],
    ];
    for (const [x, y, width, shade] of flecks) {
      context.fillStyle = shade;
      context.fillRect(x, y, width, 1);
    }
  }
  const texture = new THREE.CanvasTexture(canvas);
  texture.magFilter = THREE.NearestFilter;
  texture.minFilter = THREE.NearestMipmapLinearFilter;
  texture.colorSpace = THREE.SRGBColorSpace;
  return texture;
}

function Batch({ batch, texture }: { batch: BlockBatch; texture: THREE.Texture }) {
  const ref = useRef<THREE.InstancedMesh>(null);

  useLayoutEffect(() => {
    const mesh = ref.current;
    if (!mesh) return;
    const matrix = new THREE.Matrix4();
    const base = new THREE.Color(PALETTE[batch.type]);
    const color = new THREE.Color();
    for (let index = 0; index < batch.count; index += 1) {
      matrix.setPosition(batch.positions[index * 3], batch.positions[index * 3 + 1], batch.positions[index * 3 + 2]);
      mesh.setMatrixAt(index, matrix);
      mesh.setColorAt(index, color.copy(base).multiplyScalar(batch.shades[index]));
    }
    mesh.instanceMatrix.needsUpdate = true;
    if (mesh.instanceColor) mesh.instanceColor.needsUpdate = true;
  }, [batch]);

  return (
    <instancedMesh ref={ref} args={[unitBox, undefined, batch.count]} frustumCulled={false}>
      {batch.type === "lava" ? (
        <meshBasicMaterial map={texture} />
      ) : batch.type === "water" ? (
        <meshLambertMaterial map={texture} transparent opacity={0.78} />
      ) : (
        <meshLambertMaterial map={texture} />
      )}
    </instancedMesh>
  );
}

export function TerrainMesh() {
  const batches = useMemo(generateTerrain, []);
  const texture = useMemo(tileTexture, []);
  return (
    <>
      {batches.map((batch) => (
        <Batch key={batch.type} batch={batch} texture={texture} />
      ))}
    </>
  );
}
