import { useEffect, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { motion } from 'framer-motion'
import Particles, { initParticlesEngine } from '@tsparticles/react'
import { loadStarsPreset } from '@tsparticles/preset-stars'
import appIcon from './assets/app-icon.ico'
import steamLogo from './assets/steam-logo.png'
import epicLogo from './assets/epic-logo.webp'
import battlenetLogo from './assets/battlenet-logo.png'
import { GameIcon } from './components/GameIcon'
import { RobotBuddy } from './components/RobotBuddy'
import './App.css'

const SOURCE_VISUALS: Record<string, { logo: string | null; accent: string }> = {
  steam: { logo: steamLogo, accent: '#66c0f4' },
  epic: { logo: epicLogo, accent: '#f2f2f2' },
  battlenet: { logo: battlenetLogo, accent: '#1da1f2' },
  gog: { logo: null, accent: '#7c3aed' },
  local: { logo: null, accent: '#9ca3af' },
  all: { logo: null, accent: '#20bf6b' },
}

type DetectedGame = {
  appId: string
  detectedTitle: string
  store: string
  source: string
  trustedLauncherMatch: boolean
  libraryPath: string
  installDir: string
  manifestPath: string
}

type MatchResult = {
  candidateAppId: string
  candidateTitle: string
  matchedTitle: string | null
  confidence: number
  decision: 'auto_accept' | 'needs_confirmation' | 'unknown' | string
  provider: string
  reason: string
}

type ReviewedGame = {
  candidateAppId: string
  detectedTitle: string
  store: string
  source: string
  installDir: string
  installDirExists: boolean
  status: 'accepted' | 'rejected' | string
  savePath: string | null
  reviewedAt: number
}

type ScanSourceGroup = {
  sourceKey: string
  sourceLabel: string
  candidates: DetectedGame[]
  matches: MatchResult[]
}

type ScanGamesResponse = {
  sources: ScanSourceGroup[]
  totalCandidates: number
  totalAutoAccepted: number
}

type SavePathSuggestion = {
  path: string
  source: string
}

type RunningGameStatus = {
  candidateAppId: string
  running: boolean
  processName: string | null
  processPath: string | null
}

type BackupEntry = {
  id: number
  candidateAppId: string
  createdAt: number
  backupPath: string
  triggerReason: string
  fileCount: number
  totalBytes: number
}

type BackupCycleResponse = {
  created: BackupEntry[]
}

type GameReviewRequest = {
  candidate: DetectedGame
  accepted: boolean
  savePath: string | null
}

function formatError(error: unknown): string {
  if (typeof error === 'string') return error
  if (error && typeof error === 'object' && 'message' in error) {
    const maybeMessage = (error as { message?: unknown }).message
    if (typeof maybeMessage === 'string' && maybeMessage.length > 0) return maybeMessage
  }
  return 'Unknown error'
}

function getVolume(path: string): string {
  const normalized = path.trim()
  if (normalized.length >= 2 && normalized[1] === ':') {
    return normalized.slice(0, 2)
  }
  return 'Unknown'
}

function formatTimestamp(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleString()
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let size = bytes
  let idx = 0
  while (size >= 1024 && idx < units.length - 1) {
    size /= 1024
    idx += 1
  }
  return `${size.toFixed(size >= 10 || idx === 0 ? 0 : 1)} ${units[idx]}`
}

function toCandidateFromReviewed(reviewed: ReviewedGame): DetectedGame {
  return {
    appId: reviewed.candidateAppId,
    detectedTitle: reviewed.detectedTitle,
    store: reviewed.store,
    source: reviewed.source,
    trustedLauncherMatch: false,
    libraryPath: '',
    installDir: reviewed.installDir,
    manifestPath: '',
  }
}

async function selectSaveFolder(
  currentPath: string,
  onSelect: (path: string) => void,
  onError?: (message: string) => void
): Promise<void> {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      defaultPath: currentPath || undefined,
    })
    if (selected && typeof selected === 'string') {
      onSelect(selected)
    }
  } catch (error) {
    onError?.(formatError(error))
  }
}

