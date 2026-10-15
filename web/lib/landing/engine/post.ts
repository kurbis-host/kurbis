/**
 * From the scene's linear, unclamped light to the picture on screen: bloom, the
 * full-frame covers, a vignette, a touch of saturation, then display colour. No tone
 * curve, to match Blender's Standard view.
 */

import {
  BufferAttribute,
  BufferGeometry,
  Color,
  HalfFloatType,
  LinearFilter,
  Mesh,
  OrthographicCamera,
  RGBAFormat,
  Scene,
  ShaderMaterial,
  Vector2,
  WebGLRenderTarget,
  type Camera,
  type Texture,
  type WebGLRenderer,
} from "three";

const LEVELS = 5;

const VERTEX = /* glsl */ `
out vec2 vUv;
void main() {
  vUv = position.xy * 0.5 + 0.5;
  gl_Position = vec4(position.xy, 0.0, 1.0);
}
`;

// Bright parts only, at half size: what goes on to glow.
const BRIGHT = /* glsl */ `
uniform sampler2D tSource;
uniform vec2 uTexel;
uniform float uThreshold;
in vec2 vUv;
void main() {
  vec3 sum = vec3(0.0);
  for (int i = 0; i < 4; i++) {
    vec2 off = vec2(i < 2 ? -0.5 : 0.5, (i & 1) == 0 ? -0.5 : 0.5) * uTexel;
    vec3 c = min(texture(tSource, vUv + off).rgb, vec3(24.0));
    float peak = max(c.r, max(c.g, c.b));
    sum += c * (max(peak - uThreshold, 0.0) / max(peak, 1e-4));
  }
  gl_FragColor = vec4(sum * 0.25, 1.0);
}
`;

const DOWN = /* glsl */ `
uniform sampler2D tSource;
uniform vec2 uTexel;
in vec2 vUv;
void main() {
  vec3 sum = texture(tSource, vUv).rgb * 4.0;
  sum += texture(tSource, vUv + vec2(-1.0, -1.0) * uTexel).rgb;
  sum += texture(tSource, vUv + vec2(1.0, -1.0) * uTexel).rgb;
  sum += texture(tSource, vUv + vec2(-1.0, 1.0) * uTexel).rgb;
  sum += texture(tSource, vUv + vec2(1.0, 1.0) * uTexel).rgb;
  gl_FragColor = vec4(sum / 8.0, 1.0);
}
`;

// A wider, softer copy of the level below added onto this one.
const UP = /* glsl */ `
uniform sampler2D tSource;
uniform sampler2D tWide;
uniform vec2 uTexel;
uniform float uSpread;
in vec2 vUv;
void main() {
  vec3 wide = vec3(0.0);
  wide += texture(tWide, vUv + vec2(-1.0, 0.0) * uTexel).rgb * 2.0;
  wide += texture(tWide, vUv + vec2(1.0, 0.0) * uTexel).rgb * 2.0;
  wide += texture(tWide, vUv + vec2(0.0, -1.0) * uTexel).rgb * 2.0;
  wide += texture(tWide, vUv + vec2(0.0, 1.0) * uTexel).rgb * 2.0;
  wide += texture(tWide, vUv + vec2(-1.0, -1.0) * uTexel).rgb;
  wide += texture(tWide, vUv + vec2(1.0, -1.0) * uTexel).rgb;
  wide += texture(tWide, vUv + vec2(-1.0, 1.0) * uTexel).rgb;
  wide += texture(tWide, vUv + vec2(1.0, 1.0) * uTexel).rgb;
  gl_FragColor = vec4(mix(texture(tSource, vUv).rgb, wide / 12.0, uSpread), 1.0);
}
`;

