import type { DashboardStatus, RegistryGateway, StatusWorker } from "../types";
import { COPY, type Language } from "../i18n";

type Props = {
  status: DashboardStatus | null;
  selectedPeer: string | null;
  onSelect: (peerId: string | null) => void;
  language: Language;
};

type Pt = { x: number; y: number };

function shortPeer(id: string, n = 10): string {
  if (id.length <= n + 3) return id;
  return `${id.slice(0, n)}…`;
}

function workerPoints(count: number, cx: number, cy: number, r: number): Pt[] {
  if (count <= 0) return [];
  if (count === 1) return [{ x: cx, y: cy + r }];
  const start = Math.PI * 0.18;
  const end = Math.PI * 0.82;
  return Array.from({ length: count }, (_, i) => {
    const t = count === 1 ? 0.5 : i / (count - 1);
    const a = start + (end - start) * t;
    return { x: cx + Math.cos(a) * r, y: cy + Math.sin(a) * r * 0.92 };
  });
}

function gatewayPoints(count: number, width: number): Pt[] {
  if (count <= 0) return [];
  const gap = width / (count + 1);
  return Array.from({ length: count }, (_, i) => ({ x: gap * (i + 1), y: 102 }));
}

export function Topology({ status, selectedPeer, onSelect, language }: Props) {
  const copy = COPY[language];
  const W = 760;
  const H = 460;
  const gateways = status?.gateways ?? [];
  const gatewayPts = gatewayPoints(gateways.length, W);
  const workers = status?.workers ?? [];
  const pts = workerPoints(workers.length, W / 2, 210, 178);

  return (
    <div className="topo-shell" aria-label={copy.registryLabel}>
      <svg viewBox={`0 0 ${W} ${H}`} role="img">
        <defs>
          <filter id="hard" x="-20%" y="-20%" width="140%" height="140%">
            <feDropShadow dx="0" dy="0" stdDeviation="2" floodColor="#37d5ff" floodOpacity="0.12" />
          </filter>
        </defs>

        {workers.map((w, i) => {
          const p = pts[i];
          const gateway = gateways.length ? gatewayPts[i % gateways.length] : null;
          if (!p || !gateway) return null;
          return (
            <path
              key={`link-${w.peer_id}`}
              className={`link ${w.connected ? "on" : "off"}`}
              d={`M ${gateway.x} ${gateway.y + 24} C ${gateway.x} ${(gateway.y + p.y) / 2 + 22}, ${p.x} ${(gateway.y + p.y) / 2 - 8}, ${p.x} ${p.y - 26}`}
            />
          );
        })}

        {gateways.map((gateway, i) => (
          <GatewayNode
            key={gateway.peer_id}
            gateway={gateway}
            point={gatewayPts[i]!}
            selected={selectedPeer === gateway.peer_id}
            onSelect={() => onSelect(gateway.peer_id)}
          />
        ))}

        {workers.map((w, i) => (
          <WorkerNode
            key={w.peer_id}
            worker={w}
            point={pts[i]!}
            selected={selectedPeer === w.peer_id}
            onSelect={() => onSelect(w.peer_id)}
            index={i}
          />
        ))}
      </svg>

      {!status && (
        <div className="topo-empty">{language === "zh" ? "等待注册表状态" : "WAITING FOR REGISTRY STATUS"}</div>
      )}
    </div>
  );
}

function GatewayNode({
  gateway,
  point,
  selected,
  onSelect,
}: {
  gateway: RegistryGateway;
  point: Pt;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <g
      className={`node-btn gateway-node ${selected ? "selected" : ""}`}
      onClick={onSelect}
      transform={`translate(${point.x}, ${point.y})`}
    >
      <rect className="node-ring" x="-54" y="-28" width="108" height="56" rx="2" />
      <rect className="node-core" x="-45" y="-19" width="90" height="38" rx="2" filter="url(#hard)" />
      <text className="node-label gw" textAnchor="middle" y="-2">
        {gateway.gateway_id}
      </text>
      <text className="node-sub" textAnchor="middle" y="13">
        {shortPeer(gateway.peer_id, 12)}
      </text>
      <text className="node-sub dim" textAnchor="middle" y="45">
        {gateway.connected_workers}/{gateway.capacity}
      </text>
    </g>
  );
}

function WorkerNode({
  worker,
  point,
  selected,
  onSelect,
  index,
}: {
  worker: StatusWorker;
  point: Pt;
  selected: boolean;
  onSelect: () => void;
  index: number;
}) {
  return (
    <g
      className={`node-btn worker-node ${worker.connected ? "online" : "offline"} ${selected ? "selected" : ""}`}
      onClick={onSelect}
      transform={`translate(${point.x}, ${point.y})`}
    >
      <circle className="node-ring" r="27" />
      <circle className="node-core" r="21" filter="url(#hard)" />
      <circle className="status-pin" r="4.5" />
      <text className="node-label" textAnchor="middle" y="42">
        Worker {index + 1}
      </text>
      <text className="node-sub" textAnchor="middle" y="56">
        {shortPeer(worker.peer_id, 12)}
      </text>
    </g>
  );
}
