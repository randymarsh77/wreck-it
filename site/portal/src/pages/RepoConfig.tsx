import { useEffect, useState, useCallback } from 'react'
import { useParams, Link } from 'react-router-dom'
import {
  getRepoConfig,
  updateRepoConfig,
  getTemplates,
  deployRalph,
  getRalphTasks,
  updateRalphTasks,
  getRalphState,
  updateRalphState,
} from '../api/client'
import type {
  RalphConfig,
  RalphTemplate,
  RalphTemplateEntry,
  RalphTask,
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
      } catch (e: unknown) {
        const detail = e instanceof SyntaxError ? e.message : 'unknown error'
        setParseError(`Fix JSON errors before switching to GUI mode: ${detail}`)
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

      <div style={{ marginBottom: 16 }}>
        <Link to={`/repos/${owner}/${repo}/plan`} className="btn btn-primary btn-sm">
          ✦ Generate Plan
        </Link>
      </div>

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
  const [expandedRalph, setExpandedRalph] = useState<string | null>(null)
  const [expandedView, setExpandedView] = useState<'tasks' | 'state' | null>(null)

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
              id: `${customName}-task-${Date.now()}`,
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
                    className={`btn btn-sm ${expandedRalph === ralph.name && expandedView === 'tasks' ? 'btn-primary' : ''}`}
                    onClick={() => {
                      if (expandedRalph === ralph.name && expandedView === 'tasks') {
                        setExpandedRalph(null)
                        setExpandedView(null)
                      } else {
                        setExpandedRalph(ralph.name)
                        setExpandedView('tasks')
                      }
                    }}
                  >
                    Tasks
                  </button>
                  <button
                    className={`btn btn-sm ${expandedRalph === ralph.name && expandedView === 'state' ? 'btn-primary' : ''}`}
                    onClick={() => {
                      if (expandedRalph === ralph.name && expandedView === 'state') {
                        setExpandedRalph(null)
                        setExpandedView(null)
                      } else {
                        setExpandedRalph(ralph.name)
                        setExpandedView('state')
                      }
                    }}
                  >
                    State
                  </button>
                  <button
                    className="btn btn-sm btn-danger"
                    onClick={() => handleRemoveRalph(ralph.name)}
                    disabled={saving}
                  >
                    Remove
                  </button>
                </div>
                {expandedRalph === ralph.name && expandedView === 'tasks' && (
                  <RalphTasksPanel owner={owner} repo={repo} name={ralph.name} />
                )}
                {expandedRalph === ralph.name && expandedView === 'state' && (
                  <RalphStatePanel owner={owner} repo={repo} name={ralph.name} />
                )}
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

// ---------------------------------------------------------------------------
// Ralph Tasks Panel component
// ---------------------------------------------------------------------------

interface RalphPanelProps {
  owner: string
  repo: string
  name: string
}

function statusBadgeClass(status: string | undefined): string {
  switch (status) {
    case 'completed':
      return 'task-status-completed'
    case 'inprogress':
      return 'task-status-inprogress'
    case 'failed':
      return 'task-status-failed'
    default:
      return 'task-status-pending'
  }
}

function RalphTasksPanel({ owner, repo, name }: RalphPanelProps) {
  const [tasks, setTasks] = useState<RalphTask[]>([])
  const [sha, setSha] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [successMsg, setSuccessMsg] = useState<string | null>(null)
  const [editingTask, setEditingTask] = useState<string | null>(null)
  const [editDraft, setEditDraft] = useState('')
  const [showAddForm, setShowAddForm] = useState(false)
  const [newTaskId, setNewTaskId] = useState('')
  const [newTaskDesc, setNewTaskDesc] = useState('')
  const [newTaskRole, setNewTaskRole] = useState('')

  useEffect(() => {
    setLoading(true)
    setError(null)
    getRalphTasks(owner, repo, name)
      .then((data) => {
        setTasks(data.tasks)
        setSha(data._sha)
      })
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load tasks'),
      )
      .finally(() => setLoading(false))
  }, [owner, repo, name])

  async function saveTasks(newTasks: RalphTask[]) {
    setSaving(true)
    setError(null)
    setSuccessMsg(null)
    try {
      const result = await updateRalphTasks(owner, repo, name, newTasks, sha)
      setTasks(result.tasks)
      setSha(result._sha)
      setSuccessMsg('Tasks saved.')
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save tasks')
    } finally {
      setSaving(false)
    }
  }

  function handleDeleteTask(taskId: string) {
    const newTasks = tasks.filter((t) => t.id !== taskId)
    void saveTasks(newTasks)
  }

  function handleStatusChange(taskId: string, newStatus: string) {
    const newTasks = tasks.map((t) =>
      t.id === taskId ? { ...t, status: newStatus } : t,
    )
    void saveTasks(newTasks)
  }

  function handleStartEdit(task: RalphTask) {
    setEditingTask(task.id)
    setEditDraft(JSON.stringify(task, null, 2))
  }

  function handleSaveEdit() {
    if (!editingTask) return
    try {
      const parsed = JSON.parse(editDraft) as RalphTask
      const newTasks = tasks.map((t) => (t.id === editingTask ? parsed : t))
      setEditingTask(null)
      void saveTasks(newTasks)
    } catch {
      setError('Invalid JSON in task editor')
    }
  }

  function handleAddTask() {
    if (!newTaskId.trim() || !newTaskDesc.trim()) return
    const newTask: RalphTask = {
      id: newTaskId.trim(),
      description: newTaskDesc.trim(),
      status: 'pending',
      role: newTaskRole.trim() || 'implementer',
    }
    const newTasks = [...tasks, newTask]
    setNewTaskId('')
    setNewTaskDesc('')
    setNewTaskRole('')
    setShowAddForm(false)
    void saveTasks(newTasks)
  }

  if (loading) return <div className="ralph-panel"><p className="muted">Loading tasks…</p></div>

  const pending = tasks.filter((t) => !t.status || t.status === 'pending').length
  const completed = tasks.filter((t) => t.status === 'completed').length
  const inProgress = tasks.filter((t) => t.status === 'inprogress').length
  const failed = tasks.filter((t) => t.status === 'failed').length

  return (
    <div className="ralph-panel">
      <div className="ralph-panel-header">
        <h4>Tasks</h4>
        <div className="task-summary">
          {pending > 0 && <span className="task-count task-status-pending">{pending} pending</span>}
          {inProgress > 0 && <span className="task-count task-status-inprogress">{inProgress} in progress</span>}
          {completed > 0 && <span className="task-count task-status-completed">{completed} completed</span>}
          {failed > 0 && <span className="task-count task-status-failed">{failed} failed</span>}
        </div>
      </div>

      {error && <p className="error-text">{error}</p>}
      {successMsg && <p className="success-text">{successMsg}</p>}

      {tasks.length === 0 ? (
        <p className="muted">No tasks found.</p>
      ) : (
        <div className="task-list">
          {tasks.map((task) => (
            <div key={task.id} className="task-item">
              {editingTask === task.id ? (
                <div className="task-edit">
                  <textarea
                    className="config-editor task-editor"
                    value={editDraft}
                    onChange={(e) => setEditDraft(e.target.value)}
                    rows={10}
                  />
                  <div className="task-edit-actions">
                    <button className="btn btn-sm btn-primary" onClick={handleSaveEdit} disabled={saving}>
                      Save
                    </button>
                    <button className="btn btn-sm" onClick={() => setEditingTask(null)}>
                      Cancel
                    </button>
                  </div>
                </div>
              ) : (
                <>
                  <div className="task-item-header">
                    <span className="task-id">{task.id}</span>
                    <span className={`task-status-badge ${statusBadgeClass(task.status)}`}>
                      {task.status ?? 'pending'}
                    </span>
                  </div>
                  <p className="task-description">{task.description}</p>
                  {task.role && (
                    <span className="task-meta">Role: {task.role}</span>
                  )}
                  {task.kind && (
                    <span className="task-meta">Kind: {task.kind}</span>
                  )}
                  {task.priority !== undefined && (
                    <span className="task-meta">Priority: {task.priority}</span>
                  )}
                  <div className="task-item-actions">
                    <select
                      className="task-status-select"
                      value={task.status ?? 'pending'}
                      onChange={(e) => handleStatusChange(task.id, e.target.value)}
                      disabled={saving}
                    >
                      <option value="pending">pending</option>
                      <option value="inprogress">inprogress</option>
                      <option value="completed">completed</option>
                      <option value="failed">failed</option>
                    </select>
                    <button
                      className="btn btn-sm"
                      onClick={() => handleStartEdit(task)}
                      disabled={saving}
                    >
                      Edit
                    </button>
                    <button
                      className="btn btn-sm btn-danger"
                      onClick={() => handleDeleteTask(task.id)}
                      disabled={saving}
                    >
                      Delete
                    </button>
                  </div>
                </>
              )}
            </div>
          ))}
        </div>
      )}

      {showAddForm ? (
        <div className="task-add-form">
          <div className="form-field">
            <label htmlFor={`task-id-${name}`}>Task ID</label>
            <input
              id={`task-id-${name}`}
              type="text"
              className="form-input"
              placeholder="e.g. my-new-task"
              value={newTaskId}
              onChange={(e) => setNewTaskId(e.target.value)}
            />
          </div>
          <div className="form-field">
            <label htmlFor={`task-desc-${name}`}>Description</label>
            <textarea
              id={`task-desc-${name}`}
              className="form-textarea"
              placeholder="What should this task do?"
              value={newTaskDesc}
              onChange={(e) => setNewTaskDesc(e.target.value)}
              rows={3}
            />
          </div>
          <div className="form-field">
            <label htmlFor={`task-role-${name}`}>
              Role <span className="muted">(optional, default: implementer)</span>
            </label>
            <input
              id={`task-role-${name}`}
              type="text"
              className="form-input"
              placeholder="e.g. evaluator, ideas, implementer"
              value={newTaskRole}
              onChange={(e) => setNewTaskRole(e.target.value)}
            />
          </div>
          <div className="task-edit-actions">
            <button
              className="btn btn-sm btn-primary"
              onClick={handleAddTask}
              disabled={!newTaskId.trim() || !newTaskDesc.trim() || saving}
            >
              {saving ? 'Adding…' : 'Add Task'}
            </button>
            <button className="btn btn-sm" onClick={() => setShowAddForm(false)}>
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <button
          className="btn btn-sm"
          onClick={() => setShowAddForm(true)}
          disabled={saving}
          style={{ marginTop: '8px' }}
        >
          + Add Task
        </button>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Ralph State Panel component
// ---------------------------------------------------------------------------

function RalphStatePanel({ owner, repo, name }: RalphPanelProps) {
  const [state, setState] = useState<Record<string, unknown>>({})
  const [sha, setSha] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [successMsg, setSuccessMsg] = useState<string | null>(null)
  const [parseError, setParseError] = useState<string | null>(null)

  useEffect(() => {
    setLoading(true)
    setError(null)
    getRalphState(owner, repo, name)
      .then((data) => {
        setState(data.state)
        setSha(data._sha)
        setDraft(JSON.stringify(data.state, null, 2))
      })
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load state'),
      )
      .finally(() => setLoading(false))
  }, [owner, repo, name])

  function handleDraftChange(value: string) {
    setDraft(value)
    setSuccessMsg(null)
    try {
      JSON.parse(value)
      setParseError(null)
    } catch {
      setParseError('Invalid JSON')
    }
  }

  async function handleSave() {
    if (parseError) return
    setSaving(true)
    setError(null)
    setSuccessMsg(null)
    try {
      const parsed = JSON.parse(draft) as Record<string, unknown>
      const result = await updateRalphState(owner, repo, name, parsed, sha)
      setState(result.state)
      setSha(result._sha)
      setDraft(JSON.stringify(result.state, null, 2))
      setSuccessMsg('State saved.')
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save state')
    } finally {
      setSaving(false)
    }
  }

  if (loading) return <div className="ralph-panel"><p className="muted">Loading state…</p></div>

  const isEmpty = Object.keys(state).length === 0

  return (
    <div className="ralph-panel">
      <div className="ralph-panel-header">
        <h4>State</h4>
      </div>

      {error && <p className="error-text">{error}</p>}
      {successMsg && <p className="success-text">{successMsg}</p>}

      {isEmpty && !draft.trim() ? (
        <p className="muted">No state file found. Edit below to create one.</p>
      ) : null}

      <textarea
        className="config-editor state-editor"
        value={draft}
        onChange={(e) => handleDraftChange(e.target.value)}
        spellCheck={false}
        rows={12}
      />
      {parseError && <p className="error-text">{parseError}</p>}
      <button
        className="btn btn-sm btn-primary"
        onClick={() => void handleSave()}
        disabled={saving || !!parseError}
      >
        {saving ? 'Saving…' : 'Save State'}
      </button>
    </div>
  )
}
