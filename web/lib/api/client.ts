import type { ZodType } from "zod";

export class ApiError extends Error {
  constructor(message: string, public readonly status: number, public readonly detail?: string) {
    super(message);
    this.name = "ApiError";
  }
}

async function errorFromResponse(response: Response) {
  let message = `Request failed (${response.status})`;
  let detail: string | undefined;
  try {
    const payload = (await response.json()) as { error?: string; message?: string; detail?: string };
    message = payload.message ?? payload.error ?? message;
    detail = payload.detail;
  } catch {}
  return new ApiError(message, response.status, detail);
}

export async function apiRequest<T>(path: string, schema: ZodType<T>, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...init,
    credentials: "include",
    headers: { accept: "application/json", ...(init?.body ? { "content-type": "application/json" } : {}), ...init?.headers },
  });
  if (!response.ok) throw await errorFromResponse(response);
  if (response.status === 204) return schema.parse(undefined);
  return schema.parse(await response.json());
}

export async function apiVoid(path: string, init?: RequestInit) {
  const response = await fetch(path, {
    ...init,
    credentials: "include",
    headers: { accept: "application/json", ...(init?.body ? { "content-type": "application/json" } : {}), ...init?.headers },
  });
  if (!response.ok) throw await errorFromResponse(response);
}

export function jsonBody(value: unknown) { return JSON.stringify(value); }
export function errorMessage(error: unknown) { return error instanceof Error ? error.message : "The request could not be completed."; }
