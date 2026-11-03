<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    getSystemHealth,
    resetIndexerHealth,
    type SystemHealthResponse
  } from '#lib/api/client.js';
  import { showToast } from '#lib/stores/toast.svelte.js';

  let health = $state<SystemHealthResponse | null>(null);
  let loading = $state(false);
  let error = $state('');
  let resettingId = $state<string | null>(null);

  async function handleReset(id: string) {
    resettingId = id;
    try {
      await resetIndexerHealth(id);
      showToast('Indexer health reset');
      await loadHealth();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to reset indexer health';
    } finally {
      resettingId = null;
    }
  }
  let pollInterval: ReturnType<typeof setInterval> | null = null;

  async function loadHealth() {
    loading = true;
    error = '';
    try {
      health = await getSystemHealth();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load system health';
    } finally {
      loading = false;
    }
  }

  function statusColor(status: string): string {
    switch (status) {
      case 'healthy':
        return 'var(--healthy, #28a745)';
      case 'degraded':
        return 'var(--degraded, #ffc107)';
      case 'unhealthy':
      case 'auth_failed':
        return 'var(--unhealthy, #dc3545)';
      default:
        return 'var(--muted)';
    }
  }

  onMount(() => {
    loadHealth();
    pollInterval = setInterval(loadHealth, 30000);
  });

  onDestroy(() => {
    if (pollInterval) clearInterval(pollInterval);
  });
</script>

<div class="page-header">
  <div>
    <h1>System Health</h1>
    <p class="muted">Indexer connectivity and overall system status.</p>
  </div>
  <button class="btn alt" type="button" onclick={loadHealth} disabled={loading}>Refresh</button>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

{#if loading && !health}
  <div class="muted">Loading...</div>
{:else if health}
  <section class="card">
    <h2>Overview</h2>
    <div class="stats-row">
      <div class="stat">
        <span class="stat-value">{health.active_patterns}</span>
        <span class="stat-label">Active Patterns</span>
      </div>
      <div class="stat">
        <span class="stat-value">{health.total_indexers}</span>
        <span class="stat-label">Indexers</span>
      </div>
      <div class="stat healthy">
        <span class="stat-value">{health.healthy_indexers}</span>
        <span class="stat-label">Healthy</span>
      </div>
      <div class="stat degraded">
        <span class="stat-value">{health.degraded_indexers}</span>
        <span class="stat-label">Degraded</span>
      </div>
      <div class="stat unhealthy">
        <span class="stat-value">{health.unhealthy_indexers}</span>
        <span class="stat-label">Unhealthy</span>
      </div>
    </div>
  </section>

  <section class="card">
    <h2>Indexers</h2>
    {#if health.indexers.length === 0}
      <div class="empty">No indexers configured.</div>
    {:else}
      <div class="indexer-list">
        {#each health.indexers as indexer (indexer.id)}
          <div class="indexer-row">
            <div class="indexer-info">
              <div class="indexer-name">
                <span class="status-dot" style="background: {statusColor(indexer.health_status)}"
                ></span>
                {indexer.name}
                {#if !indexer.enabled}
                  <span class="badge disabled">disabled</span>
                {/if}
              </div>
              <div class="indexer-meta">
                <span class="badge status" style="color: {statusColor(indexer.health_status)}"
                  >{indexer.health_status === 'auth_failed'
                    ? 'sign-in expired'
                    : indexer.health_status}</span
                >
                {#if indexer.consecutive_failures > 0}
                  <span class="failures">{indexer.consecutive_failures} failures</span>
                {/if}
                {#if indexer.last_error}
                  <span class="last-error" title={indexer.last_error}>{indexer.last_error}</span>
                {/if}
              </div>
            </div>
            {#if indexer.consecutive_failures > 0}
              <button
                class="btn compact alt reset-btn"
                type="button"
                disabled={resettingId === indexer.id}
                onclick={() => handleReset(indexer.id)}
              >
                {resettingId === indexer.id ? 'Resetting...' : 'Reset & Retry'}
              </button>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>
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
    margin: 0 0 6px;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
  }

  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 18px;
    margin-bottom: 18px;
  }

  .card h2 {
    margin: 0 0 12px;
    font-size: 16px;
  }

  .btn {
    padding: 10px 18px;
    border-radius: 10px;
    border: 1px solid var(--accent);
    background: var(--accent);
    color: #1a1b14;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn.alt {
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
  }

  .error {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .stats-row {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
  }

  .stat {
    display: flex;
    flex-direction: column;
    align-items: center;
    min-width: 80px;
    padding: 12px 16px;
    border-radius: 12px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid var(--ring);
  }

  :global([data-theme='light']) .stat {
    background: rgba(0, 0, 0, 0.03);
  }

  .stat-value {
    font-size: 24px;
    font-weight: 600;
  }

  .stat-label {
    font-size: 11px;
    color: var(--muted);
    margin-top: 4px;
  }

  .stat.healthy .stat-value {
    color: #28a745;
  }

  .stat.degraded .stat-value {
    color: #ffc107;
  }

  .stat.unhealthy .stat-value {
    color: #dc3545;
  }

  @media (max-width: 768px) {
    .stats-row {
      display: grid;
      grid-template-columns: repeat(2, 1fr);
    }

    .stat {
      min-width: 0;
    }
  }

  .empty {
    text-align: center;
    padding: 32px;
    color: var(--muted);
    font-size: 14px;
  }

  .indexer-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .indexer-row {
    display: flex;
    align-items: center;
    padding: 10px 12px;
    border-radius: 8px;
    gap: 12px;
  }

  .indexer-row:hover {
    background: rgba(255, 255, 255, 0.03);
  }

  :global([data-theme='light']) .indexer-row:hover {
    background: rgba(0, 0, 0, 0.03);
  }

  .indexer-info {
    flex: 1;
    min-width: 0;
  }

  .indexer-name {
    font-size: 13px;
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .indexer-meta {
    display: flex;
    gap: 8px;
    margin-top: 4px;
    font-size: 11px;
    color: var(--muted);
    align-items: center;
    flex-wrap: wrap;
  }

  .badge {
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 10px;
    font-weight: 500;
    text-transform: uppercase;
  }

  .badge.disabled {
    background: rgba(108, 117, 125, 0.2);
    color: #6c757d;
  }

  .badge.status {
    background: transparent;
  }

  .failures {
    color: #dc3545;
  }

  .last-error {
    max-width: 300px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: #dc3545;
    opacity: 0.8;
  }

  .reset-btn {
    flex-shrink: 0;
    padding: 5px 12px;
    font-size: 11px;
    border-radius: 6px;
    border: 1px solid var(--ring);
    background: transparent;
    color: var(--ink);
    cursor: pointer;
  }

  .reset-btn:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }

  .reset-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
