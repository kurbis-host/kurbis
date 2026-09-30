export const DEFAULT_CONTROL_PLANE_INTERNAL_URL = "http://127.0.0.1:8080";

export function controlPlaneOrigin(value = process.env.CONTROL_PLANE_INTERNAL_URL) {
  const configured = value?.trim() || DEFAULT_CONTROL_PLANE_INTERNAL_URL;
  let origin: URL;

  try {
    origin = new URL(configured);
  } catch {
    throw new Error(
      `CONTROL_PLANE_INTERNAL_URL must be an absolute http(s) URL; received ${JSON.stringify(configured)}`,
    );
  }

  if (origin.protocol !== "http:" && origin.protocol !== "https:") {
    throw new Error(
      `CONTROL_PLANE_INTERNAL_URL must use http or https; received ${JSON.stringify(configured)}`,
    );
  }

  return configured.replace(/\/+$/, "");
}
