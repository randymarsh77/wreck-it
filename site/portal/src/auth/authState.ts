import { createContext } from 'react'
import type { User } from '../api/client'

export interface AuthState {
  user: User | null
  token: string | null
  loading: boolean
  login: () => void
  handleCallback: (code: string) => Promise<void>
  logout: () => void
}

export const AuthContext = createContext<AuthState | undefined>(undefined)