function App() {
  const [activeTab, setActiveTab] = useState<'add' | 'checkpoint'>('add')
  const [checkpointView, setCheckpointView] = useState<'grid' | 'list' | 'compact'>('grid')
  const [particlesReady, setParticlesReady] = useState(false)
  const [hasGrantedScan, setHasGrantedScan] = useState(false)
  const [scanLoading, setScanLoading] = useState(false)
  const [sourceGroups, setSourceGroups] = useState<ScanSourceGroup[]>([])
  const [expandedByGroup, setExpandedByGroup] = useState<Record<string, boolean>>({ all: true })
  const [reviewedById, setReviewedById] = useState<Record<string, ReviewedGame>>({})
  const [reviewLoadingById, setReviewLoadingById] = useState<Record<string, boolean>>({})
  const [savePathDraftById, setSavePathDraftById] = useState<Record<string, string>>({})
  const [runningById, setRunningById] = useState<Record<string, RunningGameStatus>>({})
  const [backupsByGameId, setBackupsByGameId] = useState<Record<string, BackupEntry[]>>({})
  const [backupBusyByGameId, setBackupBusyByGameId] = useState<Record<string, boolean>>({})
  const requestedBackupsRef = useRef<Record<string, boolean>>({})
  const [hoveredGameId, setHoveredGameId] = useState<string | null>(null)
  const [scanMessage, setScanMessage] = useState('Start by allowing a scan.')
  const [addModal, setAddModal] = useState<{
    candidate: DetectedGame
    matchedTitle: string | null
    suggestions: SavePathSuggestion[]
    pathInput: string
    anchorOffset: number
  } | null>(null)

  const runScan = async () => {
    setScanLoading(true)
    setScanMessage('Scanning launchers first, then local folders...')
    try {
      const result = await invoke<ScanGamesResponse>('scan_games')
      setSourceGroups(result.sources)
      const nextExpanded: Record<string, boolean> = { all: true }
      for (const group of result.sources) nextExpanded[group.sourceKey] = false
      setExpandedByGroup(nextExpanded)

      const availableToAdd = result.sources.reduce((count, group) => {
        return (
          count +
          group.candidates.filter((candidate) => reviewedById[candidate.appId]?.status !== 'accepted').length
        )
      }, 0)
      const message = `Found ${availableToAdd} game(s). Select games you want to CheckPoint!`
      setScanMessage(message)
    } catch (error) {
      setSourceGroups([])
      setScanMessage(`Scan failed: ${formatError(error)}`)
    } finally {
      setScanLoading(false)
    }
  }

  const beginDetectionFlow = async () => {
    setHasGrantedScan(true)
    await runScan()
  }

  const loadReviewedGames = async () => {
    try {
      const reviewed = await invoke<ReviewedGame[]>('list_reviewed_games')
      const map: Record<string, ReviewedGame> = {}
      for (const item of reviewed) map[item.candidateAppId] = item
      setReviewedById(map)
      setSavePathDraftById((prev) => {
        const next = { ...prev }
        for (const item of reviewed) {
          next[item.candidateAppId] = item.savePath ?? ''
        }
        return next
      })
    } catch (error) {
      setScanMessage(`Warning: unable to load reviewed games (${formatError(error)})`)
    }
  }

  useEffect(() => {
    void initParticlesEngine(async (engine) => {
      await loadStarsPreset(engine)
    }).then(() => setParticlesReady(true))
  }, [])

  useEffect(() => {
    void loadReviewedGames()
  }, [])

  useEffect(() => {
    const pollRunningStatuses = async () => {
      try {
        const items = await invoke<RunningGameStatus[]>('get_running_game_statuses')
        const map: Record<string, RunningGameStatus> = {}
        for (const item of items) {
          map[item.candidateAppId] = item
        }
        setRunningById(map)
      } catch {
        // Keep UI resilient if polling fails.
      }
    }

    void pollRunningStatuses()
    const timer = window.setInterval(() => {
      void pollRunningStatuses()
    }, 4000)

    return () => window.clearInterval(timer)
  }, [])

  const reviewCandidate = async (candidate: DetectedGame, accepted: boolean, savePath: string | null) => {
    const reviewRequest: GameReviewRequest = {
      candidate,
      accepted,
      savePath,
    }
    setReviewLoadingById((prev) => ({ ...prev, [candidate.appId]: true }))
    try {
      const reviewed = await invoke<ReviewedGame>('review_game_candidate', { review: reviewRequest })
      setReviewedById((prev) => ({ ...prev, [reviewed.candidateAppId]: reviewed }))
      setSavePathDraftById((prev) => ({ ...prev, [reviewed.candidateAppId]: reviewed.savePath ?? '' }))
    } catch (error) {
      setScanMessage(`Update failed for ${candidate.detectedTitle}: ${formatError(error)}`)
    } finally {
      setReviewLoadingById((prev) => ({ ...prev, [candidate.appId]: false }))
    }
  }

  const openAddModal = async (
    candidate: DetectedGame,
    matchedTitle: string | null,
    triggerEl?: HTMLButtonElement | null
  ) => {
    setReviewLoadingById((prev) => ({ ...prev, [candidate.appId]: true }))
    try {
      const suggestions = await invoke<SavePathSuggestion[]>('suggest_save_paths', {
        candidate,
        matchedTitle,
      })
      const existing = reviewedById[candidate.appId]?.savePath ?? ''
      const triggerTop = triggerEl?.getBoundingClientRect().top ?? window.innerHeight * 0.22
      const anchorOffset = Math.max(14, Math.min(triggerTop - 70, window.innerHeight * 0.34))
      setAddModal({
        candidate,
        matchedTitle,
        suggestions,
        pathInput: existing || suggestions[0]?.path || '',
        anchorOffset,
      })
    } catch (error) {
      setScanMessage(`Unable to suggest save path for ${candidate.detectedTitle}: ${formatError(error)}`)
    } finally {
      setReviewLoadingById((prev) => ({ ...prev, [candidate.appId]: false }))
    }
  }

  const confirmAddFromModal = async () => {
    if (!addModal) return
    const chosenPath = addModal.pathInput.trim()
    if (!chosenPath) {
      setScanMessage(`Please confirm save location before adding ${addModal.candidate.detectedTitle}.`)
      return
    }
    await reviewCandidate(addModal.candidate, true, chosenPath)
    setAddModal(null)
  }

  const updateTrackedSavePath = async (reviewed: ReviewedGame) => {
    const draft = (savePathDraftById[reviewed.candidateAppId] ?? '').trim()
    if (!draft) {
      setScanMessage(`Please enter a save path for ${reviewed.detectedTitle}.`)
      return
    }
    setReviewLoadingById((prev) => ({ ...prev, [reviewed.candidateAppId]: true }))
    try {
      const updated = await invoke<ReviewedGame>('set_save_location', {
        candidateAppId: reviewed.candidateAppId,
        savePath: draft,
      })
      setReviewedById((prev) => ({ ...prev, [updated.candidateAppId]: updated }))
      setSavePathDraftById((prev) => ({ ...prev, [updated.candidateAppId]: updated.savePath ?? '' }))
    } catch (error) {
      setScanMessage(`Could not update save path for ${reviewed.detectedTitle}: ${formatError(error)}`)
    } finally {
      setReviewLoadingById((prev) => ({ ...prev, [reviewed.candidateAppId]: false }))
    }
  }

  const openInExplorer = async (path: string) => {
    try {
      await invoke('open_in_file_explorer', { path })
    } catch (error) {
      setScanMessage(`Could not open folder: ${formatError(error)}`)
    }
  }

  const loadBackupsForGame = async (candidateAppId: string) => {
    try {
      const backups = await invoke<BackupEntry[]>('list_backups', { candidateAppId })
      setBackupsByGameId((prev) => ({ ...prev, [candidateAppId]: backups }))
    } catch (error) {
      setScanMessage(`Could not load backups: ${formatError(error)}`)
    }
  }

  const createBackupNow = async (game: ReviewedGame) => {
    setBackupBusyByGameId((prev) => ({ ...prev, [game.candidateAppId]: true }))
    try {
      const backup = await invoke<BackupEntry>('create_backup_now', { candidateAppId: game.candidateAppId })
      setBackupsByGameId((prev) => ({
        ...prev,
        [game.candidateAppId]: [backup, ...(prev[game.candidateAppId] ?? [])],
      }))
      setScanMessage(`Backup created for ${game.detectedTitle} at ${formatTimestamp(backup.createdAt)}.`)
    } catch (error) {
      setScanMessage(`Backup failed for ${game.detectedTitle}: ${formatError(error)}`)
    } finally {
      setBackupBusyByGameId((prev) => ({ ...prev, [game.candidateAppId]: false }))
    }
  }

  const restoreBackupById = async (game: ReviewedGame, backupId: number) => {
    setBackupBusyByGameId((prev) => ({ ...prev, [game.candidateAppId]: true }))
    try {
      await invoke('restore_backup', { backupId })
      setScanMessage(`Restore completed for ${game.detectedTitle}.`)
    } catch (error) {
      setScanMessage(`Restore failed for ${game.detectedTitle}: ${formatError(error)}`)
    } finally {
      setBackupBusyByGameId((prev) => ({ ...prev, [game.candidateAppId]: false }))
    }
  }

  const removeFromCheckpoint = async (reviewed: ReviewedGame) => {
    await reviewCandidate(toCandidateFromReviewed(reviewed), false, null)
  }

  const displayGroups = useMemo<ScanSourceGroup[]>(() => {
    if (sourceGroups.length === 0) return []
    const acceptedIds = new Set(
      Object.values(reviewedById)
        .filter((item) => item.status === 'accepted')
        .map((item) => item.candidateAppId)
    )
    const filteredGroups = sourceGroups.map((group) => {
      const candidates = group.candidates.filter((candidate) => !acceptedIds.has(candidate.appId))
      const candidateIds = new Set(candidates.map((item) => item.appId))
      const matches = group.matches.filter((match) => candidateIds.has(match.candidateAppId))
      return { ...group, candidates, matches }
    })
    const allCandidates = filteredGroups.flatMap((group) => group.candidates)
    const allMatches = filteredGroups.flatMap((group) => group.matches)
    return [
      {
        sourceKey: 'all',
        sourceLabel: 'All Games',
        candidates: allCandidates,
        matches: allMatches,
      },
      ...filteredGroups,
    ]
  }, [sourceGroups, reviewedById])

  const toggleGroup = (key: string) => {
    setExpandedByGroup((prev) => ({ ...prev, [key]: !prev[key] }))
  }

  const trackedGames = useMemo(
    () => Object.values(reviewedById).filter((item) => item.status === 'accepted'),
    [reviewedById]
  )

  useEffect(() => {
    for (const game of trackedGames) {
      if (!requestedBackupsRef.current[game.candidateAppId]) {
        requestedBackupsRef.current[game.candidateAppId] = true
        void loadBackupsForGame(game.candidateAppId)
      }
    }
    const trackedIds = new Set(trackedGames.map((item) => item.candidateAppId))
    for (const key of Object.keys(requestedBackupsRef.current)) {
      if (!trackedIds.has(key)) delete requestedBackupsRef.current[key]
    }
    setBackupsByGameId((prev) => {
      const next: Record<string, BackupEntry[]> = {}
      for (const [key, value] of Object.entries(prev)) {
        if (trackedIds.has(key)) next[key] = value
      }
      return next
    })
  }, [trackedGames])

  useEffect(() => {
    const runCycle = async () => {
      try {
        const response = await invoke<BackupCycleResponse>('run_backup_cycle')
        if (response.created.length > 0) {
          setBackupsByGameId((prev) => {
            const next = { ...prev }
            for (const backup of response.created) {
              next[backup.candidateAppId] = [backup, ...(next[backup.candidateAppId] ?? [])]
            }
            return next
          })
          setScanMessage(`Auto backup created for ${response.created.length} game(s).`)
        }
      } catch {
        // keep silent in polling loop
      }
    }

    void runCycle()
    const timer = window.setInterval(() => {
      void runCycle()
    }, 5000)
    return () => window.clearInterval(timer)
  }, [])

  return (
    <main className="app">
      {particlesReady && (
        <Particles
          id="checkpoint-particles"
          className="particlesLayer"
          options={{
            preset: 'stars',
            background: { color: { value: 'transparent' } },
            particles: {
              color: { value: '#20bf6b' },
              opacity: { value: 0.35 },
              size: { value: { min: 1, max: 2 } },
              move: { speed: 0.5 },
            },
          }}
        />
      )}

      <div className="rocketLayer" aria-hidden="true">
        <span className="rocket rocketA" />
        <span className="rocket rocketB" />
      </div>

      <header className="header">
        <img src={appIcon} className="appIcon" alt="CheckPoint icon" />
        <div>
          <h1>CheckPoint</h1>
          <p className="subtitle">Welcome. Let&apos;s find your games and add them to CheckPoint.</p>
        </div>
      </header>

      <div className="vhs-overlay" aria-hidden="true" />

      {!hasGrantedScan ? (
        <section className="welcomeCard">
          <p className="welcomeLead">Hi, I am your CheckPoint bot 🤖</p>
          <p className="welcomeText">
            Allow a safe local scan to detect launchers and installed games on your PC.
          </p>
          <button className="primary secondary" onClick={() => void beginDetectionFlow()} disabled={scanLoading}>
            {scanLoading ? 'Scanning...' : 'Let&apos;s start finding games'}
          </button>
        </section>
      ) : (
        <>
          <div className="topTabs">
            <button
              className={`topTab ${activeTab === 'add' ? 'active' : ''}`}
              onClick={() => setActiveTab('add')}
            >
              Add Games
            </button>
            <button
              className={`topTab ${activeTab === 'checkpoint' ? 'active' : ''}`}
              onClick={() => setActiveTab('checkpoint')}
            >
              CheckPoint
            </button>
          </div>

          <div className="actions">
            <button
              className="primary secondary"
              onClick={() => void runScan()}
              disabled={scanLoading || activeTab !== 'add'}
            >
              {scanLoading ? 'Scanning...' : 'Rescan games on this PC'}
            </button>
          </div>

          <div className="status">
            <strong>Scan:</strong> <span>{scanMessage}</span>
          </div>

          {activeTab === 'add' ? (
            <section className="categoryList" aria-label="Game source categories">
              {displayGroups.map((group) => {
                const visuals = SOURCE_VISUALS[group.sourceKey] ?? { logo: null, accent: '#9ca3af' }
                const expanded = !!expandedByGroup[group.sourceKey]
                return (
                  <section key={group.sourceKey} className="sourceSection expanded">
                    <button
                      className={`sourceHeader ${expanded ? 'open' : ''}`}
                      style={{ borderColor: visuals.accent }}
                      onClick={() => toggleGroup(group.sourceKey)}
                      aria-expanded={expanded}
                      aria-controls={`source-list-${group.sourceKey}`}
                    >
                      <span className="sourceArrow">{expanded ? '▼' : '▶'}</span>
                      {visuals.logo ? (
                        <img src={visuals.logo} className="launcherLogo" alt={`${group.sourceLabel} logo`} />
                      ) : (
                        <div className="launcherFallback">{group.sourceLabel.slice(0, 2).toUpperCase()}</div>
                      )}
                      <span className="sourceTitle">
                        {group.sourceLabel} ({group.candidates.length})
                      </span>
                    </button>

                    {expanded && (
                      <section id={`source-list-${group.sourceKey}`}>
                        <ul className="games games-list">
                          {group.candidates.map((game) => {
                            const match = group.matches.find((item) => item.candidateAppId === game.appId)
                            const reviewed = reviewedById[game.appId]
                            return (
                              <motion.li
                                key={`${group.sourceKey}:${game.appId}`}
                                className="game game-list"
                                whileHover={{ scale: 1.01, x: 3 }}
                                onHoverStart={() => setHoveredGameId(game.appId)}
                                onHoverEnd={() =>
                                  setHoveredGameId((prev) => (prev === game.appId ? null : prev))
                                }
                              >
                                <div className="gameTop gameTop-list">
                                  <GameIcon
                                    appId={game.appId}
                                    store={game.store}
                                    installDir={game.installDir}
                                    detectedTitle={game.detectedTitle}
                                    matchedTitle={match?.matchedTitle}
                                    matchedProvider={match?.provider}
                                    size={64}
                                  />
                                  <div className="gameInfo">
                                    <RobotBuddy isHovered={hoveredGameId === game.appId} />
                                    <h3>{game.detectedTitle}</h3>
                                    <p>
                                      <strong>AppID:</strong> {game.appId}
                                    </p>
                                    <p className="installDir" title={game.installDir}>
                                      <strong>Install Dir:</strong> {game.installDir}
                                    </p>
                                    {reviewed?.status === 'accepted' ? (
                                      <p className="reviewState accepted">Added to CheckPoint tracking.</p>
                                    ) : reviewed?.status === 'rejected' ? (
                                      <p className="reviewState rejected">Removed from CheckPoint tracking.</p>
                                    ) : (
                                      <p className="reviewState pending">Not added to CheckPoint yet.</p>
                                    )}
                                    {match ? (
                                      <p className="matchLine">
                                        Match: {match.matchedTitle ?? 'Unknown'} ({match.provider})
                                      </p>
                                    ) : null}
                                  </div>
                                </div>
                                <div className="reviewButtons">
                                  <button
                                    className="miniBtn acceptBtn"
                                    disabled={!!reviewLoadingById[game.appId]}
                                    onClick={(event) =>
                                      void openAddModal(game, match?.matchedTitle ?? null, event.currentTarget)
                                    }
                                  >
                                    {reviewLoadingById[game.appId] ? 'Loading...' : 'Add'}
                                  </button>
                                </div>
                              </motion.li>
                            )
                          })}
                        </ul>
                      </section>
                    )}
                  </section>
                )
              })}
            </section>
          ) : (
            <section className="checkpointTab">
              <h2>Tracked Games ({trackedGames.length})</h2>
              <div className="viewToggles checkpointViewBar">
                <button
                  className={`viewBtn ${checkpointView === 'grid' ? 'active' : ''}`}
                  onClick={() => setCheckpointView('grid')}
                >
                  Grid
                </button>
                <button
                  className={`viewBtn ${checkpointView === 'list' ? 'active' : ''}`}
                  onClick={() => setCheckpointView('list')}
                >
                  List
                </button>
                <button
                  className={`viewBtn ${checkpointView === 'compact' ? 'active' : ''}`}
                  onClick={() => setCheckpointView('compact')}
                >
                  Compact
                </button>
              </div>
              {trackedGames.length === 0 ? (
                <p className="hint">No games added yet. Go to Add Games and add titles first.</p>
              ) : (
                <ul className={`games checkpointGames checkpointGames-${checkpointView}`}>
                  {trackedGames.map((game) => (
                    (() => {
                      const missingSavePath = !game.savePath?.trim()
                      const missingInstallDir = !game.installDirExists
                      const healthClass = missingInstallDir
                        ? 'checkpointGame-error'
                        : missingSavePath
                          ? 'checkpointGame-warning'
                          : 'checkpointGame-ok'
                      return (
                    <li
                      key={game.candidateAppId}
                      className={`game checkpointGame checkpointGame-${checkpointView} ${healthClass}`}
                    >
                      <div className={`gameTop checkpointTop checkpointTop-${checkpointView}`}>
                        <GameIcon
                          appId={game.candidateAppId}
                          store={game.store}
                          installDir={game.installDir}
                          detectedTitle={game.detectedTitle}
                          size={checkpointView === 'grid' ? 96 : checkpointView === 'compact' ? 72 : 80}
                        />
                        <div className="gameInfo">
                          <h3>{game.detectedTitle}</h3>
                          <p
                            className={`runningState ${
                              runningById[game.candidateAppId]?.running ? 'running' : 'notRunning'
                            }`}
                          >
                            {runningById[game.candidateAppId]?.running ? 'Running now' : 'Not running'}
                          </p>
                          <p>
                            <strong>Launcher:</strong> {game.store}
                          </p>
                          <p>
                            <strong>App ID:</strong> {game.candidateAppId}
                          </p>
                          <p className="installDir" title={game.installDir}>
                            <strong>Install Dir:</strong> {game.installDir}
                          </p>
                          <p>
                            <strong>Volume:</strong> {getVolume(game.installDir)}
                          </p>
                          {missingInstallDir ? (
                            <p className="healthAlert error">
                              Install directory is missing. Check install directory or remove this game and rescan.
                            </p>
                          ) : null}
                          {missingSavePath ? (
                            <p className="reviewState pending">Save path missing. Please set one now.</p>
                          ) : null}
                        </div>
                      </div>
                      <div className="checkpointSaveEditor">
                        <label htmlFor={`save-path-${game.candidateAppId}`}>Save location</label>
                        <div className="savePathRow">
                          <input
                            id={`save-path-${game.candidateAppId}`}
                            className="savePathInput"
                            value={savePathDraftById[game.candidateAppId] ?? game.savePath ?? ''}
                            onChange={(event) =>
                              setSavePathDraftById((prev) => ({
                                ...prev,
                                [game.candidateAppId]: event.target.value,
                              }))
                            }
                            placeholder="C:\\Users\\<you>\\Documents\\My Games\\..."
                          />
                          <button
                            className="miniBtn secondary"
                            onClick={() =>
                              void selectSaveFolder(
                                savePathDraftById[game.candidateAppId] ?? game.savePath ?? '',
                                (newPath) =>
                                  setSavePathDraftById((prev) => ({
                                    ...prev,
                                    [game.candidateAppId]: newPath,
                                  })),
                                (message) => setScanMessage(`Browse failed: ${message}`)
                              )
                            }
                          >
                            Browse
                          </button>
                        </div>
                        <div className="reviewButtons">
                          <button
                            className="miniBtn acceptBtn"
                            onClick={() => void updateTrackedSavePath(game)}
                            disabled={!!reviewLoadingById[game.candidateAppId]}
                          >
                            {reviewLoadingById[game.candidateAppId] ? 'Saving...' : 'Save Path'}
                          </button>
                          <button
                            className="miniBtn secondary"
                            onClick={() => void openInExplorer(game.installDir)}
                            disabled={missingInstallDir}
                          >
                            Open Install Folder
                          </button>
                          <button
                            className="miniBtn secondary"
                            onClick={() =>
                              void openInExplorer(savePathDraftById[game.candidateAppId] ?? game.savePath ?? '')
                            }
                          >
                            Open Save Folder
                          </button>
                          <button
                            className="miniBtn rejectBtn"
                            onClick={() => void removeFromCheckpoint(game)}
                            disabled={!!reviewLoadingById[game.candidateAppId]}
                          >
                            {reviewLoadingById[game.candidateAppId] ? 'Removing...' : 'Remove from CheckPoint'}
                          </button>
                        </div>
                        <div className="backupPanel">
                          <div className="backupHeader">
                            <strong>Backups ({(backupsByGameId[game.candidateAppId] ?? []).length})</strong>
                            <button
                              className="miniBtn acceptBtn"
                              disabled={!!backupBusyByGameId[game.candidateAppId] || !game.savePath?.trim()}
                              onClick={() => void createBackupNow(game)}
                            >
                              {backupBusyByGameId[game.candidateAppId] ? 'Working...' : 'Backup Now'}
                            </button>
                          </div>
                          {(backupsByGameId[game.candidateAppId] ?? []).length === 0 ? (
                            <p className="hint">No backups yet. Start the game and exit, or click Backup Now.</p>
                          ) : (
                            <ul className="backupList">
                              {(backupsByGameId[game.candidateAppId] ?? []).slice(0, 6).map((backup) => (
                                <li key={backup.id} className="backupItem">
                                  <span>
                                    {formatTimestamp(backup.createdAt)} | {backup.triggerReason} |{' '}
                                    {formatBytes(backup.totalBytes)}
                                  </span>
                                  <button
                                    className="miniBtn secondary"
                                    disabled={!!backupBusyByGameId[game.candidateAppId]}
                                    onClick={() => void restoreBackupById(game, backup.id)}
                                  >
                                    Restore
                                  </button>
                                </li>
                              ))}
                            </ul>
                          )}
                        </div>
                      </div>
                    </li>
                      )
                    })()
                  ))}
                </ul>
              )}
            </section>
          )}
        </>
      )}

      {addModal ? (
        <section className="modalOverlay" role="dialog" aria-modal="true" aria-label="Confirm game save location">
          <div className="modalCard" style={{ marginTop: `${addModal.anchorOffset}px` }}>
            <h3>Add {addModal.candidate.detectedTitle} to CheckPoint?</h3>
            <p className="hint">
              We found common save-path patterns{addModal.matchedTitle ? ` for ${addModal.matchedTitle}` : ''}. Are
              you sure this is your game save location?
            </p>
            <div className="modalSuggestions">
              {addModal.suggestions.slice(0, 5).map((item) => (
                <button
                  key={`${item.source}:${item.path}`}
                  className="miniBtn secondary suggestionBtn"
                  onClick={() => setAddModal((prev) => (prev ? { ...prev, pathInput: item.path } : prev))}
                >
                  {item.path}
                </button>
              ))}
            </div>
            <label htmlFor="modal-save-path">Save location</label>
            <div className="savePathRow">
              <input
                id="modal-save-path"
                className="savePathInput"
                value={addModal.pathInput}
                autoFocus
                onChange={(event) =>
                  setAddModal((prev) => (prev ? { ...prev, pathInput: event.target.value } : prev))
                }
                placeholder="Edit if needed..."
              />
              <button
                className="miniBtn secondary"
                onClick={() =>
                  void selectSaveFolder(
                    addModal.pathInput,
                    (newPath) => setAddModal((prev) => (prev ? { ...prev, pathInput: newPath } : prev)),
                    (message) => setScanMessage(`Browse failed: ${message}`)
                  )
                }
              >
                Browse
              </button>
            </div>
            <div className="reviewButtons">
              <button className="miniBtn secondary" onClick={() => void openInExplorer(addModal.candidate.installDir)}>
                Open Install Folder
              </button>
              <button className="miniBtn secondary" onClick={() => void openInExplorer(addModal.pathInput)}>
                Open Save Folder
              </button>
            </div>
            <div className="reviewButtons modalActions">
              <button className="miniBtn acceptBtn" onClick={() => void confirmAddFromModal()}>
                Yes, Add to CheckPoint
              </button>
              <button className="miniBtn rejectBtn" onClick={() => setAddModal(null)}>
                Cancel
              </button>
            </div>
          </div>
        </section>
      ) : null}

      <div className="hudBadge">
        CHECKPOINT // ACTIVE <span className="hudCursor" />
      </div>
    </main>
  )
}

export default App
