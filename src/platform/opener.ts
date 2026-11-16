// Replacement for "@tauri-apps/plugin-opener".
export async function openUrl(url: string | URL): Promise<void> {
  window.open(String(url), "_blank", "noopener,noreferrer");
}
