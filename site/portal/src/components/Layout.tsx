import { useState } from 'react'
import { NavLink, Outlet } from 'react-router-dom'
import { useAuth } from '../auth/useAuth'

export default function Layout() {
  const { user, logout } = useAuth()
  const [menuOpen, setMenuOpen] = useState(false)

  return (
    <div className="layout">
      <header className="topbar">
        <div className="topbar-left">
          <NavLink to="/" className="brand" onClick={() => setMenuOpen(false)}>
            wreck-it portal
          </NavLink>
          {user && (
            <button
              className="menu-toggle"
              onClick={() => setMenuOpen((v) => !v)}
              aria-label="Toggle navigation"
            >
              <span className={`hamburger ${menuOpen ? 'open' : ''}`} />
            </button>
          )}
          {user && (
            <nav className={`nav-links ${menuOpen ? 'nav-open' : ''}`}>
              <NavLink to="/" end onClick={() => setMenuOpen(false)}>
                Dashboard
              </NavLink>
              <NavLink to="/installations" onClick={() => setMenuOpen(false)}>
                Installations
              </NavLink>
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
