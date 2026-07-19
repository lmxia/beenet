import { useEffect, useMemo, useState } from "react";
import { fetchStatus } from "./api";
import { Topology } from "./components/Topology";
import { SidePanel } from "./components/SidePanel";
import {
  COPY,
  detectLanguage,
  formatTime,
  LANGUAGE_LABEL,
  LANGUAGE_STORAGE_KEY,
  type Language,
} from "./i18n";
import type { DashboardStatus } from "./types";

const POLL_MS = 3000;

export default function App() {
  const [language, setLanguage] = useState<Language>(() => detectLanguage());
  const [status, setStatus] = useState<DashboardStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState<string>("—");
  const [selectedPeer, setSelectedPeer] = useState<string | null>(null);

  const copy = useMemo(() => COPY[language], [language]);

  useEffect(() => {
    window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
    document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
  }, [language]);

  useEffect(() => {
    let cancelled = false;
    const ctrl = { current: null as AbortController | null };

    const tick = async () => {
      ctrl.current?.abort();
      const ac = new AbortController();
      ctrl.current = ac;
      try {
        const data = await fetchStatus(ac.signal);
        if (cancelled) return;
        setStatus(data);
        setError(null);
        setUpdatedAt(formatTime(language, Date.now()));
        setSelectedPeer((prev) => {
          if (!prev) return prev;
          if (data.workers.some((w) => w.peer_id === prev)) return prev;
          return data.gateways.some((g) => g.peer_id === prev) ? prev : null;
        });
      } catch (e) {
        if (cancelled || (e instanceof DOMException && e.name === "AbortError")) return;
        setError(e instanceof Error ? e.message : String(e));
      }
    };

    void tick();
    const id = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      cancelled = true;
      ctrl.current?.abort();
      window.clearInterval(id);
    };
  }, [language]);

  const online = status?.workers.filter((w) => w.connected).length ?? 0;
  const gateways = status?.gateways ?? [];
  const registryState = error ? "DEGRADED" : status ? "ONLINE" : "SYNCING";
  const snapshotAt = status?.generated_at_unix_ms
    ? formatTime(language, status.generated_at_unix_ms)
    : "—";

  return (
    <div className="app">
      <header className="console-head">
        <div className="head-brand">
          <div>
            <div className="eyebrow">{copy.eyebrow}</div>
            <h1 className="brand">{copy.brand}</h1>
            <div className="subline">
              <span>{copy.sublinePrefix}</span>
              <span className="slash">/</span>
              <span className="mono">{status ? copy.gatewayCountSuffix(status.gateway_count) : "—"}</span>
            </div>
          </div>
          <button
            type="button"
            className="lang-switch"
            onClick={() => setLanguage((prev) => (prev === "zh" ? "en" : "zh"))}
            aria-label={copy.toggle}
          >
            {LANGUAGE_LABEL[language]}
          </button>
        </div>
        <div className="meta-grid">
          <div className="stat">
            <span className="label">{copy.registryLabel}</span>
            <span className={`value state ${error ? "bad" : "ok"}`}>{registryState}</span>
          </div>
          <div className="stat">
            <span className="label">{copy.gatewaysLabel}</span>
            <span className="value">{status?.gateway_count ?? 0}</span>
          </div>
          <div className="stat">
            <span className="label">{copy.workersLabel}</span>
            <span className="value">{online}/{status?.worker_count ?? 0}</span>
          </div>
          <div className="stat">
            <span className="label">{copy.healthyGatewaysLabel}</span>
            <span className="value">{gateways.length}</span>
          </div>
          <div className="stat">
            <span className="label">{copy.snapshotLabel}</span>
            <span className="value mono">{snapshotAt}</span>
          </div>
          <div className="stat">
            <span className="label">{copy.lastSyncLabel}</span>
            <span className="value mono">{updatedAt}</span>
          </div>
        </div>
      </header>

      <div className="layout">
        <Topology
          status={status}
          selectedPeer={selectedPeer}
          onSelect={setSelectedPeer}
          language={language}
        />
        <SidePanel
          status={status}
          selectedPeer={selectedPeer}
          onSelect={setSelectedPeer}
          language={language}
        />
      </div>

      {error && <p className="err">{copy.statusFetchPrefix}{error}</p>}
      <p className="footer">{copy.registryProxy} · poll {POLL_MS / 1000}s · source /v1/dashboard/status</p>
    </div>
  );
}
