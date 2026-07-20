import { useEffect, useMemo, useState } from "react";
import { AuthError, createJoinToken, deleteJoinToken, fetchStatus, listJoinTokens, listRegistrations, type CreatedJoinTokenView, type JoinTokenView } from "./api";
import { Topology } from "./components/Topology";
import { SidePanel } from "./components/SidePanel";
import { COPY, detectLanguage, formatTime, LANGUAGE_LABEL, LANGUAGE_STORAGE_KEY, type Language } from "./i18n";
import type { DashboardStatus } from "./types";

const POLL_MS = 3000;
const ADMIN_TOKEN_KEY = "beenet-dashboard-admin-token";

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
  const [registrationsCount, setRegistrationsCount] = useState<number | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [loginError, setLoginError] = useState<string | null>(null);
  const [adminError, setAdminError] = useState<string | null>(null);

  const copy = useMemo(() => COPY[language], [language]);

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

  useEffect(() => {
    if (!authed || !adminToken) return;
    void (async () => {
      try {
        const [tokens, registrations] = await Promise.all([listJoinTokens(adminToken), listRegistrations(adminToken)]);
        setJoinTokens(tokens.tokens);
        setJoinTokensCount(tokens.tokens.length);
        setRegistrationsCount(registrations.registrations.length);
      } catch {
        setJoinTokens([]);
        setJoinTokensCount(null);
        setRegistrationsCount(null);
      }
    })();
  }, [adminToken, authed]);

  const login = async () => {
    const nextToken = tokenInput.trim();
    if (!nextToken) {
      setLoginError(language === "zh" ? "请输入 admin token" : "Enter the admin token");
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
    setRegistrationsCount(null);
    setAdminError(null);
  };

  const refreshJoinTokens = async () => {
    const response = await listJoinTokens(adminToken);
    setJoinTokens(response.tokens);
    setJoinTokensCount(response.tokens.length);
  };

  const createBootstrapToken = async () => {
    setBusy("create-token");
    setAdminError(null);
    try {
      const token = await createJoinToken(adminToken, "dashboard", 600);
      setCreatedJoinToken(token);
      await refreshJoinTokens();
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
      await refreshJoinTokens();
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const copyCreatedToken = async () => {
    if (!createdJoinToken) return;
    try {
      await navigator.clipboard.writeText(createdJoinToken.token_value);
      setAdminError(null);
    } catch (e) {
      setAdminError(e instanceof Error ? e.message : String(e));
    }
  };

  const online = status?.workers.filter((w) => w.connected).length ?? 0;
  const gateways = status?.gateways ?? [];
  const registryState = error ? "DEGRADED" : status ? "ONLINE" : "SYNCING";
  const snapshotAt = status?.generated_at_unix_ms ? formatTime(language, status.generated_at_unix_ms) : "—";

  if (!authed) {
    return (
      <div className="app">
        <section className="login-shell">
          <div className="panel login-panel">
            <h1>{copy.brand}</h1>
            <p>{language === "zh" ? "请输入管理员 token 进入控制台" : "Enter the admin token to access the console"}</p>
            <input className="token-input" type="password" autoFocus value={tokenInput} onChange={(e) => setTokenInput(e.target.value)} placeholder="Bearer token" />
            <button type="button" className="primary" onClick={() => void login()} disabled={busy === "login"}>
              {busy === "login" ? (language === "zh" ? "登录中…" : "Signing in…") : (language === "zh" ? "登录" : "Sign in")}
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
              {language === "zh" ? "退出" : "Logout"}
            </button>
          </div>
        </div>
        <div className="meta-grid">
          <div className="stat"><span className="label">{copy.registryLabel}</span><span className={`value state ${error ? "bad" : "ok"}`}>{registryState}</span></div>
          <div className="stat"><span className="label">{copy.gatewaysLabel}</span><span className="value">{status?.gateway_count ?? 0}</span></div>
          <div className="stat"><span className="label">{copy.workersLabel}</span><span className="value">{online}/{status?.worker_count ?? 0}</span></div>
          <div className="stat"><span className="label">{copy.healthyGatewaysLabel}</span><span className="value">{gateways.length}</span></div>
          <div className="stat"><span className="label">{copy.snapshotLabel}</span><span className="value mono">{snapshotAt}</span></div>
          <div className="stat"><span className="label">{copy.lastSyncLabel}</span><span className="value mono">{updatedAt}</span></div>
          <div className="stat"><span className="label">Join Tokens</span><span className="value">{joinTokensCount ?? "—"}</span></div>
          <div className="stat"><span className="label">Registrations</span><span className="value">{registrationsCount ?? "—"}</span></div>
        </div>
      </header>

      <div className="layout">
        <Topology status={status} selectedPeer={selectedPeer} onSelect={setSelectedPeer} language={language} />
        <SidePanel status={status} selectedPeer={selectedPeer} onSelect={setSelectedPeer} language={language} />
      </div>

      {error && <p className="err">{copy.statusFetchPrefix}{error}</p>}
      <section className="panel admin-panel">
        <h2>{language === "zh" ? "Admin" : "Admin"}</h2>
        <div className="admin-form">
          <button type="button" className="primary" onClick={() => void createBootstrapToken()} disabled={busy === "create-token"}>
            {busy === "create-token" ? (language === "zh" ? "创建中…" : "Creating…") : (language === "zh" ? "创建 10 分钟 Join Token" : "Create 10-minute Join Token")}
          </button>
        </div>
        {createdJoinToken && (
          <div className="token-created">
            <p>{language === "zh" ? "此 token 只显示一次，请立即复制。" : "This token is shown once. Copy it now."}</p>
            <div className="token-secret-row">
              <code className="token-secret">{createdJoinToken.token_value}</code>
              <button type="button" className="lang-switch" onClick={() => void copyCreatedToken()}>
                {language === "zh" ? "复制" : "Copy"}
              </button>
            </div>
            <p className="token-expiry">{language === "zh" ? "过期时间" : "Expires"}: {formatTime(language, createdJoinToken.expires_at_unix_ms)}</p>
          </div>
        )}
        <div className="token-list">
          {joinTokens.length === 0 && <p className="empty-line">{language === "zh" ? "暂无有效 Join Token" : "No active join tokens"}</p>}
          {joinTokens.map((token) => (
            <div className="token-row" key={token.id}>
              <div>
                <div className="token-description">{token.description || "—"}</div>
                <div className="token-expiry">{formatTime(language, token.expires_at_unix_ms)}</div>
              </div>
              <button type="button" className="lang-switch" onClick={() => void revokeBootstrapToken(token.id)} disabled={busy === `revoke-${token.id}`}>
                {language === "zh" ? "撤销" : "Revoke"}
              </button>
            </div>
          ))}
        </div>
        {adminError && <p className="err">{adminError}</p>}
      </section>
      <p className="footer">{copy.registryProxy} · poll {POLL_MS / 1000}s · source /v1/dashboard/status</p>
    </div>
  );
}
