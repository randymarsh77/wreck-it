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

export function logout(): void {
  clearToken()
}

export { getToken, clearToken }
