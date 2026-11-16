// Replacement for "@tauri-apps/api/core": invoke() becomes POST /api/invoke/:cmd.
import { apiFetch, errorFrom } from "./http";

export async function invoke<T = unknown>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  // The desktop app opened a "save file" dialog; on the web it is a download.
  if (cmd === "export_pairing_cmd") {
    await downloadPairingFile();
    return undefined as T;
  }
  const resp = await apiFetch(`/api/invoke/${encodeURIComponent(cmd)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(args ?? {}),
  });
  if (!resp.ok) throw await errorFrom(resp);
  const text = await resp.text();
  return (text ? JSON.parse(text) : null) as T;
}

async function downloadPairingFile(): Promise<void> {
  const resp = await apiFetch("/api/pairing/export");
  if (!resp.ok) throw await errorFrom(resp);
  const blob = await resp.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = "pairingFile.plist";
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
