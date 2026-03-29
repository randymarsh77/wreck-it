import { useCallback, useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import type { User } from '../api/client'
import * as api from '../api/client'
import { AuthContext } from './authState'

export function AuthProvider({ children }: { children: ReactNode }) {
  const initialToken = api.getToken()
  const [user, setUser] = useState<User | null>(null)
  const [token, setToken] = useState<string | null>(initialToken)
  const [loading, setLoading] = useState(!!initialToken)

  useEffect(() => {
    if (!token) return

    api
      .getUser()
      .then((u) => {
        setUser(u)
      })
      .catch(() => {
        api.clearToken()
        setToken(null)
        setUser(null)
      })
      .finally(() => setLoading(false))
  }, [token])

  const login = useCallback(() => {
    api.login()
  }, [])

  const handleCallback = useCallback(async (code: string) => {
    const data = await api.handleCallback(code)
    setToken(data.token)
    setUser(data.user)
  }, [])

  const logout = useCallback(() => {
    api.logout()
    setToken(null)
    setUser(null)
  }, [])

  return (
    <AuthContext.Provider value={{ user, token, loading, login, handleCallback, logout }}>
      {children}
    </AuthContext.Provider>
  )
}