const FINAL = /* glsl */ `
uniform sampler2D tScene;
uniform sampler2D tBloom;
uniform sampler2D tSwirl;
uniform float uBloom;
uniform float uVignette;
uniform float uSaturation;
uniform float uAspect;
uniform float uWobble;
uniform vec4 uWhite;
uniform float uDark;
uniform float uSwirl;
uniform vec2 uSwirlTile;
uniform float uTime;
uniform float uClock;
in vec2 vUv;

vec3 display(vec3 c) {
  c = clamp(c, 0.0, 1.0);
  return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c));
}

void main() {
  vec2 uv = vUv;
  if (uWobble != 0.0) {
    // The portal bends the picture as a lens would, without showing its corners.
    vec2 centred = uv * 2.0 - 1.0;
    float far = dot(centred, centred);
    centred *= (1.0 + uWobble * far) / (1.0 + uWobble * 2.0 * max(sign(uWobble), 0.0));
    uv = centred * 0.5 + 0.5;
  }
  vec3 color = texture(tScene, uv).rgb + texture(tBloom, uv).rgb * uBloom;
  if (uSwirl > 0.0) {
    // The portal's picture is a strip of 32 frames, one above the other, played as the game plays it.
    vec2 tile = fract((vUv - 0.5) * vec2(uAspect, 1.0) / uSwirlTile + 0.5);
    float frame = mod(floor(uClock * 20.0), 32.0);
    color = mix(color, texture(tSwirl, vec2(tile.x, (31.0 - frame + tile.y) / 32.0)).rgb * 2.4, uSwirl);
  }
  color = mix(color, uWhite.rgb, uWhite.a);
  color *= 1.0 - uDark;
  float grey = dot(color, vec3(0.2126, 0.7152, 0.0722));
  color = mix(vec3(grey), color, uSaturation);
  vec2 from = (vUv - 0.5) / vec2(0.9, 0.86);
  float shade = smoothstep(0.28, 0.78, length(from));
  color *= 1.0 - uVignette * shade;
  float grain = fract(sin(dot(gl_FragCoord.xy + uTime, vec2(12.9898, 78.233))) * 43758.5453) - 0.5;
  gl_FragColor = vec4(display(color) + grain / 255.0, 1.0);
}
`;

export interface Covers {
  /** The cloud the dive goes through: how much of the frame it whites out. */
  white: number;
  /** The portal's swirl over the whole frame. */
  swirl: number;
  /** Lens bend while going through the portal. */
  wobble: number;
  /** A dip to dark, for a film that steps from stop to stop instead of flying. */
  dark: number;
}

export class Post {
  readonly covers: Covers = { white: 0, swirl: 0, wobble: 0, dark: 0 };
  bloom = 0.38;
  threshold = 1.1;
  vignette = 0.32;
  saturation = 1.05;
  enabled = true;

  private readonly scene = new Scene();
  private readonly camera = new OrthographicCamera(-1, 1, 1, -1, 0, 1);
  private readonly quad: Mesh;
  private readonly target: WebGLRenderTarget;
  private readonly levels: WebGLRenderTarget[] = [];
  private readonly bright: ShaderMaterial;
  private readonly down: ShaderMaterial;
  private readonly up: ShaderMaterial;
  private readonly final: ShaderMaterial;
  private readonly size = new Vector2(1, 1);

  constructor(private readonly renderer: WebGLRenderer, samples: number) {
    const geometry = new BufferGeometry();
    geometry.setAttribute("position", new BufferAttribute(new Float32Array([-1, -1, 0, 3, -1, 0, -1, 3, 0]), 3));
    this.quad = new Mesh(geometry);
    this.quad.frustumCulled = false;
    this.scene.add(this.quad);
    this.target = new WebGLRenderTarget(1, 1, { type: HalfFloatType, format: RGBAFormat, samples, depthBuffer: true });
    for (let i = 0; i < LEVELS; i += 1) {
      this.levels.push(new WebGLRenderTarget(1, 1, { type: HalfFloatType, format: RGBAFormat, depthBuffer: false, minFilter: LinearFilter, magFilter: LinearFilter }));
    }
    const pass = (fragmentShader: string, uniforms: ShaderMaterial["uniforms"]) =>
      new ShaderMaterial({ vertexShader: VERTEX, fragmentShader, uniforms, depthTest: false, depthWrite: false });
    this.bright = pass(BRIGHT, { tSource: { value: null }, uTexel: { value: new Vector2() }, uThreshold: { value: this.threshold } });
    this.down = pass(DOWN, { tSource: { value: null }, uTexel: { value: new Vector2() } });
    this.up = pass(UP, { tSource: { value: null }, tWide: { value: null }, uTexel: { value: new Vector2() }, uSpread: { value: 0.55 } });
    this.final = pass(FINAL, {
      tScene: { value: null },
      tBloom: { value: null },
      tSwirl: { value: null },
      uBloom: { value: this.bloom },
      uVignette: { value: this.vignette },
      uSaturation: { value: this.saturation },
      uAspect: { value: 1 },
      uWobble: { value: 0 },
      uWhite: { value: [1.0, 0.68, 0.6, 0] },
      uDark: { value: 0 },
      uSwirl: { value: 0 },
      uSwirlTile: { value: new Vector2(0.98, 0.815) },
      uTime: { value: 0 },
      uClock: { value: 0 },
    });
  }

