import type { components } from "./generated";

export class ApiError extends Error {
  constructor(message: string, public readonly status: number) {
    super(message);
    this.name = "ApiError";
  }
}

type ApiResponse<T> = { data?: T; error?: unknown; response: Response };

function toApiError(result: ApiResponse<unknown>) {
  const payload = result.error as components["schemas"]["ApiErrorResponse"] | undefined;
  return new ApiError(
    payload?.message ?? payload?.code ?? `Request failed (${result.response.status})`,
    result.response.status,
  );
}

export async function apiRequest<T>(request: Promise<ApiResponse<T>>): Promise<T> {
  const result = await request;
  if (!result.response.ok || result.error !== undefined) throw toApiError(result);
  return result.data as T;
}

export async function apiVoid(request: Promise<ApiResponse<unknown>>): Promise<void> {
  await apiRequest(request);
}

export function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : "The request could not be completed.";
}
