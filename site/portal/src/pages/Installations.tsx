import { useEffect, useState, useMemo, useCallback } from 'react'
import { Link } from 'react-router-dom'
import { getInstallations, getInstallationRepos } from '../api/client'
import type { Installation, Repository } from '../api/client'

const DISPLAY_PER_PAGE = 30

interface RepoState {
  repos: Repository[]
  totalCount: number
  loading: boolean
}

export default function Installations() {
  const [installations, setInstallations] = useState<Installation[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<number | null>(null)
  const [repoState, setRepoState] = useState<Record<number, RepoState>>({})
  const [filter, setFilter] = useState('')
  const [displayPage, setDisplayPage] = useState(1)

  useEffect(() => {
    getInstallations()
      .then(setInstallations)
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load installations'),
      )
      .finally(() => setLoading(false))
  }, [])

  const fetchAllRepos = useCallback(async (installationId: number) => {
    setRepoState((prev) => ({
      ...prev,
      [installationId]: { repos: [], totalCount: 0, loading: true },
    }))

    const perPage = 100
    let page = 1
    let allRepos: Repository[] = []
    let totalCount = 0

    try {
      for (;;) {
        const data = await getInstallationRepos(installationId, page, perPage)
        totalCount = data.total_count ?? 0
        const repos = data.repositories ?? []
        allRepos = [...allRepos, ...repos]

        if (allRepos.length >= totalCount || repos.length < perPage) {
          break
        }
        page++
      }

      setRepoState((prev) => ({
        ...prev,
        [installationId]: { repos: allRepos, totalCount, loading: false },
      }))
    } catch {
      setRepoState((prev) => ({
        ...prev,
        [installationId]: { repos: [], totalCount: 0, loading: false },
      }))
    }
  }, [])

  function toggleExpand(id: number) {
    if (expanded === id) {
      setExpanded(null)
      return
    }
    setExpanded(id)
    setFilter('')
    setDisplayPage(1)
    if (!repoState[id]) {
      void fetchAllRepos(id)
    }
  }

  // Filter repos client-side across all loaded repos
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

  // Client-side pagination of filtered results
  const totalDisplayPages =
    DISPLAY_PER_PAGE > 0 ? Math.ceil(filteredRepos.length / DISPLAY_PER_PAGE) : 0
  const displayedRepos = filteredRepos.slice(
    (displayPage - 1) * DISPLAY_PER_PAGE,
    displayPage * DISPLAY_PER_PAGE,
  )

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
                    {state && !state.loading && state.repos.length > 0 && (
                      <input
                        type="text"
                        className="repo-filter"
                        placeholder="Filter repositories…"
                        value={filter}
                        onChange={(e) => {
                          setFilter(e.target.value)
                          setDisplayPage(1)
                        }}
                      />
                    )}
                    {state?.loading ? (
                      <p className="muted">Loading repos…</p>
                    ) : !state || state.repos.length === 0 ? (
                      <p className="muted">No repositories found.</p>
                    ) : filteredRepos.length === 0 ? (
                      <p className="muted">No repositories match your filter.</p>
                    ) : (
                      <>
                        <ul className="repo-list">
                          {displayedRepos.map((r) => (
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
                        {totalDisplayPages > 1 && (
                          <div className="pagination">
                            <button
                              className="btn btn-sm"
                              disabled={displayPage <= 1}
                              onClick={() => setDisplayPage((p) => p - 1)}
                            >
                              ← Prev
                            </button>
                            <span className="muted pagination-info">
                              Page {displayPage} of {totalDisplayPages} ({filteredRepos.length}{' '}
                              repos)
                            </span>
                            <button
                              className="btn btn-sm"
                              disabled={displayPage >= totalDisplayPages}
                              onClick={() => setDisplayPage((p) => p + 1)}
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
