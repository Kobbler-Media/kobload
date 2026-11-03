<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getDevSnapshotStatus,
    saveDevSnapshot,
    deleteDevSnapshot,
    type DevSnapshotStatus
  } from '#lib/api/client.js';

  let devSnapshot = $state<DevSnapshotStatus | null>(null);
  let devSnapshotLoading = $state(false);
  let devSnapshotSaving = $state(false);
  let devSnapshotDeleting = $state(false);
  let devSnapshotError = $state('');

  const loadDevSnapshot = async () => {
    devSnapshotLoading = true;
    devSnapshotError = '';
    try {
      devSnapshot = await getDevSnapshotStatus();
    } catch (err) {
      devSnapshotError = err instanceof Error ? err.message : 'Failed to load dev snapshot status';
    } finally {
      devSnapshotLoading = false;
    }
  };

  const handleSaveDevSnapshot = async () => {
    devSnapshotSaving = true;
    devSnapshotError = '';
    try {
      const result = await saveDevSnapshot();
      devSnapshotError = '';
      await loadDevSnapshot();
      devSnapshotError = `Saved: ${result.table_count} tables`;
    } catch (err) {
      devSnapshotError = err instanceof Error ? err.message : 'Failed to save dev snapshot';
    } finally {
      devSnapshotSaving = false;
    }
  };

  const handleDeleteDevSnapshot = async () => {
    const confirmed = confirm('Delete the dev snapshot? This cannot be undone.');
    if (!confirmed) return;

    devSnapshotDeleting = true;
    devSnapshotError = '';
    try {
      await deleteDevSnapshot();
      await loadDevSnapshot();
    } catch (err) {
      devSnapshotError = err instanceof Error ? err.message : 'Failed to delete dev snapshot';
    } finally {
      devSnapshotDeleting = false;
    }
  };

  const formatBytes = (bytes: number): string => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  };

  onMount(() => {
    loadDevSnapshot();
  });
</script>

<div class="page-header">
  <div>
    <h1>Dev Tools</h1>
    <span class="dev-badge">DEV MODE</span>
  </div>
</div>

<section class="card">
  <p class="muted">Development utilities for testing and debugging.</p>
</section>

<section class="card">
  <h3>Dev Snapshot</h3>
  <p class="muted">
    Save your current configuration as the dev default. When <code>DEV_AUTO_RESTORE=1</code>, this
    snapshot will be automatically restored on startup.
  </p>

  {#if devSnapshotError}
    <div class="dev-message" class:success={devSnapshotError.startsWith('Saved:')}>
      {devSnapshotError}
    </div>
  {/if}

  {#if devSnapshotLoading}
    <div class="loading">Loading...</div>
  {:else if devSnapshot}
    <div class="snapshot-status">
      {#if devSnapshot.exists}
        <div class="status-row">
          <span class="label">Status:</span>
          <span class="value success">Snapshot exists</span>
        </div>
        {#if devSnapshot.size}
          <div class="status-row">
            <span class="label">Size:</span>
            <span class="value">{formatBytes(devSnapshot.size)}</span>
          </div>
        {/if}
        {#if devSnapshot.created_at}
          <div class="status-row">
            <span class="label">Created:</span>
            <span class="value">{new Date(devSnapshot.created_at).toLocaleString()}</span>
          </div>
        {/if}
      {:else}
        <div class="status-row">
          <span class="label">Status:</span>
          <span class="value muted">No snapshot saved</span>
        </div>
      {/if}
    </div>

    <div class="actions">
      <button
        class="btn"
        type="button"
        onclick={handleSaveDevSnapshot}
        disabled={devSnapshotSaving || devSnapshotDeleting}
      >
        {devSnapshotSaving ? 'Saving...' : 'Save as Dev Default'}
      </button>
      {#if devSnapshot.exists}
        <button
          class="btn ghost"
          type="button"
          onclick={handleDeleteDevSnapshot}
          disabled={devSnapshotSaving || devSnapshotDeleting}
        >
          {devSnapshotDeleting ? 'Deleting...' : 'Clear Dev Default'}
        </button>
      {/if}
    </div>
  {/if}
</section>

<section class="card">
  <h3>What gets saved</h3>
  <ul class="info-list">
    <li>User accounts and API keys</li>
    <li>Prowlarr configuration</li>
    <li>Download clients (Transmission, SABnzbd, etc.)</li>
    <li>Media types and quality profiles</li>
    <li>Indexer sources and watch patterns</li>
  </ul>
  <p class="muted" style="margin-top: 12px;">
    Transient data like downloads, events, and matches are <strong>not</strong> included.
  </p>
</section>

<style>
  .page-header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 20px;
  }

  .page-header h1 {
    margin: 0;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .dev-badge {
    background: var(--accent);
    color: white;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.5px;
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

  .card h3 {
    margin: 0 0 8px;
    font-size: 16px;
  }

  .card p.muted {
    margin-bottom: 16px;
  }

  code {
    background: rgba(255, 255, 255, 0.08);
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 12px;
    font-family: monospace;
  }

  :global([data-theme='light']) code {
    background: rgba(0, 0, 0, 0.06);
  }

  .snapshot-status {
    display: grid;
    gap: 8px;
    margin-bottom: 16px;
    padding: 12px;
    background: rgba(255, 255, 255, 0.03);
    border-radius: 8px;
  }

  :global([data-theme='light']) .snapshot-status {
    background: rgba(0, 0, 0, 0.03);
  }

  .status-row {
    display: flex;
    gap: 8px;
    font-size: 13px;
  }

  .status-row .label {
    color: var(--muted);
    min-width: 80px;
  }

  .status-row .value {
    color: var(--ink);
  }

  .status-row .value.success {
    color: #22c55e;
  }

  .status-row .value.muted {
    color: var(--muted);
  }

  .loading {
    color: var(--muted);
    font-size: 13px;
  }

  .dev-message {
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    margin-bottom: 12px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .dev-message.success {
    background: rgba(40, 167, 69, 0.15);
    border: 1px solid rgba(40, 167, 69, 0.3);
    color: #28a745;
  }

  .info-list {
    margin: 0;
    padding-left: 20px;
  }

  .info-list li {
    font-size: 13px;
    margin-bottom: 6px;
    color: var(--ink);
  }

  .info-list li:last-child {
    margin-bottom: 0;
  }

  .actions {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
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

  .btn.ghost {
    background: transparent;
    border-color: transparent;
    color: var(--muted);
  }

  .btn.ghost:hover:not(:disabled) {
    color: #dc3545;
  }
</style>
