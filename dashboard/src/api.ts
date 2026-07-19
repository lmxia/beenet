import type { DashboardStatus } from "./types";

/** In cluster/local nginx, API is proxied under /registry. */
export const REGISTRY_BASE =
  (import.meta.env.VITE_REGISTRY_BASE as string | undefined) || "/registry";

export async function fetchStatus(signal?: AbortSignal): Promise<DashboardStatus> {
  const res = await fetch(
    `${REGISTRY_BASE.replace(/\/$/, "")}/v1/dashboard/status`,
    {
      signal,
      headers: { Accept: "application/json" },
    },
  );
  if (!res.ok) {
    throw new Error(`Registry status HTTP ${res.status}`);
  }
  return (await res.json()) as DashboardStatus;
}
