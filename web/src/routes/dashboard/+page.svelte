<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    getConfig,
    listMediaTypes,
    getDashboardSummary,
    type DownloadClientConfig,
    type DownloadItem,
    type MediaType,
    type DashboardSummary
  } from '#lib/api/client.js';
  import { createSseConnection, type SseConnection } from '#lib/sse.js';
  import { getPatternTitle } from '#lib/components/watch/grouping.js';
  import LoadingState from '#lib/components/LoadingState.svelte';
  import {
    getDownloadStatusStyle,
    getWatchStatusSolid,
    getMatchActionStyle
  } from '#lib/styles/statusColors.js';

  let loading = $state(true);
  let downloadClients = $state.raw<DownloadClientConfig[]>([]);
  let mediaTypes = $state.raw<MediaType[]>([]);
  let downloads = $state.raw<DownloadItem[]>([]);
  let summary = $state.raw<DashboardSummary | null>(null);
  let sseConnection: SseConnection | null = null;

  const handleStreamEvent = (payload: Record<string, unknown>) => {
    const type = payload.type as string;
    if (type === 'snapshot' && Array.isArray(payload.downloads)) {
      downloads = payload.downloads as DownloadItem[];
    } else if (type === 'created' && payload.download) {
      const dl = payload.download as DownloadItem;
      downloads = [dl, ...downloads.filter((d) => d.id !== dl.id)];
    } else if (type === 'updated' && payload.download) {
      const dl = payload.download as DownloadItem;
      downloads = downloads.map((d) => (d.id === dl.id ? dl : d));
    }
  };

  const refreshStaticData = async () => {
    try {
      const [config, mt, s] = await Promise.all([
        getConfig(),
        listMediaTypes(),
        getDashboardSummary()
      ]);
      downloadClients = config.download_clients ?? [];
      mediaTypes = mt;
      summary = s;
    } catch {
      // Ignore errors
    }
  };

  const handleVisibility = () => {
    if (document.visibilityState === 'visible') {
      refreshStaticData();
    }
  };

  onMount(async () => {
    try {
      const [config, mt, s] = await Promise.all([
        getConfig(),
        listMediaTypes(),
        getDashboardSummary()
      ]);
      downloadClients = config.download_clients ?? [];
      mediaTypes = mt;
      summary = s;
    } catch {
      // Ignore errors for now
    } finally {
      loading = false;
    }

    sseConnection = createSseConnection(
      '/api/v1/downloads/stream',
      handleStreamEvent,
      () => {} // Dashboard doesn't need to show SSE errors
    );

    document.addEventListener('visibilitychange', handleVisibility);
  });

  onDestroy(() => {
    sseConnection?.stop();
    document.removeEventListener('visibilitychange', handleVisibility);
  });

  const ACTIVE_STATUSES = ['pending', 'queued', 'downloading', 'seeding', 'processing'];

  const enabledClients = $derived(downloadClients.filter((c) => c.enabled));
  const activeDownloads = $derived(downloads.filter((d) => ACTIVE_STATUSES.includes(d.status)));
  const completedDownloads = $derived(downloads.filter((d) => d.status === 'completed'));
  const failedDownloads = $derived(downloads.filter((d) => d.status === 'failed'));

  const formatRelative = (date: string): string => {
    const diff = Date.now() - new Date(date).getTime();
    const minutes = Math.floor(diff / 60000);
    if (minutes < 1) return 'Just now';
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  };
</script>

<div class="page-header">
  <div>
    <h1>Dashboard</h1>
    <p class="muted">Download orchestration overview</p>
  </div>
</div>

