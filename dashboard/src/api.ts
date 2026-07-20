import type { DashboardStatus } from "./types";

export type JoinTokenView = {
  id: string;
  description: string;
  token_value: string;
  created_at_unix_ms: number;
  expires_at_unix_ms: number | null;
};

export type RegistrationView = {
  peer_id: string;
  registered_at_unix_ms: number;
  supported_cids: string[];
  loaded_cids: string[];
};

/** In cluster/local nginx, API is proxied under /registry. */
export const REGISTRY_BASE =
  (import.meta.env.VITE_REGISTRY_BASE as string | undefined) || "/registry";

export class AuthError extends Error {
  constructor(message = "Unauthorized") {
    super(message);
    this.name = "AuthError";
  }
}

function tokenHeaders(token: string) {
  return {
    Accept: "application/json",
    Authorization: `Bearer ${token}`,
  };
}

async function requestJson<T>(path: string, token: string): Promise<T> {
  const res = await fetch(`${REGISTRY_BASE.replace(/\/$/, "")}${path}`, {
    headers: tokenHeaders(token),
  });
  if (res.status === 401) {
    throw new AuthError();
  }
  if (!res.ok) {
    throw new Error(`Registry HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

export async function fetchStatus(token: string): Promise<DashboardStatus> {
  return requestJson<DashboardStatus>("/v1/dashboard/status", token);
}

export async function createJoinToken(token: string, description: string, ttl_secs?: number | null) {
  const res = await fetch(`${REGISTRY_BASE.replace(/\/$/, "")}/v1/admin/tokens`, {
    method: "POST",
    headers: { ...tokenHeaders(token), "Content-Type": "application/json" },
    body: JSON.stringify({ description, ttl_secs }),
  });
  if (res.status === 401) throw new AuthError();
  if (!res.ok) throw new Error(`Registry HTTP ${res.status}`);
  return (await res.json()) as JoinTokenView;
}

export async function listJoinTokens(token: string) {
  return requestJson<{ tokens: JoinTokenView[] }>("/v1/admin/tokens", token);
}

export async function listRegistrations(token: string) {
  return requestJson<{ registrations: RegistrationView[] }>("/v1/admin/registrations", token);
}
