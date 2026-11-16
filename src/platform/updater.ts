// Replacement for "@tauri-apps/plugin-updater": the server is updated by its
// administrator, so the in-app updater is disabled.
export type DownloadEvent =
  | { event: "Started"; data: { contentLength?: number } }
  | { event: "Progress"; data: { chunkLength: number } }
  | { event: "Finished" };

export interface Update {
  version: string;
  downloadAndInstall(onEvent?: (e: DownloadEvent) => void): Promise<void>;
}

export async function check(): Promise<Update | null> {
  return null;
}
