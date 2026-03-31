import { useEffect, useState, useMemo } from 'react'
import { Link } from 'react-router-dom'
import { getInstallations, getInstallationRepos } from '../api/client'
import type { Installation, Repository } from '../api/client'

const REPOS_PER_PAGE = 30

interface RepoState {
  repos: Repository[]
  totalCount: number
  page: number
}

export default function Installations() {
  const [installations, setInstallations] = useState<Installation[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<number | null>(null)
  const [repoState, setRepoState] = useState<Record<number, RepoState>>({})
  const [reposLoading, setReposLoading] = useState<number | null>(null)
  const [filter, setFilter] = useState('')

  useEffect(() => {
    getInstallations()
      .then(setInstallations)
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load installations'),
      )
      .finally(() => setLoading(false))
  }, [])

  function fetchRepos(installationId: number, page: number) {
    setReposLoading(installationId)
    getInstallationRepos(installationId, page, REPOS_PER_PAGE)
      .then((data) =>
        setRepoState((prev) => ({
          ...prev,
          [installationId]: {
            repos: data.repositories ?? [],
            totalCount: data.total_count ?? 0,
            page,
          },
        })),
      )
      .catch(() =>
        setRepoState((prev) => ({
          ...prev,
          [installationId]: { repos: [], totalCount: 0, page: 1 },
        })),
      )
      .finally(() => setReposLoading(null))
  }

  function toggleExpand(id: number) {
    if (expanded === id) {
      setExpanded(null)
      return
    }
    setExpanded(id)
    if (!repoState[id]) {
      fetchRepos(id, 1)
    }
  }

  function goToPage(installationId: number, page: number) {
    fetchRepos(installationId, page)
  }

  // Filter repos client-side by name or description
  const filteredRepos = useMemo(() => {
    if (expanded === null || !repoState[expanded]) return []
    const q = filter.toLowerCase()
    if (!q) return repoState[expanded].repos
    return repoState[expanded].repos.filter(
      (r) =>
        r.full_name.toLowerCase().includes(q) ||
        (r.description && r.description.toLowerCase().includes(q)),
    )
  }, [expanded, repoState, filter])

  if (loading) return <div className="loading">Loading installations…</div>
  if (error) return <div className="error-text">{error}</div>

  return (
    <div className="installations">
      <h1>Installations</h1>
      {installations.length === 0 ? (
        <p className="muted">No installations found.</p>
      ) : (
        <ul className="install-list">
          {installations.map((inst) => {
            const state = repoState[inst.id]
            const totalPages = state ? Math.ceil(state.totalCount / REPOS_PER_PAGE) : 0
            return (
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
                    {state && state.repos.length > 0 && (
                      <input
                        type="text"
                        className="repo-filter"
                        placeholder="Filter repositories…"
                        value={filter}
                        onChange={(e) => setFilter(e.target.value)}
                      />
                    )}
                    {reposLoading === inst.id ? (
                      <p className="muted">Loading repos…</p>
                    ) : !state || state.repos.length === 0 ? (
                      <p className="muted">No repositories found.</p>
                    ) : filteredRepos.length === 0 ? (
                      <p className="muted">No repositories match your filter.</p>
                    ) : (
                      <>
                        <ul className="repo-list">
                          {filteredRepos.map((r) => (
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
                        {totalPages > 1 && (
                          <div className="pagination">
                            <button
                              className="btn btn-sm"
                              disabled={state.page <= 1 || reposLoading === inst.id}
                              onClick={() => goToPage(inst.id, state.page - 1)}
                            >
                              ← Prev
                            </button>
                            <span className="muted pagination-info">
                              Page {state.page} of {totalPages}
                            </span>
                            <button
                              className="btn btn-sm"
                              disabled={state.page >= totalPages || reposLoading === inst.id}
                              onClick={() => goToPage(inst.id, state.page + 1)}
                            >
                              Next →
                            </button>
                          </div>
                        )}
                      </>
                    )}
                  </div>
                )}
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