  /** The cloud's colour, as the previs has it (linear, already at its strength). */
  setWhite(color: Color) {
    const white = this.final.uniforms.uWhite.value as number[];
    white[0] = color.r;
    white[1] = color.g;
    white[2] = color.b;
  }

  /** The cloud's colour, as it was last set. */
  white(): Color {
    const [r, g, b] = this.final.uniforms.uWhite.value as number[];
    return new Color(r, g, b);
  }

  /** The portal's picture, and how big one tile of it is on screen, in frame heights. */
  setSwirl(texture: Texture, tile: [number, number]) {
    this.final.uniforms.tSwirl.value = texture;
    (this.final.uniforms.uSwirlTile.value as Vector2).set(tile[0], tile[1]);
  }

  setSize(width: number, height: number) {
    this.size.set(width, height);
    this.target.setSize(width, height);
    let w = width;
    let h = height;
    for (const level of this.levels) {
      w = Math.max(Math.floor(w / 2), 2);
      h = Math.max(Math.floor(h / 2), 2);
      level.setSize(w, h);
    }
    this.final.uniforms.uAspect.value = width / height;
  }

  private run(material: ShaderMaterial, into: WebGLRenderTarget | null) {
    this.quad.material = material;
    this.renderer.setRenderTarget(into);
    this.renderer.render(this.scene, this.camera);
  }

  render(scene: Scene, camera: Camera, time: number) {
    const { renderer, levels } = this;
    renderer.setRenderTarget(this.target);
    renderer.render(scene, camera);
    if (this.enabled) {
      this.bright.uniforms.tSource.value = this.target.texture;
      (this.bright.uniforms.uTexel.value as Vector2).set(1 / this.size.x, 1 / this.size.y);
      this.bright.uniforms.uThreshold.value = this.threshold;
      this.run(this.bright, levels[0]);
      for (let i = 1; i < levels.length; i += 1) {
        this.down.uniforms.tSource.value = levels[i - 1].texture;
        (this.down.uniforms.uTexel.value as Vector2).set(1 / levels[i - 1].width, 1 / levels[i - 1].height);
        this.run(this.down, levels[i]);
      }
      // Up again: each level takes on a softened copy of the one below it. They are read
      // and written in turn, so the wide one is rendered into a level that is not its source.
      for (let i = levels.length - 2; i >= 0; i -= 1) {
        this.up.uniforms.tSource.value = levels[i].texture;
        this.up.uniforms.tWide.value = levels[i + 1].texture;
        (this.up.uniforms.uTexel.value as Vector2).set(1 / levels[i + 1].width, 1 / levels[i + 1].height);
        this.swap(i);
      }
    }
    const final = this.final.uniforms;
    final.tScene.value = this.target.texture;
    final.tBloom.value = levels[0].texture;
    final.uBloom.value = this.enabled ? this.bloom : 0;
    final.uVignette.value = this.vignette;
    final.uSaturation.value = this.saturation;
    final.uWobble.value = this.covers.wobble;
    (final.uWhite.value as number[])[3] = this.covers.white;
    final.uSwirl.value = this.covers.swirl;
    final.uDark.value = this.covers.dark;
    final.uTime.value = time % 97;
    final.uClock.value = time;
    this.run(this.final, null);
  }

  private readonly spare: WebGLRenderTarget[] = [];

  /** Render the up pass for level i into a spare of the same size, then make that the level. */
  private swap(i: number) {
    const level = this.levels[i];
    let spare = this.spare[i];
    if (!spare) {
      spare = level.clone();
      this.spare[i] = spare;
    }
    if (spare.width !== level.width || spare.height !== level.height) spare.setSize(level.width, level.height);
    this.run(this.up, spare);
    this.levels[i] = spare;
    this.spare[i] = level;
  }

  dispose() {
    this.target.dispose();
    for (const level of [...this.levels, ...this.spare]) level?.dispose();
    for (const material of [this.bright, this.down, this.up, this.final]) material.dispose();
    this.quad.geometry.dispose();
  }
}
