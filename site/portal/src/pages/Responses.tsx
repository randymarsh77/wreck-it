import { useCallback, useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { responseRequest } from "../api/client";

type Account = {
  id: string;
  owner: string;
  harness: string;
  auth: string;
  credential_ref: string;
  enabled: boolean;
  models: { model: string; capabilities: string[] }[];
  max_concurrent_sessions: number;
};
type Incident = {
  id: string;
  phase: string;
  reason: string;
  attempt: number;
  occurrences: number;
  signal: { title: string };
  selection?: { account_id: string; model: string };
  decision?: { excluded: { account_id: string; reason: string }[] };
  pr?: { url: string };
  deployment?: { url: string };
  draft?: string;
  history: { at: number; from: string; to: string; reason: string }[];
};
type View = {
  owner: string;
  policy: Record<string, unknown> | null;
  accounts: Account[];
  quotas: Record<
    string,
    {
      source: string;
      complete: boolean;
      snapshot: {
        observed_at: number;
        windows: { remaining_basis_points: number; resets_at: number }[];
        blocked_until: number | null;
        requires_reauthentication: boolean;
      };
    }
  >;
  incidents: Incident[];
};
const date = (n: number) => new Date(n * 1000).toLocaleString();
export default function Responses() {
  const { owner = "", repo = "" } = useParams();
  const [data, setData] = useState<View | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [policy, setPolicy] = useState("");
  const [account, setAccount] = useState("");
  const [quota, setQuota] = useState("");
  const load = useCallback(async () => {
    try {
      const v = await responseRequest<View>(owner, repo, "view");
      setData(v);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, [owner, repo]);
  useEffect(() => {
    void load();
    const interval = setInterval(() => void load(), 15000);
    return () => clearInterval(interval);
  }, [load]);
  async function save(action: string, input: unknown) {
    setBusy(true);
    setMessage("");
    try {
      await responseRequest(owner, repo, action, input);
      setMessage("Saved.");
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  function submit(action: string, text: string) {
    try {
      void save(action, JSON.parse(text));
    } catch {
      setError("Enter valid JSON before saving.");
    }
  }
  return (
    <section>
      <div className="page-header">
        <h1>
          Autonomous responses · {owner}/{repo}
        </h1>
        <Link to={`/repos/${owner}/${repo}/config`}>
          Repository configuration
        </Link>
      </div>
      <p>
        Production signals trigger official coding harnesses. An incident closes
        only after the matching deployment passes its observation window.
      </p>
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {message && <p role="status">{message}</p>}
      {!data ? (
        <p>Loading response state…</p>
      ) : (
        <>
          <h2>Accounts and observed allowance</h2>
          <p>
            Credential owner: {data.owner}. Credentials are provisioned by the
            operator; enter references only. Claude subscription tokens cannot
            be pooled here.
          </p>
          <table>
            <thead>
              <tr>
                <th>Account</th>
                <th>Harness / models</th>
                <th>Authentication</th>
                <th>Observed usage</th>
                <th>Action</th>
              </tr>
            </thead>
            <tbody>
              {data.accounts.map((a) => {
                const q = data.quotas[a.id];
                return (
                  <tr key={a.id}>
                    <td>
                      {a.id} · {a.enabled ? "enabled" : "disabled"}
                    </td>
                    <td>
                      {a.harness}
                      <br />
                      {a.models
                        .map((m) => `${m.model} (${m.capabilities.join(", ")})`)
                        .join("; ")}
                    </td>
                    <td>
                      {a.auth}
                      <br />
                      {q?.snapshot.requires_reauthentication
                        ? "Reauthentication required"
                        : ""}
                    </td>
                    <td>
                      {q
                        ? `${q.source} · ${q.complete ? "complete" : "unavailable"} · observed ${date(q.snapshot.observed_at)}`
                        : "Unknown — routing deferred"}
                      {q?.snapshot.windows.map((w, n) => (
                        <div key={n}>
                          {w.remaining_basis_points / 100}% remaining; resets{" "}
                          {date(w.resets_at)}
                        </div>
                      ))}
                    </td>
                    <td>
                      <button
                        className="btn"
                        onClick={() => setAccount(JSON.stringify(a, null, 2))}
                      >
                        Edit
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <details>
            <summary>Configure an account</summary>
            <p>
              Use codex + api_key or native_subscription, claude_code + api_key,
              or copilot + copilot_token. Each model lists its supported triage,
              repair, or deep_repair capabilities.
            </p>
            <button
              className="btn"
              onClick={() =>
                setAccount(
                  JSON.stringify(
                    {
                      id: "codex",
                      owner: data.owner,
                      harness: "codex",
                      auth: "api_key",
                      credential_ref: "secret:CODEX_PERSONAL",
                      enabled: true,
                      models: [
                        { model: "YOUR_MODEL", capabilities: ["repair"] },
                      ],
                      max_concurrent_sessions: 1,
                    },
                    null,
                    2,
                  ),
                )
              }
            >
              New account example
            </button>
            <textarea
              aria-label="Account configuration"
              rows={16}
              value={account}
              onChange={(e) => setAccount(e.target.value)}
            />
            <button
              disabled={busy || !account}
              className="btn"
              onClick={() => submit("account", account)}
            >
              Save account
            </button>
          </details>
          <details>
            <summary>Import observed allowance</summary>
            <p>
              Import a complete, timestamped report from a supported provider
              interface or an owner-observed report. Unknown allowance defers
              routing. Values are remaining basis points (10,000 = 100%);
              include every applicable window.
            </p>
            <button
              className="btn"
              onClick={() =>
                setQuota(
                  JSON.stringify(
                    {
                      source: "operator_report",
                      complete: false,
                      snapshot: {
                        account_id: data.accounts[0]?.id ?? "ACCOUNT",
                        observed_at: Math.floor(Date.now() / 1000),
                        windows: [],
                        active_sessions: 0,
                        blocked_until: null,
                        requires_reauthentication: false,
                      },
                    },
                    null,
                    2,
                  ),
                )
              }
            >
              Report example
            </button>
            <textarea
              aria-label="Observed allowance"
              rows={12}
              value={quota}
              onChange={(e) => setQuota(e.target.value)}
            />
            <button
              disabled={busy || !quota}
              className="btn"
              onClick={() => submit("quota", quota)}
            >
              Import report
            </button>
          </details>
          <h2>Delivery policy</h2>
          <p>
            Configured policy is separate from observed CI and deployment
            results. Missing checks or missing health evidence cannot authorize
            delivery or resolution.
          </p>
          <button
            className="btn"
            onClick={() =>
              setPolicy(
                JSON.stringify(
                  data.policy ?? {
                    repo: `${owner}/${repo}`,
                    enabled: false,
                    base_branch: "main",
                    sources: [
                      {
                        id: "errors",
                        kind: "error_report",
                        secret_ref: "secret:ERROR_SIGNING",
                        capability: "repair",
                        senders: [],
                      },
                      {
                        id: "health",
                        kind: "health",
                        secret_ref: "secret:HEALTH_SIGNING",
                        capability: "repair",
                        senders: [],
                      },
                    ],
                    required_checks: ["test"],
                    require_review: true,
                    auto_merge: false,
                    blocked_paths: [".github/", ".wreck-it/"],
                    environment: "production",
                    deployment_workflow: "deploy.yml",
                    observation_seconds: 300,
                    health_max_age_seconds: 60,
                    max_attempts: 3,
                    session_timeout_seconds: 900,
                    verification: [["npm", "test"]],
                    minimum_remaining_basis_points: 1000,
                    usage_max_age_seconds: 300,
                  },
                  null,
                  2,
                ),
              )
            }
          >
            Edit policy
          </button>
          {policy && (
            <>
              <textarea
                aria-label="Delivery policy"
                rows={24}
                value={policy}
                onChange={(e) => setPolicy(e.target.value)}
              />
              <button
                className="btn"
                disabled={busy}
                onClick={() => submit("policy", policy)}
              >
                Save policy
              </button>
            </>
          )}
          <h2>Incidents</h2>
          <button
            className="btn"
            disabled={busy}
            onClick={() => void save("tick", {})}
          >
            Reconcile now
          </button>
          {data.incidents.length === 0 && <p>No signals received.</p>}
          {data.incidents.map((i) => (
            <article key={i.id} className="card">
              <h3>{i.signal.title}</h3>
              <p>
                <strong>{i.phase}</strong> · attempt {i.attempt} ·{" "}
                {i.occurrences} occurrences
              </p>
              <p>{i.reason}</p>
              {i.selection && (
                <p>
                  {i.selection.account_id} / {i.selection.model}
                </p>
              )}
              {i.decision?.excluded.map((x) => (
                <p key={x.account_id}>
                  {x.account_id}: {x.reason}
                </p>
              ))}
              {i.pr && (
                <a href={i.pr.url} target="_blank" rel="noreferrer">
                  Pull request
                </a>
              )}{" "}
              {i.deployment && (
                <a href={i.deployment.url} target="_blank" rel="noreferrer">
                  Deployment
                </a>
              )}
              {i.draft && <p>Support response draft (not sent): {i.draft}</p>}
              <details>
                <summary>Lifecycle history</summary>
                {i.history.map((h, n) => (
                  <p key={n}>
                    {date(h.at)} · {h.from} → {h.to}: {h.reason}
                  </p>
                ))}
              </details>
              {!["resolved", "needs_attention", "cancelled"].includes(
                i.phase,
              ) && (
                <button
                  className="btn"
                  disabled={busy}
                  onClick={() => void save("cancel", { id: i.id })}
                >
                  Cancel response
                </button>
              )}
            </article>
          ))}
        </>
      )}
    </section>
  );
}
