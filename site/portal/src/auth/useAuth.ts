import { useContext } from 'react'
import { AuthContext } from './authState'
import type { AuthState } from './authState'

export function useAuth(): AuthState {
  const ctx = useContext(AuthContext)
  if (!ctx) {
    throw new Error('useAuth must be used within an AuthProvider')
  }
  return ctx
}
