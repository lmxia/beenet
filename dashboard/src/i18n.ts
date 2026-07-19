export type Language = "zh" | "en";

export const LANGUAGE_STORAGE_KEY = "beenet-dashboard-language";

export const LANGUAGE_LABEL: Record<Language, string> = {
  zh: "中文",
  en: "EN",
};

export const COPY = {
  zh: {
    eyebrow: "BEENET 注册表快照",
    brand: "registry",
    sublinePrefix: "多网关",
    registryLabel: "注册表",
    gatewaysLabel: "网关数",
    workersLabel: "工作节点",
    healthyGatewaysLabel: "在线网关",
    snapshotLabel: "快照时间",
    lastSyncLabel: "同步时间",
    registryStates: {
      DEGRADED: "降级",
      ONLINE: "在线",
      SYNCING: "同步中",
    },
    footer: "注册表代理 · 每 {poll}s 拉取 · 来源 /v1/dashboard/status",
    statusFetchFailed: "状态获取失败",
    gatewayLeases: "网关租约",
    workers: "工作节点",
    detail: "详情",
    noActiveLease: "暂无活跃租约",
    noWorkerLease: "暂无工作节点租约",
    noSelection: "未选择对象",
    role: "角色",
    gatewayId: "网关 ID",
    peerId: "Peer ID",
    dialAddress: "拨号地址",
    region: "区域",
    load: "负载",
    lastLease: "最近续租",
    workerStatus: "状态",
    lastSeen: "最近在线",
    supportedCids: "支持的 CID",
    loadedCids: "已加载 CID（热）",
    gatewayRole: "网关",
    workerRole: "工作节点",
    online: "在线",
    offline: "离线",
    workerLabel: (index: number) => `工作节点 ${index + 1}`,
    toggle: "EN / 中文",
    registryProxy: "注册表代理",
    statusFetchPrefix: "状态获取失败：",
    healthyGatewayCount: (count: number) => `在线网关 ${count}`,
    gatewayCountSuffix: (count: number) => `${count} 个网关`,
  },
  en: {
    eyebrow: "BEENET REGISTRY SNAPSHOT",
    brand: "registry",
    sublinePrefix: "multi-gateway",
    registryLabel: "Registry",
    gatewaysLabel: "Gateways",
    workersLabel: "Workers",
    healthyGatewaysLabel: "Healthy Gateways",
    snapshotLabel: "Snapshot",
    lastSyncLabel: "Last Sync",
    registryStates: {
      DEGRADED: "DEGRADED",
      ONLINE: "ONLINE",
      SYNCING: "SYNCING",
    },
    footer: "Registry proxy · poll {poll}s · source /v1/dashboard/status",
    statusFetchFailed: "Status fetch failed",
    gatewayLeases: "Gateway Leases",
    workers: "Workers",
    detail: "Detail",
    noActiveLease: "NO ACTIVE LEASE",
    noWorkerLease: "NO WORKER LEASE",
    noSelection: "NO SELECTION",
    role: "Role",
    gatewayId: "Gateway ID",
    peerId: "Peer ID",
    dialAddress: "Dial address",
    region: "Region",
    load: "Load",
    lastLease: "Last lease",
    workerStatus: "Status",
    lastSeen: "Last seen",
    supportedCids: "supported_cids",
    loadedCids: "loaded_cids (hot)",
    gatewayRole: "Gateway",
    workerRole: "Worker",
    online: "online",
    offline: "offline",
    workerLabel: (index: number) => `Worker ${index + 1}`,
    toggle: "中文 / EN",
    registryProxy: "Registry proxy",
    statusFetchPrefix: "STATUS FETCH FAILED: ",
    healthyGatewayCount: (count: number) => `${count} healthy`,
    gatewayCountSuffix: (count: number) => `${count} gateways`,
  },
} as const;

export function detectLanguage(): Language {
  const saved = typeof window !== "undefined"
    ? window.localStorage.getItem(LANGUAGE_STORAGE_KEY)
    : null;
  if (saved === "zh" || saved === "en") return saved;
  const nav = typeof navigator !== "undefined" ? navigator.language.toLowerCase() : "";
  return nav.startsWith("zh") ? "zh" : "en";
}

export function formatTime(language: Language, value: number | string): string {
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  return new Intl.DateTimeFormat(language === "zh" ? "zh-CN" : "en-US", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
}

export function formatDateTime(language: Language, value: number): string {
  return new Intl.DateTimeFormat(language === "zh" ? "zh-CN" : "en-US", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(value));
}
