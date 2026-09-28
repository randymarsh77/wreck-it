import { useCallback, useEffect, useState } from 'react'
import {
  deleteSlackLink,
  getSlackChannels,
  getSlackInstallUrl,
  getSlackLinks,
  getSlackWorkspaces,
  putSlackLink,
} from '../api/client'
import type { SlackChannel, SlackLink, SlackTeam } from '../api/client'

interface Props {
  owner: string
  repo: string
}

export default function SlackPanel({ owner, repo }: Props) {
  const [teams, setTeams] = useState<SlackTeam[]>([])
  const [links, setLinks] = useState<SlackLink[]>([])
  const [selectedTeam, setSelectedTeam] = useState('')
  const [channels, setChannels] = useState<SlackChannel[]>([])
  const [selectedChannel, setSelectedChannel] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    getSlackWorkspaces()
      .then((t) => {
        setTeams(t)
        if (t.length > 0) setSelectedTeam((prev) => prev || t[0].team_id)
      })
      .catch(() => setTeams([]))
    getSlackLinks(owner, repo)
      .then(setLinks)
      .catch(() => setLinks([]))
  }, [owner, repo])

  useEffect(() => {
    load()
  }, [load])

  useEffect(() => {
    if (!selectedTeam) return
    getSlackChannels(selectedTeam)
      .then((c) => {
        setChannels(c)
        if (c.length > 0) setSelectedChannel((prev) => prev || c[0].id)
      })
      .catch((e) => setError(e instanceof Error ? e.message : 'Failed to load channels'))
  }, [selectedTeam])

  const install = async () => {
    setBusy(true)
    setError(null)
    try {
      const url = await getSlackInstallUrl(owner, repo)
      window.open(url, '_blank', 'noopener')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to start Slack install')
    } finally {
      setBusy(false)
    }
  }

  const linkChannel = async () => {
    if (!selectedTeam || !selectedChannel) return
    setBusy(true)
    setError(null)
    try {
      await putSlackLink(owner, repo, {
        team_id: selectedTeam,
        channel_id: selectedChannel,
        notify_triage: true,
        notify_pr: true,
        notify_security: true,
      })
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to link channel')
    } finally {
      setBusy(false)
    }
  }

  const unlink = async (link: SlackLink) => {
    setBusy(true)
    setError(null)
    try {
      await deleteSlackLink(owner, repo, link.team_id, link.channel_id)
      load()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to unlink channel')
    } finally {
      setBusy(false)
    }
  }

  const channelName = (link: SlackLink) =>
    channels.find((c) => c.id === link.channel_id)?.name ?? link.channel_id

  return (
    <div className="card slack-panel">
      <h2>Slack</h2>
      <p className="muted">
        Post triage updates to a channel and accept @wreck-it callouts. Updates
        for each item thread onto its announcement.
      </p>
      {error && <p className="error-text">{error}</p>}

      {teams.length === 0 ? (
        <button className="btn btn-primary btn-sm" onClick={() => void install()} disabled={busy}>
          Connect a Slack workspace
        </button>
      ) : (
        <>
          <div className="slack-link-row">
            <select
              className="task-status-select"
              value={selectedTeam}
              onChange={(e) => setSelectedTeam(e.target.value)}
            >
              {teams.map((t) => (
                <option key={t.team_id} value={t.team_id}>
                  {t.team_name}
                </option>
              ))}
            </select>
            <select
              className="task-status-select"
              value={selectedChannel}
              onChange={(e) => setSelectedChannel(e.target.value)}
            >
              {channels.map((c) => (
                <option key={c.id} value={c.id}>
                  #{c.name}
                </option>
              ))}
            </select>
            <button
              className="btn btn-primary btn-sm"
              onClick={() => void linkChannel()}
              disabled={busy || !selectedChannel}
            >
              Link channel
            </button>
            <button className="btn btn-sm" onClick={() => void install()} disabled={busy}>
              Add workspace
            </button>
          </div>

          {links.length > 0 && (
            <ul className="slack-links">
              {links.map((link) => (
                <li key={`${link.team_id}/${link.channel_id}`}>
                  <span>
                    #{channelName(link)}{' '}
                    <span className="muted">({link.team_id})</span>
                  </span>
                  <button
                    className="btn btn-sm btn-danger"
                    onClick={() => void unlink(link)}
                    disabled={busy}
                  >
                    Unlink
                  </button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  )
}
