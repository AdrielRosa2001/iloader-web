// Replacement for "@tauri-apps/plugin-process".
export async function relaunch(): Promise<void> {
  location.reload();
}
