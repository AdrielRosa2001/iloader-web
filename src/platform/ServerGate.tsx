// Asks for the server access token before the app is mounted, keeps the event
// WebSocket connected and shows when the connection to the server is lost.
import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch, UNAUTHORIZED_EVENT } from "./http";
import { connectEvents, disconnectEvents, onConnectionStatus } from "./event";
import "./ServerGate.css";

type GateState = "checking" | "login" | "ready";

export const ServerGate: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const { t } = useTranslation();
  const [state, setState] = useState<GateState>("checking");
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [conn, setConn] = useState<string>("disconnected");

  const check = useCallback(async () => {
    try {
      const resp = await apiFetch("/api/auth/session");
      setState(resp.ok ? "ready" : "login");
    } catch {
      setError(t("web.server_unreachable", { defaultValue: "Unable to reach the server." }));
      setState("login");
    }
  }, [t]);

  useEffect(() => {
    check();
  }, [check]);

  useEffect(() => {
    const onUnauthorized = () => {
      disconnectEvents();
      setState("login");
    };
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
  }, []);

  useEffect(() => {
    if (state !== "ready") return;
    connectEvents();
    return onConnectionStatus(setConn);
  }, [state]);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!token.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const resp = await apiFetch("/api/auth/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ token: token.trim() }),
      });
      if (resp.ok) {
        setToken("");
        setState("ready");
      } else if (resp.status === 429) {
        setError(t("web.too_many_attempts", { defaultValue: "Too many failed attempts. Try again in a few minutes." }));
      } else {
        setError(t("web.invalid_token", { defaultValue: "Invalid access token." }));
      }
    } catch {
      setError(t("web.server_unreachable", { defaultValue: "Unable to reach the server." }));
    } finally {
      setBusy(false);
    }
  };

  if (state === "checking") return null;

  if (state === "login") {
    return (
      <main className="server-gate">
        <form className="server-gate-card" onSubmit={submit}>
          <h2>{t("web.server_login_title", { defaultValue: "Connect to the server" })}</h2>
          <p className="server-gate-hint">
            {t("web.server_login_hint", {
              defaultValue: "Enter the access token of this iloader server (the access_token file in its data directory).",
            })}
          </p>
          <input
            type="password"
            autoFocus
            autoComplete="current-password"
            placeholder={t("web.token_placeholder", { defaultValue: "Access token" })}
            value={token}
            onChange={(e) => setToken(e.target.value)}
          />
          {error && <p className="server-gate-error">{error}</p>}
          <button type="submit" disabled={busy || !token.trim()}>
            {t("web.connect", { defaultValue: "Connect" })}
          </button>
        </form>
      </main>
    );
  }

  return (
    <>
      {children}
      {conn !== "connected" && (
        <div className="server-connection-banner" role="status">
          {conn === "connecting"
            ? t("web.connecting", { defaultValue: "Connecting to the server…" })
            : t("web.disconnected", { defaultValue: "Disconnected from the server. Reconnecting…" })}
        </div>
      )}
      <button
        className="server-logout"
        title={t("web.logout", { defaultValue: "Disconnect from server" })}
        onClick={async () => {
          await apiFetch("/api/auth/logout", { method: "POST" }).catch(() => {});
          disconnectEvents();
          setState("login");
        }}
      >
        {t("web.logout", { defaultValue: "Disconnect from server" })}
      </button>
    </>
  );
};
