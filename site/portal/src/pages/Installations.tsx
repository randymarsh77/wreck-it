import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { getInstallations, getInstallationRepos } from '../api/client'
import type { Installation, Repository } from '../api/client'

const PER_PAGE = 30

export default function Installations() {
  const [installations, setInstallations] = useState<Installation[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<number | null>(null)
  const [repos, setRepos] = useState<Record<number, Repository[]>>({})
  const [totalCounts, setTotalCounts] = useState<Record<number, number>>({})
  const [pages, setPages] = useState<Record<number, number>>({})
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

  function fetchRepos(id: number, page: number) {
    setReposLoading(id)
    getInstallationRepos(id, page, PER_PAGE)
      .then((data) => {
        setRepos((prev) => ({ ...prev, [id]: data.repositories }))
        setTotalCounts((prev) => ({ ...prev, [id]: data.total_count }))
        setPages((prev) => ({ ...prev, [id]: page }))
      })
      .catch(() => {
        setRepos((prev) => ({ ...prev, [id]: [] }))
        setTotalCounts((prev) => ({ ...prev, [id]: 0 }))
      })
      .finally(() => setReposLoading(null))
  }

  function toggleExpand(id: number) {
    if (expanded === id) {
      setExpanded(null)
      return
    }
    setExpanded(id)
    if (!repos[id]) {
      fetchRepos(id, 1)
    }
  }

  function goToPage(id: number, page: number) {
    fetchRepos(id, page)
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
          {installations.map((inst) => {
            const currentPage = pages[inst.id] ?? 1
            const totalCount = totalCounts[inst.id] ?? 0
            const totalPages = Math.max(1, Math.ceil(totalCount / PER_PAGE))
            const instRepos = repos[inst.id] ?? []

            const lowerFilter = filter.toLowerCase()
            const filteredRepos = lowerFilter
              ? instRepos.filter(
                  (r) =>
                    r.full_name.toLowerCase().includes(lowerFilter) ||
                    (r.description?.toLowerCase().includes(lowerFilter) ?? false),
                )
              : instRepos

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
                    {reposLoading === inst.id ? (
                      <p className="muted">Loading repos…</p>
                    ) : instRepos.length === 0 ? (
                      <p className="muted">No repositories found.</p>
                    ) : (
                      <>
                        <div className="repo-toolbar">
                          <input
                            type="text"
                            className="repo-filter"
                            placeholder="Filter repositories…"
                            value={filter}
                            onChange={(e) => setFilter(e.target.value)}
                          />
                          <span className="muted repo-count">
                            {filteredRepos.length} of {totalCount} repos
                          </span>
                        </div>

                        {filteredRepos.length === 0 ? (
                          <p className="muted">No matching repositories.</p>
                        ) : (
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
                        )}

                        {totalPages > 1 && (
                          <div className="pagination">
                            <button
                              className="btn btn-sm"
                              disabled={currentPage <= 1}
                              onClick={() => goToPage(inst.id, currentPage - 1)}
                            >
                              ← Prev
                            </button>
                            <span className="muted pagination-info">
                              Page {currentPage} of {totalPages}
                            </span>
                            <button
                              className="btn btn-sm"
                              disabled={currentPage >= totalPages}
                              onClick={() => goToPage(inst.id, currentPage + 1)}
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