{#if loading}
  <LoadingState />
{:else}
  <div class="stats-grid">
    <div class="stat-card">
      <div class="stat-value">{enabledClients.length}</div>
      <div class="stat-label">Download Clients</div>
      {#if downloadClients.length > enabledClients.length}
        <div class="stat-detail">
          {downloadClients.length - enabledClients.length} disabled
        </div>
      {/if}
    </div>

    <div class="stat-card">
      <div class="stat-value">{mediaTypes.length}</div>
      <div class="stat-label">Media Types</div>
    </div>

    <div class="stat-card">
      <div class="stat-value active">{activeDownloads.length}</div>
      <div class="stat-label">Active Downloads</div>
    </div>

    <div class="stat-card">
      <div class="stat-value completed">{completedDownloads.length}</div>
      <div class="stat-label">Completed</div>
    </div>

    <div class="stat-card">
      <div class="stat-value failed">{failedDownloads.length}</div>
      <div class="stat-label">Failed</div>
    </div>
  </div>

  {#if summary}
    <div class="activity-grid">
      <section class="activity-section">
        <div class="activity-header">
          <h2>Recent Downloads</h2>
          <a href="/downloads" class="view-all">View All</a>
        </div>
        {#if summary.recent_downloads.length === 0}
          <div class="activity-empty">No recent downloads</div>
        {:else}
          <div class="activity-list">
            {#each summary.recent_downloads as dl (dl.id)}
              {@const style = getDownloadStatusStyle(dl.status)}
              <div class="activity-item">
                <div class="activity-main">
                  <span class="activity-title" title={dl.name}>{dl.name}</span>
                  <span class="activity-time">{formatRelative(dl.created_at)}</span>
                </div>
                <div class="activity-meta">
                  <span class="badge" style="background: {style.bg}; color: {style.color}"
                    >{dl.status}</span
                  >
                  {#if dl.protocol}
                    <span class="protocol-tag">{dl.protocol}</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="activity-section">
        <div class="activity-header">
          <h2>Recent Matches</h2>
          <a href="/watch/history" class="view-all">View All</a>
        </div>
        {#if summary.recent_matches.length === 0}
          <div class="activity-empty">No recent matches</div>
        {:else}
          <div class="activity-list">
            {#each summary.recent_matches as match (match.id)}
              {@const act = getMatchActionStyle(match.action)}
              <div class="activity-item">
                <div class="activity-main">
                  <span class="activity-title" title={match.release_title}
                    >{match.release_title}</span
                  >
                  <span class="activity-time">{formatRelative(match.created_at)}</span>
                </div>
                <div class="activity-meta">
                  <span class="badge" style="background: {act.bg}">{act.label}</span>
                  <span class="score-tag">{(match.match_score * 100).toFixed(0)}%</span>
                  {#if match.rejected}
                    <span class="badge" style="background: #ef4444">Rejected</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="activity-section">
        <div class="activity-header">
          <h2>Recent Patterns</h2>
          <a href="/watch/patterns" class="view-all">View All</a>
        </div>
        {#if summary.recent_patterns.length === 0}
          <div class="activity-empty">No watch patterns</div>
        {:else}
          <div class="activity-list">
            {#each summary.recent_patterns as pattern (pattern.id)}
              {@const style = getWatchStatusSolid(pattern.status, pattern.enabled)}
              <div class="activity-item">
                <div class="activity-main">
                  <span class="activity-title" title={getPatternTitle(pattern)}
                    >{getPatternTitle(pattern)}</span
                  >
                  <span class="activity-time">{formatRelative(pattern.updated_at)}</span>
                </div>
                <div class="activity-meta">
                  <span class="badge" style="background: {style.bg}; color: {style.color}"
                    >{style.label}</span
                  >
                  <span class="source-tag">{pattern.source_system}</span>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    </div>
  {/if}

  <section class="card">
    <h2>Quick Links</h2>
    <div class="links-grid">
      <a href="/downloads" class="link-card">
        <svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
          <path d="M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z"></path>
        </svg>
        <span>View Downloads</span>
      </a>
      <a href="/settings/clients" class="link-card">
        <svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
          <path d="M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z"></path>
        </svg>
        <span>Configure Clients</span>
      </a>
      <a href="/settings/media-types" class="link-card">
        <svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
          <path d="M4 4h16v2H4V4zm0 4h16v3H4V8zm0 5h16v3H4v-3zm0 5h16v2H4v-2z"></path>
        </svg>
        <span>Media Types</span>
      </a>
      <a href="/settings/prowlarr" class="link-card">
        <svg viewBox="0 0 24 24" width="24" height="24" fill="currentColor">
          <path
            d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z"
          ></path>
        </svg>
        <span>Prowlarr Settings</span>
      </a>
    </div>
  </section>
{/if}

<style>
  .page-header {
    margin-bottom: 24px;
  }

  .page-header h1 {
    margin: 0 0 6px;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
  }

  .stats-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 16px;
    margin-bottom: 24px;
  }

  .stat-card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 20px;
    text-align: center;
  }

  .stat-value {
    font-size: 36px;
    font-weight: 600;
    color: var(--accent);
    line-height: 1;
    margin-bottom: 8px;
  }

  .stat-value.active {
    color: #3b82f6;
  }

  .stat-value.completed {
    color: #22c55e;
  }

  .stat-value.failed {
    color: #ef4444;
  }

  .stat-label {
    font-size: 14px;
    font-weight: 500;
    color: var(--ink);
    margin-bottom: 4px;
  }

  .stat-detail {
    font-size: 12px;
    color: var(--muted);
  }

  /* Activity sections */
  .activity-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: 16px;
    margin-bottom: 24px;
  }

  .activity-section {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 16px 20px;
  }

  .activity-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }

  .activity-header h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .view-all {
    font-size: 12px;
    color: var(--accent);
    text-decoration: none;
    font-weight: 500;
  }

  .view-all:hover {
    text-decoration: underline;
  }

  .activity-empty {
    color: var(--muted);
    font-size: 13px;
    padding: 12px 0;
  }

  .activity-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .activity-item {
    padding: 8px 0;
    border-top: 1px solid var(--ring);
  }

  .activity-item:first-child {
    border-top: none;
    padding-top: 0;
  }

  .activity-item:last-child {
    padding-bottom: 0;
  }

  .activity-main {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 4px;
  }

  .activity-title {
    font-size: 13px;
    color: var(--ink);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .activity-time {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .activity-meta {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
  }

  .badge {
    font-size: 10px;
    padding: 2px 7px;
    border-radius: 4px;
    font-weight: 500;
    color: #fff;
    text-transform: capitalize;
  }

  .protocol-tag,
  .source-tag,
  .score-tag {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.08);
    color: var(--muted);
    font-weight: 500;
  }

  :global([data-theme='light']) .protocol-tag,
  :global([data-theme='light']) .source-tag,
  :global([data-theme='light']) .score-tag {
    background: rgba(0, 0, 0, 0.06);
  }

  /* Quick links */
  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 20px;
  }

  .card h2 {
    margin: 0 0 16px;
    font-size: 16px;
  }

  .links-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 12px;
  }

  .link-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 20px 16px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid var(--ring);
    border-radius: 12px;
    color: var(--ink);
    text-decoration: none;
    font-size: 13px;
    transition:
      background 0.15s,
      border-color 0.15s;
  }

  .link-card:hover {
    background: rgba(211, 140, 90, 0.1);
    border-color: var(--accent);
  }

  :global([data-theme='light']) .link-card {
    background: rgba(0, 0, 0, 0.03);
  }

  :global([data-theme='light']) .link-card:hover {
    background: rgba(200, 122, 70, 0.12);
  }

  .link-card svg {
    opacity: 0.7;
  }

  .link-card:hover svg {
    opacity: 1;
    color: var(--accent);
  }
</style>
