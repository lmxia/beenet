import { useEffect, useMemo, useState } from "react";
import {
  AuthError,
  createGatewayJoinToken,
  createJoinToken,
  deleteGatewayJoinToken,
  deleteGatewayRegistration,
  deleteJoinToken,
  deleteRegistration,
  fetchStatus,
  listGatewayJoinTokens,
  listGatewayRegistrations,
  listJoinTokens,
  listRegistrations,
  type CreatedJoinTokenView,
  type GatewayRegistrationView,
  type JoinTokenView,
  type RegistrationView,
} from "./api";
import { Topology } from "./components/Topology";
import { SidePanel } from "./components/SidePanel";
import { COPY, detectLanguage, formatDateTime, formatTime, LANGUAGE_LABEL, LANGUAGE_STORAGE_KEY, type Language } from "./i18n";
import type { DashboardStatus } from "./types";

const POLL_MS = 3000;
const ADMIN_TOKEN_KEY = "beenet-dashboard-admin-token";

function shortPeer(id: string, n = 14): string {
  if (id.length <= n + 3) return id;
  return `${id.slice(0, n)}…`;
}

export default function App() {
  const [language, setLanguage] = useState<Language>(() => detectLanguage());
  const [adminToken, setAdminToken] = useState<string>(() => window.localStorage.getItem(ADMIN_TOKEN_KEY) ?? "");
  const [tokenInput, setTokenInput] = useState(adminToken);
  const [authed, setAuthed] = useState<boolean>(() => Boolean(adminToken));
  const [status, setStatus] = useState<DashboardStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState<string>("—");
  const [selectedPeer, setSelectedPeer] = useState<string | null>(null);
  const [joinTokensCount, setJoinTokensCount] = useState<number | null>(null);
  const [joinTokens, setJoinTokens] = useState<JoinTokenView[]>([]);
  const [createdJoinToken, setCreatedJoinToken] = useState<CreatedJoinTokenView | null>(null);
  const [gatewayTokensCount, setGatewayTokensCount] = useState<number | null>(null);
  const [gatewayTokens, setGatewayTokens] = useState<JoinTokenView[]>([]);
  const [createdGatewayToken, setCreatedGatewayToken] = useState<CreatedJoinTokenView | null>(null);
  const [registrations, setRegistrations] = useState<RegistrationView[]>([]);
  const [registrationsCount, setRegistrationsCount] = useState<number | null>(null);
  const [gatewayRegistrations, setGatewayRegistrations] = useState<GatewayRegistrationView[]>([]);
  const [gatewayRegistrationsCount, setGatewayRegistrationsCount] = useState<number | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [loginError, setLoginError] = useState<string | null>(null);
  const [adminError, setAdminError] = useState<string | null>(null);

  const copy = useMemo(() => COPY[language], [language]);
  const onlineWorkerPeerIds = useMemo(
    () => new Set((status?.workers ?? []).map((w) => w.peer_id)),
    [status],
  );
  const onlineGatewayPeerIds = useMemo(
    () => new Set((status?.gateways ?? []).map((g) => g.peer_id)),
    [status],
  );
  const offlineRegs = useMemo(
    () => registrations.filter((r) => !onlineWorkerPeerIds.has(r.peer_id)),
    [registrations, onlineWorkerPeerIds],
  );
  const offlineGatewayRegs = useMemo(
    () => gatewayRegistrations.filter((r) => !onlineGatewayPeerIds.has(r.peer_id)),
    [gatewayRegistrations, onlineGatewayPeerIds],
  );

  useEffect(() => {
    window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
    document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
  }, [language]);

  useEffect(() => {
    if (adminToken) window.localStorage.setItem(ADMIN_TOKEN_KEY, adminToken);
    else window.localStorage.removeItem(ADMIN_TOKEN_KEY);
  }, [adminToken]);

  useEffect(() => {
    if (!authed || !adminToken) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const data = await fetchStatus(adminToken);
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
        if (cancelled) return;
        if (e instanceof AuthError) {
          setAuthed(false);
          setError(null);
          return;
        }
        setError(e instanceof Error ? e.message : String(e));
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [adminToken, authed, language]);

  const refreshAdminLists = async () => {
    const [tokens, gatewayToks, regs, gatewayRegs] = await Promise.all([
      listJoinTokens(adminToken),
      listGatewayJoinTokens(adminToken),
      listRegistrations(adminToken),
      listGatewayRegistrations(adminToken),
    ]);
    setJoinTokens(tokens.tokens);
    setJoinTokensCount(tokens.tokens.length);
    setGatewayTokens(gatewayToks.tokens);
    setGatewayTokensCount(gatewayToks.tokens.length);
    setRegistrations(regs.registrations);
    setRegistrationsCount(regs.registrations.length);
    setGatewayRegistrations(gatewayRegs.registrations);
    setGatewayRegistrationsCount(gatewayRegs.registrations.length);
  };

  useEffect(() => {
    if (!authed || !adminToken) return;
    void (async () => {
      try {
        await refreshAdminLists();
      } catch {
        setJoinTokens([]);
        setJoinTokensCount(null);
        setGatewayTokens([]);
        setGatewayTokensCount(null);
        setRegistrations([]);
        setRegistrationsCount(null);
        setGatewayRegistrations([]);
        setGatewayRegistrationsCount(null);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- refresh only on auth change
  }, [adminToken, authed]);

  const login = async () => {
    const nextToken = tokenInput.trim();
    if (!nextToken) {
      setLoginError(copy.enterAdminToken);
      return;
    }
    setBusy("login");
    setLoginError(null);
    try {
      await fetchStatus(nextToken);
      setAdminToken(nextToken);
      setAuthed(true);
      setError(null);
    } catch (e) {
      setLoginError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const logout = () => {
    setAdminToken("");
    setTokenInput("");
    setAuthed(false);
    setStatus(null);
    setError(null);
    setJoinTokensCount(null);
    setJoinTokens([]);
    setCreatedJoinToken(null);
    setGatewayTokensCount(null);
    setGatewayTokens([]);
    setCreatedGatewayToken(null);
    setRegistrations([]);
    setRegistrationsCount(null);
    setGatewayRegistrations([]);
    setGatewayRegistrationsCount(null);
    setAdminError(null);
  };

  const createBootstrapToken = async () => {
    setBusy("create-token");
    setAdminError(null);
    try {
      const token = await createJoinToken(adminToken, "dashboard-worker", 600);
      setCreatedJoinToken(token);
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const createGatewayBootstrapToken = async () => {
    setBusy("create-gateway-token");
    setAdminError(null);
    try {
      const token = await createGatewayJoinToken(adminToken, "dashboard-gateway", 600);
      setCreatedGatewayToken(token);
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const revokeBootstrapToken = async (id: string) => {
    setBusy(`revoke-${id}`);
    setAdminError(null);
    try {
      await deleteJoinToken(adminToken, id);
      if (createdJoinToken?.id === id) setCreatedJoinToken(null);
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const revokeGatewayBootstrapToken = async (id: string) => {
    setBusy(`revoke-gw-${id}`);
    setAdminError(null);
    try {
      await deleteGatewayJoinToken(adminToken, id);
      if (createdGatewayToken?.id === id) setCreatedGatewayToken(null);
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const revokeRegistration = async (peerId: string) => {
    if (onlineWorkerPeerIds.has(peerId)) return;
    setBusy(`del-reg-${peerId}`);
    setAdminError(null);
    try {
      await deleteRegistration(adminToken, peerId);
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const pruneOfflineRegistrations = async () => {
    if (offlineRegs.length === 0) return;
    setBusy("prune-offline");
    setAdminError(null);
    try {
      for (const reg of offlineRegs) {
        await deleteRegistration(adminToken, reg.peer_id);
      }
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const revokeGatewayRegistration = async (peerId: string) => {
    if (onlineGatewayPeerIds.has(peerId)) return;
    setBusy(`del-gw-reg-${peerId}`);
    setAdminError(null);
    try {
      await deleteGatewayRegistration(adminToken, peerId);
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const pruneOfflineGatewayRegistrations = async () => {
    if (offlineGatewayRegs.length === 0) return;
    setBusy("prune-gw-offline");
    setAdminError(null);
    try {
      for (const reg of offlineGatewayRegs) {
        await deleteGatewayRegistration(adminToken, reg.peer_id);
      }
      await refreshAdminLists();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const copyCreatedToken = async (value: string) => {
    try {
      await navigator.clipboard.writeText(value);
      setAdminError(null);
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    }
  };

  const online = status?.workers.filter((w) => w.connected).length ?? 0;
  const gateways = status?.gateways ?? [];
  const registryStateKey = error ? "DEGRADED" : status ? "ONLINE" : "SYNCING";
  const snapshotAt = status?.generated_at_unix_ms ? formatTime(language, status.generated_at_unix_ms) : "—";

  if (!authed) {
    return (
      <div className="app">
        <section className="login-shell">
          <div className="panel login-panel">
            <h1>{copy.brand}</h1>
            <p>{copy.loginHint}</p>
            <input
              className="token-input"
              type="password"
              autoFocus
              value={tokenInput}
              onChange={(e) => setTokenInput(e.target.value)}
              placeholder={copy.loginPlaceholder}
            />
            <button type="button" className="primary" onClick={() => void login()} disabled={busy === "login"}>
              {busy === "login" ? copy.signingIn : copy.signIn}
            </button>
            {loginError && <p className="err">{loginError}</p>}
          </div>
        </section>
      </div>
    );
  }

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
          <div className="head-actions">
            <button type="button" className="lang-switch" onClick={() => setLanguage((prev) => (prev === "zh" ? "en" : "zh"))} aria-label={copy.toggle}>
              {LANGUAGE_LABEL[language]}
            </button>
            <button type="button" className="lang-switch" onClick={logout}>
              {copy.logout}
            </button>
          </div>
        </div>
        <div className="meta-grid">
          <div className="stat"><span className="label">{copy.registryLabel}</span><span className={`value state ${error ? "bad" : "ok"}`}>{copy.registryStates[registryStateKey]}</span></div>
          <div className="stat"><span className="label">{copy.gatewaysLabel}</span><span className="value">{status?.gateway_count ?? 0}</span></div>
          <div className="stat"><span className="label">{copy.workersLabel}</span><span className="value">{online}/{status?.worker_count ?? 0}</span></div>
          <div className="stat"><span className="label">{copy.healthyGatewaysLabel}</span><span className="value">{gateways.length}</span></div>
          <div className="stat"><span className="label">{copy.snapshotLabel}</span><span className="value mono">{snapshotAt}</span></div>
          <div className="stat"><span className="label">{copy.lastSyncLabel}</span><span className="value mono">{updatedAt}</span></div>
          <div className="stat"><span className="label">{copy.workerTokensLabel}</span><span className="value">{joinTokensCount ?? "—"}</span></div>
          <div className="stat"><span className="label">{copy.gatewayTokensLabel}</span><span className="value">{gatewayTokensCount ?? "—"}</span></div>
          <div className="stat"><span className="label">{copy.workerRegsLabel}</span><span className="value">{registrationsCount ?? "—"}</span></div>
          <div className="stat"><span className="label">{copy.gatewayRegsLabel}</span><span className="value">{gatewayRegistrationsCount ?? "—"}</span></div>
        </div>
      </header>

      <div className="layout">
        <Topology status={status} selectedPeer={selectedPeer} onSelect={setSelectedPeer} language={language} />
        <SidePanel status={status} selectedPeer={selectedPeer} onSelect={setSelectedPeer} language={language} />
      </div>

      {error && <p className="err">{copy.statusFetchPrefix}{error}</p>}
      <section className="panel admin-panel">
        <h2>{copy.adminWorkerTokens}</h2>
        <div className="admin-form">
          <button type="button" className="primary" onClick={() => void createBootstrapToken()} disabled={busy === "create-token"}>
            {busy === "create-token" ? copy.creating : copy.createWorkerToken}
          </button>
        </div>
        {createdJoinToken && (
          <div className="token-created">
            <p>{copy.tokenOnce}</p>
            <div className="token-secret-row">
              <code className="token-secret">{createdJoinToken.token_value}</code>
              <button type="button" className="lang-switch" onClick={() => void copyCreatedToken(createdJoinToken.token_value)}>
                {copy.copy}
              </button>
            </div>
            <p className="token-expiry">{copy.expires}: {formatTime(language, createdJoinToken.expires_at_unix_ms)}</p>
          </div>
        )}
        <div className="token-list">
          {joinTokens.length === 0 && <p className="empty-line">{copy.noWorkerTokens}</p>}
          {joinTokens.map((token) => (
            <div className="token-row" key={token.id}>
              <div>
                <div className="token-description">{token.description || "—"}</div>
                <div className="token-expiry">{formatTime(language, token.expires_at_unix_ms)}</div>
              </div>
              <button type="button" className="lang-switch" onClick={() => void revokeBootstrapToken(token.id)} disabled={busy === `revoke-${token.id}`}>
                {copy.revoke}
              </button>
            </div>
          ))}
        </div>
      </section>
      <section className="panel admin-panel">
        <h2>{copy.adminGatewayTokens}</h2>
        <div className="admin-form">
          <button type="button" className="primary" onClick={() => void createGatewayBootstrapToken()} disabled={busy === "create-gateway-token"}>
            {busy === "create-gateway-token" ? copy.creating : copy.createGatewayToken}
          </button>
        </div>
        {createdGatewayToken && (
          <div className="token-created">
            <p>{copy.tokenOnce}</p>
            <div className="token-secret-row">
              <code className="token-secret">{createdGatewayToken.token_value}</code>
              <button type="button" className="lang-switch" onClick={() => void copyCreatedToken(createdGatewayToken.token_value)}>
                {copy.copy}
              </button>
            </div>
            <p className="token-expiry">{copy.expires}: {formatTime(language, createdGatewayToken.expires_at_unix_ms)}</p>
          </div>
        )}
        <div className="token-list">
          {gatewayTokens.length === 0 && <p className="empty-line">{copy.noGatewayTokens}</p>}
          {gatewayTokens.map((token) => (
            <div className="token-row" key={token.id}>
              <div>
                <div className="token-description">{token.description || "—"}</div>
                <div className="token-expiry">{formatTime(language, token.expires_at_unix_ms)}</div>
              </div>
              <button type="button" className="lang-switch" onClick={() => void revokeGatewayBootstrapToken(token.id)} disabled={busy === `revoke-gw-${token.id}`}>
                {copy.revoke}
              </button>
            </div>
          ))}
        </div>
      </section>
      <section className="panel admin-panel">
        <div className="admin-panel-head">
          <h2>{copy.adminWorkerRegs}</h2>
          <button
            type="button"
            className="lang-switch"
            onClick={() => void pruneOfflineRegistrations()}
            disabled={offlineRegs.length === 0 || busy === "prune-offline"}
          >
            {busy === "prune-offline" ? copy.pruning : `${copy.pruneOffline} (${offlineRegs.length})`}
          </button>
        </div>
        <p className="admin-hint">{copy.offlineOnlyHint}</p>
        <div className="token-list">
          {registrations.length === 0 && <p className="empty-line">{copy.noRegistrations}</p>}
          {registrations.map((reg) => {
            const onlineReg = onlineWorkerPeerIds.has(reg.peer_id);
            return (
              <div className="token-row" key={reg.peer_id}>
                <div>
                  <div className="token-description">
                    <span className={`dot ${onlineReg ? "on" : "off"}`} />
                    {reg.name?.trim() || copy.unnamedWorker}
                    <span className="badge">{onlineReg ? copy.online : copy.offline}</span>
                  </div>
                  <div className="token-expiry mono">{shortPeer(reg.peer_id, 28)}</div>
                  <div className="token-expiry">{copy.registeredAt}: {formatDateTime(language, reg.registered_at_unix_ms)}</div>
                </div>
                <button
                  type="button"
                  className="lang-switch"
                  onClick={() => void revokeRegistration(reg.peer_id)}
                  disabled={onlineReg || busy === `del-reg-${reg.peer_id}`}
                >
                  {busy === `del-reg-${reg.peer_id}` ? copy.deleting : copy.deleteReg}
                </button>
              </div>
            );
          })}
        </div>
      </section>
      <section className="panel admin-panel">
        <div className="admin-panel-head">
          <h2>{copy.adminGatewayRegs}</h2>
          <button
            type="button"
            className="lang-switch"
            onClick={() => void pruneOfflineGatewayRegistrations()}
            disabled={offlineGatewayRegs.length === 0 || busy === "prune-gw-offline"}
          >
            {busy === "prune-gw-offline" ? copy.pruning : `${copy.pruneOffline} (${offlineGatewayRegs.length})`}
          </button>
        </div>
        <p className="admin-hint">{copy.offlineGatewayOnlyHint}</p>
        <div className="token-list">
          {gatewayRegistrations.length === 0 && <p className="empty-line">{copy.noGatewayRegistrations}</p>}
          {gatewayRegistrations.map((reg) => {
            const onlineReg = onlineGatewayPeerIds.has(reg.peer_id);
            return (
              <div className="token-row" key={reg.peer_id}>
                <div>
                  <div className="token-description">
                    <span className={`dot ${onlineReg ? "on" : "off"}`} />
                    {reg.gateway_id}
                    <span className="badge">{onlineReg ? copy.online : copy.offline}</span>
                  </div>
                  <div className="token-expiry mono">{shortPeer(reg.peer_id, 28)}</div>
                  <div className="token-expiry">{copy.registeredAt}: {formatDateTime(language, reg.registered_at_unix_ms)}</div>
                </div>
                <button
                  type="button"
                  className="lang-switch"
                  onClick={() => void revokeGatewayRegistration(reg.peer_id)}
                  disabled={onlineReg || busy === `del-gw-reg-${reg.peer_id}`}
                >
                  {busy === `del-gw-reg-${reg.peer_id}` ? copy.deleting : copy.deleteReg}
                </button>
              </div>
            );
          })}
        </div>
        {adminError && <p className="err">{adminError}</p>}
      </section>
      <p className="footer">{copy.footer.replace("{poll}", String(POLL_MS / 1000))}</p>
    </div>
  );
}
