import { useEffect, useState } from 'react'
import { useParams } from 'react-router-dom'
import { getRepoConfig, updateRepoConfig } from '../api/client'
import type { RalphConfig } from '../api/client'

export default function RepoConfig() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>()
  const [config, setConfig] = useState<RalphConfig | null>(null)
  const [draft, setDraft] = useState('')
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saveMsg, setSaveMsg] = useState<string | null>(null)
  const [parseError, setParseError] = useState<string | null>(null)

  useEffect(() => {
    if (!owner || !repo) return
    setLoading(true)
    getRepoConfig(owner, repo)
      .then((c) => {
        setConfig(c)
        setDraft(JSON.stringify(c, null, 2))
      })
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load config'),
      )
      .finally(() => setLoading(false))
  }, [owner, repo])

  function handleDraftChange(value: string) {
    setDraft(value)
    setSaveMsg(null)
    try {
      JSON.parse(value)
      setParseError(null)
    } catch {
      setParseError('Invalid JSON')
    }
  }

  async function handleSave() {
    if (!owner || !repo || parseError) return
    setSaving(true)
    setSaveMsg(null)
    setError(null)
    try {
      const parsed = JSON.parse(draft) as RalphConfig
      const updated = await updateRepoConfig(owner, repo, parsed)
      setConfig(updated)
      setDraft(JSON.stringify(updated, null, 2))
      setSaveMsg('Configuration saved.')
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save config')
    } finally {
      setSaving(false)
    }
  }

  if (loading) return <div className="loading">Loading configuration…</div>

  return (
    <div className="repo-config">
      <h1>
        {owner}/{repo}
      </h1>
      <h2>Ralph Configuration</h2>

      {error && <p className="error-text">{error}</p>}
      {saveMsg && <p className="success-text">{saveMsg}</p>}

      {config !== null ? (
        <>
          <textarea
            className="config-editor"
            value={draft}
            onChange={(e) => handleDraftChange(e.target.value)}
            spellCheck={false}
            rows={20}
          />
          {parseError && <p className="error-text">{parseError}</p>}
          <button
            className="btn btn-primary"
            onClick={() => void handleSave()}
            disabled={saving || !!parseError}
          >
            {saving ? 'Saving…' : 'Save Configuration'}
          </button>
        </>
      ) : (
        !error && <p className="muted">No configuration found.</p>
      )}
    </div>
  )
}
