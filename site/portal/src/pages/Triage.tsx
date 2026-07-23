import { useCallback, useEffect, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { dismissTriageItem, getTriage, retryTriageItem } from '../api/client'
import type { TriageItem, TriageStatus } from '../api/client'

const STATUS_LABELS: Record<TriageStatus, string> = {
  new: 'New',
  investigating: 'Investigating',
  pr_open: 'PR Open',
  resolved: 'Resolved',
  dismissed: 'Dismissed',
  stale: 'Stale',
}

const OPEN_STATUSES: TriageStatus[] = ['new', 'investigating', 'pr_open']
const RETRYABLE_STATUSES: TriageStatus[] = ['new', 'dismissed', 'stale']

function formatTime(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleString()
}

export default function Triage() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>()
  const [items, setItems] = useState<TriageItem[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busyId, setBusyId] = useState<string | null>(null)
  const [showResolved, setShowResolved] = useState(false)

  const load = useCallback(() => {
    if (!owner || !repo) return
    setLoading(true)
    getTriage(owner, repo)
      .then((data) => {
        // Newest activity first.
        setItems([...data].sort((a, b) => b.updated_at - a.updated_at))
        setError(null)
      })
      .catch((e) => setError(e instanceof Error ? e.message : 'Failed to load triage items'))
      .finally(() => setLoading(false))
  }, [owner, repo])

  useEffect(() => {
    load()
  }, [load])

  if (!owner || !repo) return null
  if (loading) return <div className="loading">Loading triage items…</div>
  if (error) return <div className="error-text">{error}</div>

  const visible = showResolved ? items : items.filter((i) => OPEN_STATUSES.includes(i.status))

  const act = async (id: string, action: 'dismiss' | 'retry') => {
    setBusyId(id)
    try {
      const fn = action === 'dismiss' ? dismissTriageItem : retryTriageItem
      const updated = await fn(owner, repo, id)
      setItems((prev) => prev.map((i) => (i.id === id ? updated : i)))
    } catch (e) {
      setError(e instanceof Error ? e.message : `Failed to ${action} item`)
    } finally {
      setBusyId(null)
    }
  }

  return (
    <div className="triage">
      <div className="triage-header">
        <h1>
          Triage — {owner}/{repo}
        </h1>
        <div className="triage-header-actions">
          <label className="muted triage-toggle">
            <input
              type="checkbox"
              checked={showResolved}
              onChange={(e) => setShowResolved(e.target.checked)}
            />{' '}
            Show resolved
          </label>
          <Link to={`/repos/${owner}/${repo}/config`} className="btn btn-sm">
            Repo Config
          </Link>
        </div>
      </div>

      {visible.length === 0 ? (
        <p className="muted">
          {items.length === 0
            ? 'No triage items. Failing CI runs will show up here when [triage] is enabled in .wreck-it/config.toml.'
            : 'No open triage items. 🎉'}
        </p>
      ) : (
        <ul className="triage-list">
          {visible.map((item) => (
            <li key={item.id} className={`card triage-card triage-${item.status}`}>
              <div className="triage-card-header">
                <span className={`triage-status-badge status-${item.status}`}>
                  {STATUS_LABELS[item.status]}
                </span>
                <span className={`triage-severity severity-${item.severity}`}>
                  {item.severity}
                </span>
                <span className="triage-title">{item.title}</span>
                {item.occurrences > 1 && (
                  <span className="muted triage-occurrences">×{item.occurrences}</span>
                )}
              </div>
              <div className="triage-meta muted">
                <span>Updated {formatTime(item.updated_at)}</span>
                {item.source.run_url && (
                  <a href={item.source.run_url} target="_blank" rel="noreferrer">
                    CI run
                  </a>
                )}
                {item.issue_number && (
                  <a
                    href={`https://github.com/${owner}/${repo}/issues/${item.issue_number}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Issue #{item.issue_number}
                  </a>
                )}
                {item.pr_number && (
                  <a
                    href={`https://github.com/${owner}/${repo}/pull/${item.pr_number}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    PR #{item.pr_number}
                  </a>
                )}
              </div>
              {item.detail && (
                <details className="triage-detail">
                  <summary className="muted">Evidence</summary>
                  <pre>{item.detail}</pre>
                </details>
              )}
              <div className="triage-actions">
                {RETRYABLE_STATUSES.includes(item.status) &&
                  item.source.type === 'ci_failure' && (
                    <button
                      className="btn btn-sm btn-primary"
                      disabled={busyId === item.id}
                      onClick={() => act(item.id, 'retry')}
                    >
                      {busyId === item.id ? 'Dispatching…' : 'Dispatch fix'}
                    </button>
                  )}
                {OPEN_STATUSES.includes(item.status) && (
                  <button
                    className="btn btn-sm btn-danger"
                    disabled={busyId === item.id}
                    onClick={() => act(item.id, 'dismiss')}
                  >
                    Dismiss
                  </button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
