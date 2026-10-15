/**
 * Flakes: leaves, petals, dust, embers, sparks, rain. Each is a small square at its own
 * tilt that travels with the wind for ever, wraps round inside its box and wavers
 * sideways. All of one kind are a single instanced mesh moved by its vertex shader.
 */

import { DoubleSide, InstancedBufferAttribute, InstancedBufferGeometry, BufferAttribute, Mesh, ShaderMaterial, Sphere, Vector3 } from "three";
import { seeded } from "../anim/easing";
import type { MaterialNote, TypeNote } from "./bundle";
import { HAZE_GLSL, LIGHT_GLSL, linearColor, type Globals } from "./shading";

const VERTEX = /* glsl */ `
${LIGHT_GLSL}
in vec3 aCentre;
in vec3 aAcross;
in vec3 aUp;
in float aPhase;
uniform float uTime;
uniform vec3 uLo;
uniform vec3 uSize;
uniform vec3 uVelocity;
uniform float uSway;
uniform vec3 uColor;
uniform vec3 uGlow;
uniform float uGrow;
uniform float uNear;
out vec3 vColor;
out vec3 vWorld;
void main() {
  vec3 centre = mod(aCentre - uLo + uVelocity * uTime, uSize) + uLo;
  float waver = sin(uTime * 1.7 + aPhase) * uSway;
  // (A flake nearer the eye than uNear shrinks away, rather than fill the frame: rain all round the camera.)
  float kept = uNear > 0.0 ? smoothstep(uNear, 2.0 * uNear, distance((modelMatrix * vec4(centre, 1.0)).xyz, cameraPosition)) : 1.0;
  vec3 world = (modelMatrix * vec4(centre + (aAcross * position.x + aUp * position.y) * uGrow * kept + vec3(waver, waver * 0.6, 0.0), 1.0)).xyz;
  // A flake is lit as a scrap of paper is: from either side.
  vec3 normal = normalize(cross(aAcross, aUp));
  float day = daylightAt(world);
  vec4 baked = vec4(day, 0.0, day, day);
  vColor = uColor * 0.5 * (lightOn(normal, baked, world) + lightOn(-normal, baked, world)) + uGlow;
  vWorld = world;
  gl_Position = projectionMatrix * viewMatrix * vec4(world, 1.0);
}
`;

const FRAGMENT = /* glsl */ `
${HAZE_GLSL}
uniform float uTint;
in vec3 vColor;
in vec3 vWorld;
void main() {
  gl_FragColor = vec4(seenFrom(vColor, vWorld), uTint);
}
`;

/** Build the flakes a note of the art describes, in the material it names. */
export function flakesMesh(globals: Globals, note: TypeNote, look: MaterialNote | undefined, alpha: number): Mesh {
  const lo = note.lo ?? [0, 0, 0];
  const hi = note.hi ?? [1, 1, 1];
  const count = note.count ?? 0;
  const [small, large] = note.size ?? [0.05, 0.1];
  const random = seeded((note.seed ?? 0) * 7919 + 13);
  const normal = () => {
    // A direction spread evenly over the sphere.
    const z = 2 * random() - 1;
    const turn = 2 * Math.PI * random();
    const flat = Math.sqrt(1 - z * z);
    return new Vector3(flat * Math.cos(turn), flat * Math.sin(turn), z);
  };
  const centres = new Float32Array(count * 3);
  const across = new Float32Array(count * 3);
  const up = new Float32Array(count * 3);
  const phases = new Float32Array(count);
  for (let i = 0; i < count; i += 1) {
    for (let c = 0; c < 3; c += 1) centres[i * 3 + c] = lo[c] + (hi[c] - lo[c]) * random();
    let a: Vector3;
    let b: Vector3;
    if (note.streaks) {
      // Thin upright strokes that face the viewer: rain.
      a = new Vector3(small / 2, 0, 0);
      b = new Vector3(0, 0, large / 2);
    } else {
      const size = small + (large - small) * random();
      a = normal();
      b = new Vector3().crossVectors(a, normal()).normalize().multiplyScalar(size);
      a.multiplyScalar(size);
    }
    a.toArray(across, i * 3);
    b.toArray(up, i * 3);
    phases[i] = random() * 2 * Math.PI;
  }
  const geometry = new InstancedBufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(new Float32Array([-1, -1, 0, 1, -1, 0, 1, 1, 0, -1, 1, 0]), 3));
  geometry.setIndex([0, 1, 2, 0, 2, 3]);
  geometry.setAttribute("aCentre", new InstancedBufferAttribute(centres, 3));
  geometry.setAttribute("aAcross", new InstancedBufferAttribute(across, 3));
  geometry.setAttribute("aUp", new InstancedBufferAttribute(up, 3));
  geometry.setAttribute("aPhase", new InstancedBufferAttribute(phases, 1));
  geometry.instanceCount = count;
  const middle = new Vector3((lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2, (lo[2] + hi[2]) / 2);
  geometry.boundingSphere = new Sphere(middle, middle.distanceTo(new Vector3(hi[0], hi[1], hi[2])) + large + 1);

  const fading = look?.kind === "fading";
  const color = fading ? [0, 0, 0] : linearColor(look?.rgb ?? [1, 1, 1]).toArray();
  const glow = fading ? linearColor(look?.rgb ?? [1, 1, 1], look?.strength ?? 1).toArray() : linearColor(look?.emit_rgb ?? look?.rgb ?? [1, 1, 1], look?.emit ?? 0).toArray();
  const material = new ShaderMaterial({
    uniforms: {
      ...globals,
      uLo: { value: lo },
      uSize: { value: [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]] },
      uVelocity: { value: note.drift?.velocity ?? [0, 0, 0] },
      uSway: { value: note.drift?.sway ?? 0 },
      uColor: { value: color },
      uGlow: { value: glow },
      uTint: { value: alpha },
      uGrow: { value: 1 },
      uNear: { value: 0 },
    },
    vertexShader: VERTEX,
    fragmentShader: FRAGMENT,
    side: DoubleSide,
    transparent: fading,
    depthWrite: !fading,
  });
  const mesh = new Mesh(geometry, material);
  mesh.userData.tint = alpha;
  mesh.onBeforeRender = () => {
    material.uniforms.uTint.value = mesh.userData.tint ?? 1;
    material.uniforms.uGrow.value = mesh.userData.grow ?? 1; // each flake's size: a burst swells in from nothing
  };
  return mesh;
}
