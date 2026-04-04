const API_BASE = import.meta.env.VITE_API_BASE_URL

export interface User {
  id: number
  login: string
  avatar_url: string
  name: string | null
}

export interface Installation {
  id: number
  account: {
    login: string
    avatar_url: string
    type: string
  }
  app_id: number
  target_type: string
  permissions: Record<string, string>
  events: string[]
}

export interface InstallationSettings {
  pulse_cron: string
  pulse_enabled: boolean
  events_enabled: boolean
}

export interface Repository {
  id: number
  name: string
  full_name: string
  owner: {
    login: string
    avatar_url: string
  }
  private: boolean
  html_url: string
  description: string | null
}

export interface RalphConfig {
  [key: string]: unknown
}

export interface RalphTemplateEntry {
  name: string
  task_file: string
  state_file: string
  description: string
  command?: string
  backend?: string
}

export interface RalphTemplate {
  id: string
  name: string
  description: string
  ralphs: RalphTemplateEntry[]
}

export interface RalphDeployRequest {
  name: string
  task_file: string
  state_file: string
  tasks?: unknown[]
  command?: string
  backend?: string
}

export interface RalphDeployResponse {
  status: string
  ralph: string
  task_file: string
  state_file: string
  tasks_count: number
}

export interface RalphTask {
  id: string
  description: string
  status?: string
  role?: string
  kind?: string
  phase?: number
  depends_on?: string[]
  priority?: number
  complexity?: number
  cooldown_seconds?: number
  last_attempt_at?: number
  precondition_prompt?: string
  acceptance_criteria?: string
  [key: string]: unknown
}

export interface RalphTasksResponse {
  tasks: RalphTask[]
  _sha: string | null
  _path: string
  _branch: string
}

export interface RalphStateResponse {
  state: Record<string, unknown>
  _sha: string | null
  _path: string
  _branch: string
}

export interface PlanRequest {
  goal: string
  ralph?: string
  model?: string
}

export interface PlanResponse {
  name: string
  tasks: RalphTask[]
}

export interface ModelInfo {
  id: string
}

interface AuthCallbackResponse {
  user: User
  token: string
}

class ApiError extends Error {
  status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

function getToken(): string | null {
  return localStorage.getItem('token')
}

function setToken(token: string): void {
  localStorage.setItem('token', token)
}

function clearToken(): void {
  localStorage.removeItem('token')
}

async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const token = getToken()
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...((options.headers as Record<string, string>) ?? {}),
  }
  if (token) {
    headers['Authorization'] = `Bearer ${token}`
  }

  const res = await fetch(`${API_BASE}${path}`, { ...options, headers })

  if (!res.ok) {
    const body = await res.text().catch(() => '')
    throw new ApiError(res.status, body || `Request failed: ${res.status}`)
  }

  return res.json() as Promise<T>
}

export function login(): void {
  const redirectUri = `${window.location.origin}/auth/callback`
  window.location.href = `${API_BASE}/api/portal/auth/login?redirect_uri=${encodeURIComponent(redirectUri)}`
}

export async function handleCallback(code: string): Promise<AuthCallbackResponse> {
  const redirectUri = `${window.location.origin}/auth/callback`
  const data = await request<AuthCallbackResponse>(
    `/api/portal/auth/callback?code=${encodeURIComponent(code)}&redirect_uri=${encodeURIComponent(redirectUri)}`,
  )
  setToken(data.token)
  return data
}

export async function getUser(): Promise<User> {
  return request<User>('/api/portal/auth/user')
}

export async function getInstallations(): Promise<Installation[]> {
  return request<Installation[]>('/api/portal/installations')
}

export interface PaginatedRepos {
  total_count: number
  repositories: Repository[]
}

export async function getInstallationRepos(
  installationId: number,
  page = 1,
  perPage = 30,
): Promise<PaginatedRepos> {
  return request<PaginatedRepos>(
    `/api/portal/installations/${installationId}/repos?page=${page}&per_page=${perPage}`,
  )
}

export async function getRepoConfig(owner: string, repo: string): Promise<RalphConfig> {
  return request<RalphConfig>(`/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/config`)
}

export async function updateRepoConfig(
  owner: string,
  repo: string,
  config: RalphConfig,
): Promise<RalphConfig> {
  return request<RalphConfig>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/config`,
    { method: 'PUT', body: JSON.stringify(config) },
  )
}

export async function getTemplates(): Promise<RalphTemplate[]> {
  return request<RalphTemplate[]>('/api/portal/templates')
}

export async function getIndividualRalphs(): Promise<RalphTemplateEntry[]> {
  return request<RalphTemplateEntry[]>('/api/portal/ralphs')
}

export async function deployRalph(
  owner: string,
  repo: string,
  deploy: RalphDeployRequest,
): Promise<RalphDeployResponse> {
  return request<RalphDeployResponse>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/deploy`,
    { method: 'POST', body: JSON.stringify(deploy) },
  )
}

