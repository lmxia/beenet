import type { DashboardStatus } from "./types";

export type JoinTokenView = {
  id: string;
  description: string;
  created_at_unix_ms: number;
  expires_at_unix_ms: number;
  expired: boolean;
};

export type CreatedJoinTokenView = JoinTokenView & {
  token_value: string;
};

export type RegistrationView = {
  peer_id: string;
  registered_at_unix_ms: number;
  name?: string;
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

async function createTokenAt(
  path: string,
  token: string,
  description: string,
  ttl_secs?: number | null,
) {
  const res = await fetch(`${REGISTRY_BASE.replace(/\/$/, "")}${path}`, {
    method: "POST",
    headers: { ...tokenHeaders(token), "Content-Type": "application/json" },
    body: JSON.stringify({ description, ttl_secs }),
  });
  if (res.status === 401) throw new AuthError();
  if (!res.ok) throw new Error(`Registry HTTP ${res.status}`);
  return (await res.json()) as CreatedJoinTokenView;
}

async function deleteTokenAt(path: string, token: string, id: string) {
  const res = await fetch(`${REGISTRY_BASE.replace(/\/$/, "")}${path}/${encodeURIComponent(id)}`, {
    method: "DELETE",
    headers: tokenHeaders(token),
  });
  if (res.status === 401) throw new AuthError();
  if (!res.ok) throw new Error(`Registry HTTP ${res.status}`);
  return (await res.json()) as { deleted: boolean };
}

export async function createJoinToken(token: string, description: string, ttl_secs?: number | null) {
  return createTokenAt("/v1/admin/tokens", token, description, ttl_secs);
}

export async function listJoinTokens(token: string) {
  return requestJson<{ tokens: JoinTokenView[] }>("/v1/admin/tokens", token);
}

export async function deleteJoinToken(token: string, id: string) {
  return deleteTokenAt("/v1/admin/tokens", token, id);
}

export async function createGatewayJoinToken(token: string, description: string, ttl_secs?: number | null) {
  return createTokenAt("/v1/admin/gateway-tokens", token, description, ttl_secs);
}

export async function listGatewayJoinTokens(token: string) {
  return requestJson<{ tokens: JoinTokenView[] }>("/v1/admin/gateway-tokens", token);
}

export async function deleteGatewayJoinToken(token: string, id: string) {
  return deleteTokenAt("/v1/admin/gateway-tokens", token, id);
}

export async function listRegistrations(token: string) {
  return requestJson<{ registrations: RegistrationView[] }>("/v1/admin/registrations", token);
}

export async function deleteRegistration(token: string, peerId: string) {
  const res = await fetch(
    `${REGISTRY_BASE.replace(/\/$/, "")}/v1/admin/registrations/${encodeURIComponent(peerId)}`,
    {
      method: "DELETE",
      headers: tokenHeaders(token),
    },
  );
  if (res.status === 401) throw new AuthError();
  if (!res.ok) throw new Error(`Registry HTTP ${res.status}`);
  return (await res.json()) as { deleted: boolean };
}

export type GatewayRegistrationView = {
  peer_id: string;
  gateway_id: string;
  registered_at_unix_ms: number;
  region?: string;
};

export async function listGatewayRegistrations(token: string) {
  return requestJson<{ registrations: GatewayRegistrationView[] }>("/v1/admin/gateway-registrations", token);
}

export async function deleteGatewayRegistration(token: string, peerId: string) {
  const res = await fetch(
    `${REGISTRY_BASE.replace(/\/$/, "")}/v1/admin/gateway-registrations/${encodeURIComponent(peerId)}`,
    {
      method: "DELETE",
      headers: tokenHeaders(token),
    },
  );
  if (res.status === 401) throw new AuthError();
  if (!res.ok) throw new Error(`Registry HTTP ${res.status}`);
  return (await res.json()) as { deleted: boolean };
}
