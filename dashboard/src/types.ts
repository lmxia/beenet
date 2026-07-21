export type StatusWorker = {
  peer_id: string;
  connected: boolean;
  last_seen_unix_ms: number;
  name?: string;
  supported_cids: string[];
  loaded_cids: string[];
};

export type RegistryGateway = {
  gateway_id: string;
  peer_id: string;
  dial_addr: string;
  region?: string | null;
  capacity: number;
  connected_workers: number;
  last_seen_unix_ms: number;
};

export type DashboardStatus = {
  gateways: RegistryGateway[];
  workers: StatusWorker[];
  gateway_count: number;
  worker_count: number;
  generated_at_unix_ms: number;
};
