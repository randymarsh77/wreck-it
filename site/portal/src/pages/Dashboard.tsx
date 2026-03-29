import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { useAuth } from '../auth/useAuth'
import { getInstallations } from '../api/client'
import type { Installation } from '../api/client'

export default function Dashboard() {
  const { user } = useAuth()
  const [installations, setInstallations] = useState<Installation[]>([])
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    getInstallations()
      .then(setInstallations)
      .catch(() => setInstallations([]))
      .finally(() => setLoading(false))
  }, [])

  if (!user) return null

  return (
    <div className="dashboard">
      <div className="welcome">
        <img src={user.avatar_url} alt={user.login} className="avatar-lg" />
        <div>
          <h1>Welcome, {user.name ?? user.login}</h1>
          <p className="muted">Manage your wreck-it installations and Ralph configurations.</p>
        </div>
      </div>

      <div className="stats-grid">
        <div className="card stat-card">
          <h3>{loading ? '…' : installations.length}</h3>
          <p>Installations</p>
        </div>
        <div className="card stat-card">
          <Link to="/installations" className="btn btn-primary">
            View Installations
          </Link>
        </div>
      </div>
    </div>
  )
}
