import { useEffect, useState, useMemo, useCallback, useRef } from 'react'
import { Link } from 'react-router-dom'
import {
  getInstallations,
  getInstallationRepos,
  getInstallationSettings,
  updateInstallationSettings,
} from '../api/client'
import type { Installation, Repository, InstallationSettings } from '../api/client'

const DISPLAY_PER_PAGE = 30

interface RepoState {
  repos: Repository[]
  totalCount: number
  loading: boolean
}

interface SettingsState {
  settings: InstallationSettings | null
  loading: boolean
  saving: boolean
  error: string | null
}

export default function Installations() {
  const [installations, setInstallations] = useState<Installation[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState<number | null>(null)
  const [repoState, setRepoState] = useState<Record<number, RepoState>>({})
  const [settingsState, setSettingsState] = useState<Record<number, SettingsState>>({})
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

  const fetchSettings = useCallback(async (installationId: number) => {
    setSettingsState((prev) => ({
      ...prev,
      [installationId]: { settings: null, loading: true, saving: false, error: null },
    }))

    try {
      const settings = await getInstallationSettings(installationId)
      setSettingsState((prev) => ({
        ...prev,
        [installationId]: { settings, loading: false, saving: false, error: null },
      }))
    } catch (err: unknown) {
      setSettingsState((prev) => ({
        ...prev,
        [installationId]: {
          settings: null,
          loading: false,
          saving: false,
          error: err instanceof Error ? err.message : 'Failed to load settings',
        },
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
    if (!settingsState[id]) {
      void fetchSettings(id)
    }
  }

  async function handleSettingsUpdate(
    installationId: number,
    updates: Partial<InstallationSettings>,
  ) {
    const current = settingsState[installationId]?.settings
    if (!current) return

    const newSettings: InstallationSettings = { ...current, ...updates }

    setSettingsState((prev) => ({
      ...prev,
      [installationId]: { ...prev[installationId], saving: true, error: null },
    }))

    try {
      const saved = await updateInstallationSettings(installationId, newSettings)
      setSettingsState((prev) => ({
        ...prev,
        [installationId]: { settings: saved, loading: false, saving: false, error: null },
      }))
    } catch (err: unknown) {
      setSettingsState((prev) => ({
        ...prev,
        [installationId]: {
          ...prev[installationId],
          saving: false,
          error: err instanceof Error ? err.message : 'Failed to save settings',
        },
      }))
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
            const sState = settingsState[inst.id]
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
                    <InstallationSettingsPanel
                      installationId={inst.id}
                      state={sState ?? null}
                      onUpdate={(updates) => handleSettingsUpdate(inst.id, updates)}
                    />
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

// ---------------------------------------------------------------------------
// Installation settings sub-component
// ---------------------------------------------------------------------------

interface SettingsPanelProps {
  installationId: number
  state: SettingsState | null
  onUpdate: (updates: Partial<InstallationSettings>) => void
}

function InstallationSettingsPanel({ installationId, state, onUpdate }: SettingsPanelProps) {
  const currentCron = state?.settings?.pulse_cron ?? '*/30 * * * *'
  const [cronDraft, setCronDraft] = useState(currentCron)
  const [cronDirty, setCronDirty] = useState(false)

  // Reset cronDraft when the settings cron changes externally (e.g. after
  // save round-trip).  Using a ref to track the previous cron avoids
  // calling setState inside an effect.
  const prevCronRef = useRef(currentCron)
  if (prevCronRef.current !== currentCron) {
    prevCronRef.current = currentCron
    if (!cronDirty) {
      setCronDraft(currentCron)
    }
  }

  if (!state || state.loading) {
    return <div className="settings-panel muted">Loading settings…</div>
  }

  if (state.error && !state.settings) {
    return <div className="settings-panel error-text">{state.error}</div>
  }

  const settings = state.settings
  if (!settings) return null

  // Suppress unused variable lint — installationId is used for future
  // extensibility (e.g. unique element IDs) but currently only needed in
  // the parent's onUpdate callback.
  void installationId

  return (
    <div className="settings-panel card" style={{ marginBottom: '1rem', padding: '0.75rem' }}>
      <h4 style={{ margin: '0 0 0.5rem 0' }}>Installation Settings</h4>

      {state.error && <div className="error-text" style={{ marginBottom: '0.5rem' }}>{state.error}</div>}

      <div className="settings-row" style={{ display: 'flex', alignItems: 'center', gap: '0.75rem', marginBottom: '0.5rem' }}>
        <label style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', cursor: 'pointer' }}>
          <input
            type="checkbox"
            checked={settings.events_enabled}
            disabled={state.saving}
            onChange={(e) => onUpdate({ events_enabled: e.target.checked })}
          />
          Events &amp; triggers enabled
        </label>
      </div>

      <div className="settings-row" style={{ display: 'flex', alignItems: 'center', gap: '0.75rem', marginBottom: '0.5rem' }}>
        <label style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', cursor: 'pointer' }}>
          <input
            type="checkbox"
            checked={settings.pulse_enabled}
            disabled={state.saving}
            onChange={(e) => onUpdate({ pulse_enabled: e.target.checked })}
          />
          Scheduled pulse enabled
        </label>
      </div>

      <div className="settings-row" style={{ display: 'flex', alignItems: 'center', gap: '0.75rem' }}>
        <label style={{ whiteSpace: 'nowrap' }}>Pulse schedule (cron):</label>
        <input
          type="text"
          value={cronDraft}
          disabled={state.saving || !settings.pulse_enabled}
          onChange={(e) => {
            setCronDraft(e.target.value)
            setCronDirty(e.target.value !== settings.pulse_cron)
          }}
          style={{ flex: 1, minWidth: '10rem' }}
        />
        {cronDirty && (
          <button
            className="btn btn-sm"
            disabled={state.saving}
            onClick={() => {
              onUpdate({ pulse_cron: cronDraft })
              setCronDirty(false)
            }}
          >
            Apply
          </button>
        )}
      </div>

      {state.saving && <p className="muted" style={{ marginTop: '0.5rem' }}>Saving…</p>}
    </div>
  )
}
