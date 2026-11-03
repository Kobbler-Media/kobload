<script lang="ts">
  import { onMount } from 'svelte';
  import { DeleteConfirmModal, ScrollToTop } from '@koblibs/ui';
  import { getDecisionLogSessions, deleteDecisionLogSessions } from '#lib/api/client.js';
  import type { DecisionLogSession } from '#lib/types.js';
  import { showToast } from '#lib/stores/toast.svelte.js';
  import LogToggle from '#lib/components/decision-log/LogToggle.svelte';

  let loading = $state(false);
  let error = $state('');
  let sessions = $state<DecisionLogSession[]>([]);
  let offset = $state(0);
  let hasMore = $state(true);
  let showClearConfirm = $state(false);
  let clearing = $state(false);

  const PAGE_SIZE = 50;

  async function loadSessions(reset = false) {
    if (loading) return;
    loading = true;
    error = '';
    try {
      if (reset) offset = 0;
      const result = await getDecisionLogSessions({
        trigger_type: 'feed_sync',
        is_test: false,
        limit: PAGE_SIZE,
        offset: reset ? 0 : offset
      });
      if (reset) {
        sessions = result;
      } else {
        sessions = [...sessions, ...result];
      }
      hasMore = result.length >= PAGE_SIZE;
      offset = sessions.length;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load feed log';
    } finally {
      loading = false;
    }
  }

  async function handleClearAll() {
    clearing = true;
    try {
      await deleteDecisionLogSessions('feed_sync');
      sessions = [];
      hasMore = false;
      offset = 0;
      showClearConfirm = false;
      showToast('Feed log cleared');
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to clear log';
    } finally {
      clearing = false;
    }
  }

  function getOutcomeStyle(outcome: string | null): { bg: string; color: string; label: string } {
    switch (outcome) {
      case 'downloaded':
        return { bg: 'rgba(34, 197, 94, 0.15)', color: '#22c55e', label: 'Downloaded' };
      case 'no_match':
        return { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af', label: 'No Match' };
      case 'error':
        return { bg: 'rgba(239, 68, 68, 0.15)', color: '#ef4444', label: 'Error' };
      case 'held':
        return { bg: 'rgba(168, 85, 247, 0.15)', color: '#a855f7', label: 'Held' };
      case 'callback_sent':
        return { bg: 'rgba(59, 130, 246, 0.15)', color: '#3b82f6', label: 'Notified' };
      case 'skipped':
        return { bg: 'rgba(234, 179, 8, 0.15)', color: '#eab308', label: 'Skipped' };
      default:
        return {
          bg: 'rgba(156, 163, 175, 0.15)',
          color: '#9ca3af',
          label: outcome ?? 'Pending'
        };
    }
  }

  function formatRelative(date: string): string {
    const diff = Date.now() - new Date(date).getTime();
    const minutes = Math.floor(diff / 60000);
    if (minutes < 1) return 'Just now';
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  }

  onMount(() => {
    loadSessions(true);
  });
</script>

<div class="page-header">
  <div>
    <h1>Feed Log</h1>
    <p class="muted">{sessions.length} session{sessions.length !== 1 ? 's' : ''}</p>
  </div>
  <div class="header-actions">
    <LogToggle type="feed" />
    <button
      class="btn compact alt"
      type="button"
      onclick={() => loadSessions(true)}
      disabled={loading}
    >
      Refresh
    </button>
    <button
      class="btn compact danger"
      type="button"
      onclick={() => (showClearConfirm = true)}
      disabled={sessions.length === 0}
    >
      Clear All
    </button>
  </div>
</div>

{#if error}
  <div class="error-banner">{error}</div>
{/if}

<div class="session-list">
  {#if loading && sessions.length === 0}
    <div class="loading-state">Loading...</div>
  {:else if sessions.length === 0}
    <div class="empty-state">
      No feed sessions recorded. Enable feed logging to start capturing decision data.
    </div>
  {:else}
    {#each sessions as session (session.id)}
      {@const outcome = getOutcomeStyle(session.outcome)}
      <a class="session-card" href="/watch/decision-log/{session.id}">
        <div class="session-top">
          <div class="session-left">
            <span class="trigger-badge" style="background: rgba(6, 182, 212, 0.15); color: #06b6d4">
              Feed
            </span>
            <span class="session-name">
              {session.source_name ?? 'Unknown source'}
            </span>
          </div>
          <div class="session-right">
            <span class="outcome-badge" style="background: {outcome.bg}; color: {outcome.color}">
              {outcome.label}
            </span>
          </div>
        </div>
        <div class="session-meta">
          <span>
            {session.releases_evaluated} release{session.releases_evaluated !== 1 ? 's' : ''} evaluated
          </span>
          {#if session.duration_ms !== null}
            <span>{session.duration_ms}ms</span>
          {/if}
          <span class="time" title={new Date(session.created_at).toLocaleString()}>
            {formatRelative(session.created_at)}
          </span>
        </div>
        {#if session.error_message}
          <div class="session-error">{session.error_message}</div>
        {/if}
      </a>
    {/each}

    {#if hasMore}
      <div class="load-more">
        <button
          class="btn compact alt"
          type="button"
          onclick={() => loadSessions()}
          disabled={loading}
        >
          {loading ? 'Loading...' : 'Load More'}
        </button>
      </div>
    {/if}
  {/if}
</div>

<ScrollToTop />

{#if showClearConfirm}
  <DeleteConfirmModal
    title="Clear Feed Log?"
    message="This will permanently delete all feed log sessions. This cannot be undone."
    loading={clearing}
    onConfirm={handleClearAll}
    onCancel={() => {
      showClearConfirm = false;
    }}
  />
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 20px;
  }

  .page-header h1 {
    margin: 0 0 4px;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .muted {
    color: var(--muted);
    font-size: 13px;
    margin: 0;
  }

  .header-actions {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .error-banner {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .session-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .loading-state,
  .empty-state {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
  }

  .session-card {
    display: block;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
    padding: 12px 16px;
    text-decoration: none;
    color: inherit;
    transition: border-color 0.15s;
  }

  .session-card:hover {
    border-color: var(--accent);
  }

  .session-top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    margin-bottom: 6px;
  }

  .session-left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .session-right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .trigger-badge,
  .outcome-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    font-weight: 600;
    text-transform: capitalize;
    white-space: nowrap;
  }

  .session-name {
    font-size: 14px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .session-meta {
    display: flex;
    gap: 12px;
    font-size: 11px;
    color: var(--muted);
  }

  .session-error {
    margin-top: 6px;
    padding: 4px 8px;
    background: rgba(239, 68, 68, 0.08);
    border-radius: 6px;
    font-size: 11px;
    color: #ef4444;
  }

  .load-more {
    display: flex;
    justify-content: center;
    padding: 12px;
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      gap: 10px;
    }

    .header-actions {
      flex-wrap: wrap;
    }

    .session-top {
      flex-direction: column;
      align-items: flex-start;
      gap: 6px;
    }

    .session-meta {
      flex-wrap: wrap;
      gap: 8px;
    }
  }
</style>
