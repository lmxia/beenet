import type { DashboardStatus, RegistryGateway, StatusWorker } from "../types";
import { COPY, formatDateTime, type Language } from "../i18n";

type Props = {
  status: DashboardStatus | null;
  selectedPeer: string | null;
  onSelect: (peerId: string | null) => void;
  language: Language;
};

function formatSeen(language: Language, ms: number): string {
  if (!ms) return "—";
  try {
    return formatDateTime(language, ms);
  } catch {
    return String(ms);
  }
}

export function SidePanel({ status, selectedPeer, onSelect, language }: Props) {
  const copy = COPY[language];
  const workers = status?.workers ?? [];
  const gateways = status?.gateways ?? [];
  const selected: StatusWorker | undefined = workers.find((w) => w.peer_id === selectedPeer);
  const selectedGateway: RegistryGateway | undefined = gateways.find(
    (g) => g.peer_id === selectedPeer,
  );
  const hasSelection = Boolean(selectedGateway || selected);

  return (
    <aside className="side">
      <section className="panel">
        <h2>{copy.gatewayLeases}</h2>
        <div className="asset-list">
          {gateways.length === 0 && <div className="empty-line">{copy.noActiveLease}</div>}
          {gateways.map((g) => (
            <button
              key={g.peer_id}
              type="button"
              className={`asset-item ${selectedPeer === g.peer_id ? "active" : ""}`}
              onClick={() => onSelect(g.peer_id)}
            >
              <div className="row">
                <span>
                  <span className="dot standby" />
                  {g.gateway_id}
                </span>
                <span className="badge">{g.connected_workers}/{g.capacity}</span>
              </div>
              <div className="peer">{g.dial_addr}</div>
            </button>
          ))}
        </div>
      </section>

      <section className="panel">
        <h2>{copy.workers}</h2>
        <div className="worker-list">
          {workers.length === 0 && <div className="empty-line">{copy.noWorkerLease}</div>}
          {workers.map((w, i) => (
            <button
              key={w.peer_id}
              type="button"
              className={`asset-item ${selectedPeer === w.peer_id ? "active" : ""}`}
              onClick={() => onSelect(w.peer_id)}
            >
              <div className="row">
                <span>
                  <span className={`dot ${w.connected ? "on" : "off"}`} />
                  {copy.workerLabel(w.name, i)}
                </span>
                <span className="badge">{w.connected ? copy.online : copy.offline}</span>
              </div>
              <div className="peer">{w.peer_id}</div>
            </button>
          ))}
        </div>
      </section>

      <section className="panel">
        <h2>{copy.detail}</h2>
        {!selectedPeer && <p className="empty-line">{copy.noSelection}</p>}
        {selectedPeer && !hasSelection && (
          <p className="empty-line">{copy.selectionGone}</p>
        )}
        {selectedGateway && (
          <div className="detail-grid">
            <div className="field">
              <span className="k">{copy.role}</span>
              <span className="v">{copy.gatewayRole}</span>
            </div>
            <div className="field">
              <span className="k">{copy.gatewayId}</span>
              <span className="v">{selectedGateway.gateway_id}</span>
            </div>
            <div className="field">
              <span className="k">{copy.peerId}</span>
              <span className="v mono">{selectedGateway.peer_id}</span>
            </div>
            <div className="field">
              <span className="k">{copy.dialAddress}</span>
              <span className="v mono">{selectedGateway.dial_addr}</span>
            </div>
            <div className="field">
              <span className="k">{copy.region}</span>
              <span className="v">{selectedGateway.region ?? "—"}</span>
            </div>
            <div className="field">
              <span className="k">{copy.load}</span>
              <span className="v">{selectedGateway.connected_workers}/{selectedGateway.capacity}</span>
            </div>
            <div className="field">
              <span className="k">{copy.lastLease}</span>
              <span className="v">{formatSeen(language, selectedGateway.last_seen_unix_ms)}</span>
            </div>
          </div>
        )}
        {selected && (
          <div className="detail-grid">
            <div className="field">
              <span className="k">{copy.role}</span>
              <span className="v">{copy.workerRole}</span>
            </div>
            <div className="field">
              <span className="k">{copy.workerName}</span>
              <span className="v">{selected.name?.trim() || copy.unnamedWorker}</span>
            </div>
            <div className="field">
              <span className="k">{copy.workerStatus}</span>
              <span className="v">
                <span className={`dot ${selected.connected ? "on" : "off"}`} />
                {selected.connected ? copy.online : copy.offline}
              </span>
            </div>
            <div className="field">
              <span className="k">{copy.peerId}</span>
              <span className="v mono">{selected.peer_id}</span>
            </div>
            <div className="field">
              <span className="k">{copy.lastSeen}</span>
              <span className="v">{formatSeen(language, selected.last_seen_unix_ms)}</span>
            </div>
            <div className="field">
              <span className="k">{copy.supportedCids}</span>
              <div className="cid-list">
                {selected.supported_cids.length === 0 && <span className="cid">—</span>}
                {selected.supported_cids.map((c) => (
                  <span className="cid" key={c}>
                    {c}
                  </span>
                ))}
              </div>
            </div>
            <div className="field">
              <span className="k">{copy.loadedCids}</span>
              <div className="cid-list">
                {selected.loaded_cids.length === 0 && <span className="cid">—</span>}
                {selected.loaded_cids.map((c) => (
                  <span className="cid hot" key={c}>
                    {c}
                  </span>
                ))}
              </div>
            </div>
          </div>
        )}
      </section>
    </aside>
  );
}
