import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { getInstallations, getInstallationRepos } from '../api/client'
import type { Installation, Repository } from '../api/client'

export default function Installations() {
  const [installations, setInstallations] = useState<Installation[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<number | null>(null)
  const [repos, setRepos] = useState<Record<number, Repository[]>>({})
  const [reposLoading, setReposLoading] = useState<number | null>(null)

  useEffect(() => {
    getInstallations()
      .then(setInstallations)
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load installations'),
      )
      .finally(() => setLoading(false))
  }, [])

  function toggleExpand(id: number) {
    if (expanded === id) {
      setExpanded(null)
      return
    }
    setExpanded(id)
    if (!repos[id]) {
      setReposLoading(id)
      getInstallationRepos(id)
        .then((r) => setRepos((prev) => ({ ...prev, [id]: r })))
        .catch(() => setRepos((prev) => ({ ...prev, [id]: [] })))
        .finally(() => setReposLoading(null))
    }
  }

  if (loading) return <div className="loading">Loading installations…</div>
  if (error) return <div className="error-text">{error}</div>

  return (
    <div className="installations">
      <h1>Installations</h1>
      {installations.length === 0 ? (
        <p className="muted">No installations found.</p>
      ) : (
        <ul className="install-list">
          {installations.map((inst) => (
            <li key={inst.id} className="card install-card">
              <button className="install-header" onClick={() => toggleExpand(inst.id)}>
                <img
                  src={inst.account.avatar_url}
                  alt={inst.account.login}
                  className="avatar"
                />
                <div className="install-info">
                  <strong>{inst.account.login}</strong>
                  <span className="muted">ID: {inst.id}</span>
                </div>
                <span className={`chevron ${expanded === inst.id ? 'open' : ''}`}>▸</span>
              </button>
              {expanded === inst.id && (
                <div className="install-repos">
                  {reposLoading === inst.id ? (
                    <p className="muted">Loading repos…</p>
                  ) : !repos[inst.id] || repos[inst.id].length === 0 ? (
                    <p className="muted">No repositories found.</p>
                  ) : (
                    <ul className="repo-list">
                      {repos[inst.id].map((r) => (
                        <li key={r.id}>
                          <Link to={`/repos/${r.owner.login}/${r.name}/config`}>
                            {r.full_name}
                          </Link>
                          {r.description && (
                            <span className="muted repo-desc">{r.description}</span>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
