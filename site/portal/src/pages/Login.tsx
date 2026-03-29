import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '../auth/useAuth'

export default function Login() {
  const { user, loading, login } = useAuth()
  const navigate = useNavigate()

  useEffect(() => {
    if (!loading && user) {
      navigate('/', { replace: true })
    }
  }, [user, loading, navigate])

  return (
    <div className="center-page">
      <div className="card login-card">
        <h1>wreck-it portal</h1>
        <p>
          Manage your wreck-it GitHub App installations and configure Ralph for
          your repositories.
        </p>
        <button onClick={login} className="btn btn-primary btn-lg">
          Sign in with GitHub
        </button>
      </div>
    </div>
  )
}
