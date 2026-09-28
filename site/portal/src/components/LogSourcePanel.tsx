import { useCallback, useEffect, useState } from 'react'
import {
  deleteLogSourceToken,
  getLogSourceTokenStatus,
  putLogSourceToken,
} from '../api/client'

interface Props {
  owner: string
  repo: string
}

export default function LogSourcePanel({ owner, repo }: Props) {
  const [configured, setConfigured] = useState<boolean | null>(null)
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    getLogSourceTokenStatus(owner, repo)
      .then(setConfigured)
      .catch(() => setConfigured(null))
  }, [owner, repo])

  useEffect(() => {
    load()
  }, [load])

  const save = async () => {
    if (!token.trim()) return
    setBusy(true)
    setError(null)
    try {
      await putLogSourceToken(owner, repo, token.trim())
      setToken('')
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to save token')
    } finally {
      setBusy(false)
    }
  }

  const remove = async () => {
    setBusy(true)
    setError(null)
    try {
      await deleteLogSourceToken(owner, repo)
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove token')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="card slack-panel">
      <h2>Log source</h2>
      <p className="muted">
        Configure the non-secret settings under <code>[log_source]</code> in the
        repo config above (provider, organization, project, query). The auth
        token is stored write-only on the worker — it never touches the
        repository.
      </p>
      {error && <p className="error-text">{error}</p>}
      <div className="slack-link-row">
        <span className="muted">
          Token: {configured === null ? '…' : configured ? '✅ configured' : 'not set'}
        </span>
        <input
          type="password"
          className="task-status-select"
          placeholder="Sentry auth token"
          value={token}
          onChange={(e) => setToken(e.target.value)}
        />
        <button
          className="btn btn-primary btn-sm"
          onClick={() => void save()}
          disabled={busy || !token.trim()}
        >
          Save token
        </button>
        {configured && (
          <button className="btn btn-sm btn-danger" onClick={() => void remove()} disabled={busy}>
            Remove
          </button>
        )}
      </div>
    </div>
  )
}
