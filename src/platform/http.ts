// HTTP helpers for talking to iloader-server (same origin).

export type ApiError = { type: string; message: string };

export const UNAUTHORIZED_EVENT = "iloader:unauthorized";

export async function apiFetch(
  path: string,
  init: RequestInit = {},
): Promise<Response> {
  let resp: Response;
  try {
    resp = await fetch(path, { credentials: "same-origin", ...init });
  } catch (e) {
    throw {
      type: "network",
      message: `Unable to reach the iloader server: ${String(e)}`,
    } satisfies ApiError;
  }
  if (resp.status === 401 && !path.startsWith("/api/auth/login")) {
    window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
  }
  return resp;
}

export async function errorFrom(resp: Response): Promise<ApiError> {
  try {
    const body = await resp.json();
    if (body && typeof body.type === "string") return body as ApiError;
    return { type: "misc", message: JSON.stringify(body) };
  } catch {
    return { type: "misc", message: `HTTP ${resp.status} ${resp.statusText}` };
  }
}
