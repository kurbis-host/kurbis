/**
 * The sky: a gradient by elevation, a glow and a disc round the sun, stars high up,
 * and the valley's haze in front of it all. Drawn as one triangle behind everything.
 */

import { BufferAttribute, BufferGeometry, Mesh, ShaderMaterial, Vector3, Vector4 } from "three";
import type { SkyNote } from "./bundle";
import { HAZE_GLSL, type Globals } from "./shading";

const VERTEX = /* glsl */ `
out vec3 vDir;
void main() {
  vec4 view = inverse(projectionMatrix) * vec4(position.xy, 1.0, 1.0);
  vDir = transpose(mat3(viewMatrix)) * (view.xyz / view.w);
  gl_Position = vec4(position.xy, 1.0, 1.0);
}
`;

const FRAGMENT = /* glsl */ `
${HAZE_GLSL}
#define STOPS 8
uniform vec4 uStops[STOPS];
uniform vec3 uGlow;
uniform vec4 uLobes;
uniform float uStars;
uniform float uStarsFrom;
uniform float uStrength;
uniform vec3 uDisc;
in vec3 vDir;

vec3 hash3(vec3 p) {
  p = fract(p * vec3(0.1031, 0.1030, 0.0973));
  p += dot(p, p.yxz + 33.33);
  return fract((p.xxy + p.yxx) * p.zyx);
}

void main() {
  vec3 dir = normalize(vDir);
  vec3 color = uStops[0].yzw;
  for (int i = 1; i < STOPS; i++) {
    float span = max(uStops[i].x - uStops[i - 1].x, 1e-5);
    color = mix(color, uStops[i].yzw, clamp((dir.z - uStops[i - 1].x) / span, 0.0, 1.0));
  }
  float toSun = clamp(dot(dir, uDisc), 0.0, 1.0);
  color += uGlow * (pow(toSun, uLobes.x) * uLobes.y + pow(toSun, uLobes.z) * uLobes.w);
  if (uStars > 0.0) {
    vec3 p = dir * 130.0;
    vec3 cell = floor(p);
    float speck = step(distance(p, cell + hash3(cell)), 0.06);
    color += vec3(1.0, 0.92, 0.82) * (speck * clamp((dir.z - uStarsFrom) / 0.44, 0.0, 1.0) * uStars * 0.5);
  }
  gl_FragColor = vec4(throughHaze(color * uStrength, dir, 1.0e5, 0.0), 1.0);
}
`;

export function makeSky(globals: Globals, note: SkyNote): Mesh {
  const stops = [...note.stops].sort((a, b) => a[0] - b[0]);
  const last = stops[stops.length - 1];
  const padded = Array.from({ length: 8 }, (_, i) => stops[i] ?? last);
  const material = new ShaderMaterial({
    uniforms: {
      ...globals,
      uStops: { value: padded.map(([at, rgb]) => new Vector4(at, rgb[0], rgb[1], rgb[2])) },
      uGlow: { value: new Vector3(...note.glow) },
      uLobes: { value: new Vector4(note.glow_power, note.glow_gain, note.disc_power, note.disc) },
      uStars: { value: note.stars },
      // How high over the horizon they begin to show (the night the chat can bring has them lower: film/weather.ts).
      uStarsFrom: { value: 0.16 },
      // Where the disc is drawn: a little lower than the lamp that lights the land.
      uDisc: { value: new Vector3(...note.sun).normalize() },
      uStrength: { value: note.strength },
    },
    vertexShader: VERTEX,
    fragmentShader: FRAGMENT,
    depthTest: false,
    depthWrite: false,
  });
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(new Float32Array([-1, -1, 0, 3, -1, 0, -1, 3, 0]), 3));
  const mesh = new Mesh(geometry, material);
  mesh.name = "Sky";
  mesh.frustumCulled = false;
  mesh.renderOrder = -1000;
  return mesh;
}
