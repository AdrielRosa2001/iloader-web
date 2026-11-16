// Replacement for "@tauri-apps/plugin-store".
// * "data.json" (saved Apple IDs) lives on the server: get("ids") → saved_accounts.
// * Anything else (UI preferences) is kept in this browser's localStorage.
import { invoke } from "./core";

export class Store {
  constructor(private name: string) {}

  private prefix() {
    return `iloader:${this.name}:`;
  }

  async keys(): Promise<string[]> {
    if (this.name === "data.json") return ["ids"];
    const keys: string[] = [];
    try {
      for (let i = 0; i < localStorage.length; i++) {
        const k = localStorage.key(i);
        if (k?.startsWith(this.prefix())) keys.push(k.slice(this.prefix().length));
      }
    } catch {
      /* storage unavailable */
    }
    return keys;
  }

  async get<T>(key: string): Promise<T | undefined> {
    if (this.name === "data.json") {
      if (key !== "ids") return undefined;
      return (await invoke<string[]>("saved_accounts")) as T;
    }
    try {
      const raw = localStorage.getItem(this.prefix() + key);
      return raw === null ? undefined : (JSON.parse(raw) as T);
    } catch {
      return undefined;
    }
  }

  async set(key: string, value: unknown): Promise<void> {
    if (this.name === "data.json") return; // managed by the server
    try {
      localStorage.setItem(this.prefix() + key, JSON.stringify(value));
    } catch {
      /* storage unavailable */
    }
  }

  async save(): Promise<void> {}
}

export async function load(name: string): Promise<Store> {
  return new Store(name);
}
