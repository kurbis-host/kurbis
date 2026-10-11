import { subdomainSchema } from "../schemas";

export type NameStatus = "empty" | "invalid" | "checking" | "free" | "taken";

const TAKEN = new Set(["steve", "lena", "bigfarm", "alex", "creeper", "admin", "minecraft", "kurbis", "play", "survival"]);

export function validateName(name: string): string | null {
  if (!name) return null;
  const parsed = subdomainSchema.safeParse(name);
  return parsed.success ? null : (parsed.error.issues[0]?.message ?? "Not a valid name.");
}

/** Mocked availability check. Swap for a real endpoint later; the signature stays the same. */
export async function checkName(name: string, signal?: AbortSignal): Promise<"free" | "taken"> {
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, 380);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(new DOMException("Aborted", "AbortError"));
    });
  });
  return TAKEN.has(name) ? "taken" : "free";
}
