import { useEffect, useState, useCallback } from 'react'
import { useParams } from 'react-router-dom'
import {
  getRepoConfig,
  updateRepoConfig,
  getTemplates,
  deployRalph,
} from '../api/client'
import type {
  RalphConfig,
  RalphTemplate,
  RalphTemplateEntry,
} from '../api/client'

export default function RepoConfig() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>()
  const [config, setConfig] = useState<RalphConfig | null>(null)
  const [draft, setDraft] = useState('')
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saveMsg, setSaveMsg] = useState<string | null>(null)
  const [parseError, setParseError] = useState<string | null>(null)
  const [mode, setMode] = useState<'gui' | 'json'>('gui')

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

  const syncDraftFromConfig = useCallback(
    (newConfig: RalphConfig) => {
      setConfig(newConfig)
      setDraft(JSON.stringify(newConfig, null, 2))
      setParseError(null)
    },
    [],
  )

  function handleModeSwitch(newMode: 'gui' | 'json') {
    if (newMode === 'gui' && mode === 'json') {
      try {
        const parsed = JSON.parse(draft) as RalphConfig
        setConfig(parsed)
        setParseError(null)
      } catch {
        setParseError('Fix JSON errors before switching to GUI mode')
        return
      }
    } else if (newMode === 'json' && mode === 'gui' && config) {
      setDraft(JSON.stringify(config, null, 2))
    }
    setMode(newMode)
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
          <div className="mode-switcher">
            <button
              className={`btn btn-sm ${mode === 'gui' ? 'btn-primary' : ''}`}
              onClick={() => handleModeSwitch('gui')}
            >
              GUI
            </button>
            <button
              className={`btn btn-sm ${mode === 'json' ? 'btn-primary' : ''}`}
              onClick={() => handleModeSwitch('json')}
            >
              JSON
            </button>
          </div>

          {mode === 'json' ? (
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
            <GuiMode
              owner={owner!}
              repo={repo!}
              config={config}
              onConfigChange={syncDraftFromConfig}
              saving={saving}
              setSaving={setSaving}
              setError={setError}
              setSaveMsg={setSaveMsg}
            />
          )}
        </>
      ) : (
        !error && <p className="muted">No configuration found.</p>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// GUI Mode component
// ---------------------------------------------------------------------------

interface GuiModeProps {
  owner: string
  repo: string
  config: RalphConfig
  onConfigChange: (config: RalphConfig) => void
  saving: boolean
  setSaving: (v: boolean) => void
  setError: (v: string | null) => void
  setSaveMsg: (v: string | null) => void
}

interface RalphEntry {
  name: string
  task_file: string
  state_file: string
  branch?: string
  agent?: string
  command?: string
  backend?: string
  prompt_dir?: string
  validation_command?: string
  brute_mode?: boolean
  reviewers?: string[]
}

function GuiMode({
  owner,
  repo,
  config,
  onConfigChange,
  saving,
  setSaving,
  setError,
  setSaveMsg,
}: GuiModeProps) {
  const [templates, setTemplates] = useState<RalphTemplate[]>([])
  const [showTemplates, setShowTemplates] = useState(false)
  const [showCustomForm, setShowCustomForm] = useState(false)
  const [deploying, setDeploying] = useState<string | null>(null)
  const [customName, setCustomName] = useState('')
  const [customPrompt, setCustomPrompt] = useState('')

  const ralphs = (config.ralphs as RalphEntry[] | undefined) ?? []

  useEffect(() => {
    getTemplates()
      .then(setTemplates)
      .catch(() => {
        /* templates unavailable — not critical */
      })
  }, [])

  async function handleSaveConfig(newConfig: RalphConfig) {
    setSaving(true)
    setSaveMsg(null)
    setError(null)
    try {
      const updated = await updateRepoConfig(owner, repo, newConfig)
      onConfigChange(updated)
      setSaveMsg('Configuration saved.')
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save config')
    } finally {
      setSaving(false)
    }
  }

  async function handleDeployTemplate(template: RalphTemplate) {
    setDeploying(template.id)
    setError(null)
    setSaveMsg(null)
    try {
      for (const ralph of template.ralphs) {
        await deployRalph(owner, repo, {
          name: ralph.name,
          task_file: ralph.task_file,
          state_file: ralph.state_file,
          tasks: [],
          command: ralph.command,
          backend: ralph.backend,
        })
      }

      const existing = (config.ralphs as RalphEntry[] | undefined) ?? []
      const existingNames = new Set(existing.map((r) => r.name))
      const newRalphs = template.ralphs
        .filter((r) => !existingNames.has(r.name))
        .map((r: RalphTemplateEntry) => {
          const entry: Record<string, unknown> = {
            name: r.name,
            task_file: r.task_file,
            state_file: r.state_file,
          }
          if (r.command) entry.command = r.command
          if (r.backend) entry.backend = r.backend
          return entry
        })

      if (newRalphs.length > 0) {
        const newConfig = {
          ...config,
          ralphs: [...existing, ...newRalphs],
        }
        await handleSaveConfig(newConfig)
      }

      setSaveMsg(
        `Deployed "${template.name}" template (${template.ralphs.length} ralphs).`,
      )
      setShowTemplates(false)
    } catch (err: unknown) {
      setError(
        err instanceof Error ? err.message : 'Failed to deploy template',
      )
    } finally {
      setDeploying(null)
    }
  }

  async function handleDeployCustom() {
    if (!customName.trim()) return
    setDeploying(customName)
    setError(null)
    setSaveMsg(null)
    try {
      const taskFile = `${customName}-tasks.json`
      const stateFile = `.${customName}-state.json`

      const tasks = customPrompt.trim()
        ? [
            {
              id: `${customName}-task`,
              description: customPrompt.trim(),
              status: 'pending',
              role: 'implementer',
            },
          ]
        : []

      await deployRalph(owner, repo, {
        name: customName.trim(),
        task_file: taskFile,
        state_file: stateFile,
        tasks,
      })

      const existing = (config.ralphs as RalphEntry[] | undefined) ?? []
      const newConfig = {
        ...config,
        ralphs: [
          ...existing,
          {
            name: customName.trim(),
            task_file: taskFile,
            state_file: stateFile,
          },
        ],
      }
      await handleSaveConfig(newConfig)

      setSaveMsg(`Deployed custom ralph "${customName.trim()}".`)
      setCustomName('')
      setCustomPrompt('')
      setShowCustomForm(false)
    } catch (err: unknown) {
      setError(
        err instanceof Error ? err.message : 'Failed to deploy custom ralph',
      )
    } finally {
      setDeploying(null)
    }
  }

  function handleRemoveRalph(name: string) {
    const existing = (config.ralphs as RalphEntry[] | undefined) ?? []
    const newConfig = {
      ...config,
      ralphs: existing.filter((r) => r.name !== name),
    }
    void handleSaveConfig(newConfig)
  }

  return (
    <div className="gui-mode">
      {/* Active Ralphs */}
      <div className="gui-section">
        <h3>Active Ralphs</h3>
        {ralphs.length === 0 ? (
          <p className="muted">
            No ralphs configured. Deploy a template or create a custom ralph to
            get started.
          </p>
        ) : (
          <div className="ralph-grid">
            {ralphs.map((ralph) => (
              <div key={ralph.name} className="ralph-card card">
                <div className="ralph-card-header">
                  <span className="ralph-name">{ralph.name}</span>
                  <span className="ralph-status-badge">active</span>
                </div>
                <div className="ralph-card-body">
                  <div className="ralph-field">
                    <span className="ralph-field-label">Task file</span>
                    <span className="ralph-field-value">{ralph.task_file}</span>
                  </div>
                  <div className="ralph-field">
                    <span className="ralph-field-label">State file</span>
                    <span className="ralph-field-value">
                      {ralph.state_file}
                    </span>
                  </div>
                  {ralph.command && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Command</span>
                      <span className="ralph-field-value">
                        {ralph.command}
                      </span>
                    </div>
                  )}
                  {ralph.backend && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Backend</span>
                      <span className="ralph-field-value">
                        {ralph.backend}
                      </span>
                    </div>
                  )}
                  {ralph.branch && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Branch</span>
                      <span className="ralph-field-value">{ralph.branch}</span>
                    </div>
                  )}
                  {ralph.agent && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Agent</span>
                      <span className="ralph-field-value">{ralph.agent}</span>
                    </div>
                  )}
                  {ralph.validation_command && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Validation</span>
                      <span className="ralph-field-value">
                        {ralph.validation_command}
                      </span>
                    </div>
                  )}
                  {ralph.prompt_dir && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Prompt dir</span>
                      <span className="ralph-field-value">
                        {ralph.prompt_dir}
                      </span>
                    </div>
                  )}
                  {ralph.brute_mode && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Brute mode</span>
                      <span className="ralph-field-value">enabled</span>
                    </div>
                  )}
                  {ralph.reviewers && ralph.reviewers.length > 0 && (
                    <div className="ralph-field">
                      <span className="ralph-field-label">Reviewers</span>
                      <span className="ralph-field-value">
                        {ralph.reviewers.join(', ')}
                      </span>
                    </div>
                  )}
                </div>
                <div className="ralph-card-actions">
                  <button
                    className="btn btn-sm btn-danger"
                    onClick={() => handleRemoveRalph(ralph.name)}
                    disabled={saving}
                  >
                    Remove
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Deploy actions */}
      <div className="gui-section">
        <h3>Deploy</h3>
        <div className="deploy-actions">
          <button
            className="btn btn-primary"
            onClick={() => {
              setShowTemplates(!showTemplates)
              setShowCustomForm(false)
            }}
          >
            Deploy from Template
          </button>
          <button
            className="btn"
            onClick={() => {
              setShowCustomForm(!showCustomForm)
              setShowTemplates(false)
            }}
          >
            Add Custom Ralph
          </button>
        </div>
      </div>

      {/* Template picker */}
      {showTemplates && (
        <div className="gui-section">
          <h3>Available Templates</h3>
          {templates.length === 0 ? (
            <p className="muted">Loading templates…</p>
          ) : (
            <div className="template-list">
              {templates.map((template) => (
                <div key={template.id} className="template-card card">
                  <div className="template-header">
                    <h4>{template.name}</h4>
                    <button
                      className="btn btn-primary btn-sm"
                      onClick={() => void handleDeployTemplate(template)}
                      disabled={deploying !== null}
                    >
                      {deploying === template.id ? 'Deploying…' : 'Deploy'}
                    </button>
                  </div>
                  <p className="muted template-desc">{template.description}</p>
                  <div className="template-ralphs">
                    {template.ralphs.map((r) => (
                      <div key={r.name} className="template-ralph-item">
                        <span className="template-ralph-name">{r.name}</span>
                        <span className="muted">{r.description}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Custom ralph form */}
      {showCustomForm && (
        <div className="gui-section">
          <h3>Create Custom Ralph</h3>
          <div className="custom-ralph-form card">
            <div className="form-field">
              <label htmlFor="ralph-name">Name</label>
              <input
                id="ralph-name"
                type="text"
                className="form-input"
                placeholder="e.g. security-audit"
                value={customName}
                onChange={(e) => setCustomName(e.target.value)}
              />
            </div>
            <div className="form-field">
              <label htmlFor="ralph-prompt">
                Initial task prompt{' '}
                <span className="muted">(optional)</span>
              </label>
              <textarea
                id="ralph-prompt"
                className="form-textarea"
                placeholder="Describe what this ralph should do…"
                value={customPrompt}
                onChange={(e) => setCustomPrompt(e.target.value)}
                rows={4}
              />
            </div>
            <button
              className="btn btn-primary"
              onClick={() => void handleDeployCustom()}
              disabled={!customName.trim() || deploying !== null}
            >
              {deploying ? 'Deploying…' : 'Deploy Ralph'}
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
