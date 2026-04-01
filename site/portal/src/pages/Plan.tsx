import { useState } from 'react'
import { useParams, useNavigate, Link } from 'react-router-dom'
import { generatePlan, deployRalph, getRepoConfig, updateRepoConfig } from '../api/client'
import type { RalphTask, PlanResponse, RalphConfig } from '../api/client'

export default function Plan() {
  const { owner, repo } = useParams<{ owner: string; repo: string }>()
  const navigate = useNavigate()
  const [goal, setGoal] = useState('')
  const [ralphName, setRalphName] = useState('')
  const [generating, setGenerating] = useState(false)
  const [plan, setPlan] = useState<PlanResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [deploying, setDeploying] = useState(false)
  const [deployMsg, setDeployMsg] = useState<string | null>(null)

  async function handleGenerate(e: React.FormEvent) {
    e.preventDefault()
    if (!owner || !repo || !goal.trim()) return

    setGenerating(true)
    setError(null)
    setPlan(null)
    setDeployMsg(null)

    try {
      const result = await generatePlan(owner, repo, {
        goal: goal.trim(),
        ralph: ralphName.trim() || undefined,
      })
      setPlan(result)
      if (!ralphName.trim()) {
        setRalphName(result.name)
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Plan generation failed')
    } finally {
      setGenerating(false)
    }
  }

  function handleRemoveTask(index: number) {
    if (!plan) return
    setPlan({
      ...plan,
      tasks: plan.tasks.filter((_, i) => i !== index),
    })
  }

  function handleEditDescription(index: number, description: string) {
    if (!plan) return
    const updated = [...plan.tasks]
    updated[index] = { ...updated[index], description }
    setPlan({ ...plan, tasks: updated })
  }

  async function handleDeploy() {
    if (!owner || !repo || !plan || plan.tasks.length === 0) return

    setDeploying(true)
    setError(null)
    setDeployMsg(null)

    const name = ralphName.trim() || plan.name
    const taskFile = `${name}-tasks.json`
    const stateFile = `.${name}-state.json`

    try {
      await deployRalph(owner, repo, {
        name,
        task_file: taskFile,
        state_file: stateFile,
        tasks: plan.tasks,
      })

      // Also update the repo config to include the new ralph.
      try {
        const config: RalphConfig = await getRepoConfig(owner, repo)
        const ralphs = (config.ralphs as Array<Record<string, unknown>>) ?? []
        const existing = ralphs.find((r) => r.name === name)
        if (!existing) {
          ralphs.push({
            name,
            task_file: taskFile,
            state_file: stateFile,
          })
          await updateRepoConfig(owner, repo, { ...config, ralphs })
        }
      } catch {
        // Config update is best-effort; deploy succeeded.
      }

      setDeployMsg(`Ralph "${name}" deployed with ${plan.tasks.length} task(s).`)
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Deploy failed')
    } finally {
      setDeploying(false)
    }
  }

  if (!owner || !repo) return null

  return (
    <div className="plan-page">
      <div className="plan-breadcrumb">
        <Link to={`/repos/${owner}/${repo}/config`}>← {owner}/{repo}</Link>
      </div>

      <h1>Generate Plan</h1>
      <p className="muted" style={{ marginBottom: 24 }}>
        Describe your goal and the AI will generate a structured task plan.
      </p>

      {/* Goal input form */}
      <form onSubmit={handleGenerate} className="plan-form card">
        <div className="form-field">
          <label htmlFor="goal">Goal</label>
          <textarea
            id="goal"
            className="form-textarea"
            placeholder="Describe what you want to build or accomplish…"
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
            rows={4}
            disabled={generating}
          />
        </div>
        <div className="form-field">
          <label htmlFor="ralph-name">Ralph Name (optional)</label>
          <input
            id="ralph-name"
            className="form-input"
            placeholder="Auto-generated if left empty"
            value={ralphName}
            onChange={(e) => setRalphName(e.target.value)}
            disabled={generating}
          />
        </div>
        <button
          type="submit"
          className="btn btn-primary btn-lg"
          disabled={generating || !goal.trim()}
        >
          {generating ? 'Generating…' : 'Generate Plan'}
        </button>
      </form>

      {error && <p className="error-text" style={{ marginTop: 16 }}>{error}</p>}

      {/* Plan result */}
      {plan && (
        <div className="plan-result" style={{ marginTop: 24 }}>
          <div className="plan-result-header">
            <h2>
              Plan: <span style={{ color: 'var(--primary)' }}>{ralphName || plan.name}</span>
            </h2>
            <span className="muted">{plan.tasks.length} task(s)</span>
          </div>

          <div className="task-list" style={{ marginTop: 12 }}>
            {plan.tasks.map((task: RalphTask, i: number) => (
              <div key={task.id} className="task-item">
                <div className="task-item-header">
                  <span className="task-id">{task.id}</span>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                    {task.phase !== undefined && (
                      <span className="task-meta">phase {task.phase}</span>
                    )}
                    <button
                      className="btn btn-sm btn-danger"
                      onClick={() => handleRemoveTask(i)}
                      title="Remove task"
                    >
                      ✕
                    </button>
                  </div>
                </div>
                <input
                  className="form-input"
                  style={{ width: '100%', marginTop: 4 }}
                  value={task.description}
                  onChange={(e) => handleEditDescription(i, e.target.value)}
                />
                {task.depends_on && task.depends_on.length > 0 && (
                  <div className="task-meta" style={{ marginTop: 4 }}>
                    depends on: {task.depends_on.join(', ')}
                  </div>
                )}
              </div>
            ))}
          </div>

          <div style={{ marginTop: 16, display: 'flex', gap: 12, alignItems: 'center' }}>
            <button
              className="btn btn-primary btn-lg"
              onClick={handleDeploy}
              disabled={deploying || plan.tasks.length === 0}
            >
              {deploying ? 'Deploying…' : 'Deploy Plan'}
            </button>
            <button
              className="btn"
              onClick={() => navigate(`/repos/${owner}/${repo}/config`)}
            >
              Back to Config
            </button>
          </div>

          {deployMsg && <p className="success-text" style={{ marginTop: 12 }}>{deployMsg}</p>}
        </div>
      )}
    </div>
  )
}