export async function generatePlan(
  owner: string,
  repo: string,
  planRequest: PlanRequest,
): Promise<PlanResponse> {
  return request<PlanResponse>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/plan`,
    { method: 'POST', body: JSON.stringify(planRequest) },
  )
}

export async function getAvailableModels(
  owner: string,
  repo: string,
): Promise<ModelInfo[]> {
  const data = await request<{ models: ModelInfo[] }>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/models`,
  )
  return data.models
}

export async function getRalphTasks(
  owner: string,
  repo: string,
  name: string,
): Promise<RalphTasksResponse> {
  return request<RalphTasksResponse>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/${encodeURIComponent(name)}/tasks`,
  )
}

export async function updateRalphTasks(
  owner: string,
  repo: string,
  name: string,
  tasks: RalphTask[],
  sha: string | null,
): Promise<RalphTasksResponse> {
  return request<RalphTasksResponse>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/${encodeURIComponent(name)}/tasks`,
    { method: 'PUT', body: JSON.stringify({ tasks, _sha: sha }) },
  )
}

export async function getRalphState(
  owner: string,
  repo: string,
  name: string,
): Promise<RalphStateResponse> {
  return request<RalphStateResponse>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/${encodeURIComponent(name)}/state`,
  )
}

export async function updateRalphState(
  owner: string,
  repo: string,
  name: string,
  state: Record<string, unknown>,
  sha: string | null,
): Promise<RalphStateResponse> {
  return request<RalphStateResponse>(
    `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/${encodeURIComponent(name)}/state`,
    { method: 'PUT', body: JSON.stringify({ state, _sha: sha }) },
  )
}

export async function getInstallationSettings(
  installationId: number,
): Promise<InstallationSettings> {
  return request<InstallationSettings>(
    `/api/portal/installations/${installationId}/settings`,
  )
}

export async function updateInstallationSettings(
  installationId: number,
  settings: InstallationSettings,
): Promise<InstallationSettings> {
  return request<InstallationSettings>(
    `/api/portal/installations/${installationId}/settings`,
    { method: 'PUT', body: JSON.stringify(settings) },
  )
}

export interface ReinitializeResponse {
  status: string
  installation_id: number
  repos_registered: number
  repos_total: number
  github_total_count: number
}

export async function reinitializeInstallation(
  installationId: number,
): Promise<ReinitializeResponse> {
  return request<ReinitializeResponse>(
    `/api/portal/installations/${installationId}/reinitialize`,
    { method: 'POST' },
  )
}

// ---------------------------------------------------------------------------
// Agent (Durable Object) endpoints
// ---------------------------------------------------------------------------

export interface AgentExecutionState {
  status: 'Idle' | 'Running' | 'Paused'
  current_task_id: string | null
  iteration_count: number
  last_run_at: number | null
}

export interface AgentConfig {
  task_file: string
  state_file: string
  owner: string
  repo: string
}

export interface AgentRalphState {
  tasks: RalphTask[]
  execution: AgentExecutionState
  config: AgentConfig
}

export type AgentStateResponse =
  | AgentRalphState
  | { initialized: false }

export interface AgentRunResponse {
  result: 'task_started' | 'all_complete' | 'no_pending_tasks'
  task_id?: string
}

export interface AgentActionResponse {
  status: string
}

export interface AgentMigrateResponse {
  migrated: boolean
}

function agentBasePath(owner: string, repo: string, name: string): string {
  return `/api/portal/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/ralphs/${encodeURIComponent(name)}/agent`
}

export async function getAgentState(
  owner: string,
  repo: string,
  name: string,
): Promise<AgentStateResponse> {
  return request<AgentStateResponse>(
    `${agentBasePath(owner, repo, name)}/state`,
  )
}

export async function agentRun(
  owner: string,
  repo: string,
  name: string,
): Promise<AgentRunResponse> {
  return request<AgentRunResponse>(
    `${agentBasePath(owner, repo, name)}/run`,
    { method: 'POST' },
  )
}

export async function agentPause(
  owner: string,
  repo: string,
  name: string,
): Promise<AgentActionResponse> {
  return request<AgentActionResponse>(
    `${agentBasePath(owner, repo, name)}/pause`,
    { method: 'POST' },
  )
}

export async function agentResume(
  owner: string,
  repo: string,
  name: string,
): Promise<AgentActionResponse> {
  return request<AgentActionResponse>(
    `${agentBasePath(owner, repo, name)}/resume`,
    { method: 'POST' },
  )
}

export async function agentMigrate(
  owner: string,
  repo: string,
  name: string,
  state: AgentRalphState,
): Promise<AgentMigrateResponse> {
  return request<AgentMigrateResponse>(
    `${agentBasePath(owner, repo, name)}/migrate`,
    { method: 'POST', body: JSON.stringify(state) },
  )
}

export function logout(): void {
  clearToken()
}

export { getToken, clearToken }
