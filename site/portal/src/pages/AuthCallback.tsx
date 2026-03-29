import { useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { useAuth } from '../auth/useAuth'

export default function AuthCallback() {
  const [searchParams] = useSearchParams()
  const { handleCallback } = useAuth()
  const navigate = useNavigate()
  const [error, setError] = useState<string | null>(null)
  const calledRef = useRef(false)

  const code = searchParams.get('code')

  useEffect(() => {
    if (calledRef.current || !code) return
    calledRef.current = true

    handleCallback(code)
      .then(() => navigate('/', { replace: true }))
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : 'Authentication failed.')
      })
  }, [code, handleCallback, navigate])

  if (!code) {
    return (
      <div className="center-page">
        <div className="card">
          <h2>Authentication Error</h2>
          <p className="error-text">Missing authorization code.</p>
          <a href="/login" className="btn btn-primary">
            Try Again
          </a>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="center-page">
        <div className="card">
          <h2>Authentication Error</h2>
          <p className="error-text">{error}</p>
          <a href="/login" className="btn btn-primary">
            Try Again
          </a>
        </div>
      </div>
    )
  }

  return (
    <div className="center-page">
      <div className="card">
        <p>Signing you in…</p>
      </div>
    </div>
  )
}
