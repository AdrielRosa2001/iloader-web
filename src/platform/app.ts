// Replacement for "@tauri-apps/api/app".
import { apiFetch } from "./http";

export async function getVersion(): Promise<string> {
  try {
    const resp = await apiFetch("/api/version");
    if (!resp.ok) return "?";
    return (await resp.json()).version ?? "?";
  } catch {
    return "?";
  }
}
