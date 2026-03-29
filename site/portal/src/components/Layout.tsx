import { NavLink, Outlet } from 'react-router-dom'
import { useAuth } from '../auth/useAuth'

export default function Layout() {
  const { user, logout } = useAuth()

  return (
    <div className="layout">
      <header className="topbar">
        <div className="topbar-left">
          <NavLink to="/" className="brand">
            wreck-it portal
          </NavLink>
          {user && (
            <nav className="nav-links">
              <NavLink to="/" end>
                Dashboard
              </NavLink>
              <NavLink to="/installations">Installations</NavLink>
            </nav>
          )}
        </div>
        {user && (
          <div className="topbar-right">
            <img src={user.avatar_url} alt={user.login} className="avatar" />
            <span className="username">{user.login}</span>
            <button onClick={logout} className="btn btn-sm">
              Logout
            </button>
          </div>
        )}
      </header>
      <main className="content">
        <Outlet />
      </main>
    </div>
  )
}
