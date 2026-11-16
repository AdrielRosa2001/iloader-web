// Replacement for "@tauri-apps/api/event": backend events arrive over one
// WebSocket (/api/events); emit() becomes POST /api/emit/:event.
import { apiFetch, errorFrom } from "./http";


export type Event<T> = { event: string; id: number; payload: T };
export type EventCallback<T> = (event: Event<T>) => void;
export type UnlistenFn = () => void;

type Status = "connecting" | "connected" | "disconnected";

const listeners = new Map<string, Set<EventCallback<any>>>();
const statusListeners = new Set<(s: Status) => void>();
let socket: WebSocket | null = null;
let status: Status = "disconnected";
let wanted = false;
let retryDelay = 1000;
let retryTimer: number | undefined;
let nextId = 0;

function setStatus(s: Status) {
  status = s;
  statusListeners.forEach((l) => l(s));
}

export function onConnectionStatus(cb: (s: Status) => void): () => void {
  statusListeners.add(cb);
  cb(status);
  return () => statusListeners.delete(cb);
}

function dispatch(event: string, payload: unknown) {
  const set = listeners.get(event);
  if (!set) return;
  const ev = { event, id: nextId++, payload };
  for (const cb of [...set]) {
    try {
      cb(ev);
    } catch (e) {
      console.error(`listener for ${event} failed`, e);
    }
  }
}

function open() {
  if (!wanted || socket) return;
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}/api/events`);
  socket = ws;
  setStatus("connecting");
  let opened = false;
  ws.onopen = () => {
    opened = true;
    retryDelay = 1000;
    setStatus("connected");
  };
  ws.onmessage = (msg) => {
    try {
      const data = JSON.parse(msg.data);
      dispatch(data.event, data.payload);
    } catch (e) {
      console.error("bad event message", e);
    }
  };
  ws.onclose = () => {
    socket = null;
    setStatus("disconnected");
    if (!wanted) return;
    // A refused handshake may mean the session expired (e.g. server restart):
    // probing the session sends the user back to the token screen on 401.
    if (!opened) apiFetch("/api/auth/session").catch(() => {});
    retryTimer = window.setTimeout(open, retryDelay);
    retryDelay = Math.min(retryDelay * 2, 15000);
  };
}

/** Starts (and keeps re-establishing) the event connection. */
export function connectEvents() {
  wanted = true;
  open();
}

export function disconnectEvents() {
  wanted = false;
  window.clearTimeout(retryTimer);
  socket?.close();
  socket = null;
  setStatus("disconnected");
}

export async function listen<T>(
  event: string,
  handler: EventCallback<T>,
): Promise<UnlistenFn> {
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  set.add(handler);
  return () => {
    set!.delete(handler);
  };
}

export async function emit(event: string, payload?: unknown): Promise<void> {
  const resp = await apiFetch(`/api/emit/${encodeURIComponent(event)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload ?? null),
  });
  if (!resp.ok) throw await errorFrom(resp);
}
