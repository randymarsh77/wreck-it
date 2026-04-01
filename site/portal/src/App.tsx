import { BrowserRouter, Route, Routes } from 'react-router-dom'
import { AuthProvider } from './auth/AuthContext'
import Layout from './components/Layout'
import ProtectedRoute from './components/ProtectedRoute'
import Login from './pages/Login'
import AuthCallback from './pages/AuthCallback'
import Dashboard from './pages/Dashboard'
import Installations from './pages/Installations'
import RepoConfig from './pages/RepoConfig'
import Plan from './pages/Plan'

export default function App() {
  return (
    <BrowserRouter>
      <AuthProvider>
        <Routes>
          <Route path="/login" element={<Login />} />
          <Route path="/auth/callback" element={<AuthCallback />} />
          <Route
            element={
              <ProtectedRoute>
                <Layout />
              </ProtectedRoute>
            }
          >
            <Route index element={<Dashboard />} />
            <Route path="installations" element={<Installations />} />
            <Route path="repos/:owner/:repo/config" element={<RepoConfig />} />
            <Route path="repos/:owner/:repo/plan" element={<Plan />} />
          </Route>
        </Routes>
      </AuthProvider>
    </BrowserRouter>
  )
}
