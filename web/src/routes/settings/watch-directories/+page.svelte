<script lang="ts">
  import { onMount } from 'svelte';
  import {
    listWatchDirectories,
    deleteWatchDirectory,
    scanWatchDirectory,
    retryWatchDirectory
  } from '$lib/api/client';
  import type { WatchDirectory } from '$lib/types';
  import WatchDirectoryEditModal from '$lib/components/settings/WatchDirectoryEditModal.svelte';

  let loading = $state(false);
  let error = $state('');
  let directories = $state<WatchDirectory[]>([]);
  let scanningIds = $state<Set<string>>(new Set());

  let editorOpen = $state(false);
  let editing = $state<WatchDirectory | null>(null);

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      directories = await listWatchDirectories();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load watch directories';
    } finally {
      loading = false;
    }
  };

  const openEditor = (dir: WatchDirectory | null) => {
    editing = dir;
    editorOpen = true;
  };

  const closeEditor = () => {
    editorOpen = false;
    editing = null;
  };

  const handleSaved = async () => {
    closeEditor();
    await refresh();
  };

  const handleDelete = async (id: string) => {
    error = '';
    try {
      await deleteWatchDirectory(id);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete watch directory';
    }
  };

  const handleScan = async (id: string) => {
    scanningIds = new Set([...scanningIds, id]);
    try {
      await scanWatchDirectory(id);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to trigger scan';
    } finally {
      scanningIds = new Set([...scanningIds].filter((x) => x !== id));
    }
  };

  const handleRetry = async (id: string) => {
    scanningIds = new Set([...scanningIds, id]);
    try {
      await retryWatchDirectory(id);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to retry';
    } finally {
      scanningIds = new Set([...scanningIds].filter((x) => x !== id));
    }
  };

  function formatDate(iso: string | null): string {
    if (!iso) return 'Never';
    const d = new Date(iso);
    return d.toLocaleDateString(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  const mediaTypeBadge: Record<string, string> = {
    book: 'Book',
    tv: 'TV',
    movie: 'Movie'
  };

  onMount(() => {
    refresh();
  });
</script>

<div class="page-header">
  <div>
    <h1>Watch Directories</h1>
    <p class="muted">
      Monitor filesystem directories for new media files and send them to your library server.
    </p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add directory</button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<div class="card-grid">
  {#if directories.length === 0 && !loading}
    <div class="empty-state">
      <p>No watch directories configured yet.</p>
      <p class="muted">
        Add a directory to start monitoring for new media files. Make sure the path is accessible to
        the Kobload container (Docker volume mount).
      </p>
    </div>
  {:else}
    {#each directories as dir (dir.id)}
      <article class="card">
        <div class="card-header">
          <div>
            <h3>{dir.name}</h3>
            <p class="muted path-text">{dir.path}</p>
          </div>
          <div class="badges">
            <span class="badge type">{mediaTypeBadge[dir.media_type] ?? dir.media_type}</span>
            <span class="badge" class:enabled={dir.enabled} class:disabled={!dir.enabled}>
              {dir.enabled ? 'Enabled' : 'Disabled'}
            </span>
          </div>
        </div>

        <div class="meta-grid">
          <div>
            <span class="label">Status</span>
            <span
              class="scan-status"
              class:scanning={dir.scan_status === 'scanning'}
              class:scan-error={dir.scan_status === 'error'}
            >
              {dir.scan_status}
            </span>
          </div>
          <div>
            <span class="label">Last scan</span>
            <span>{formatDate(dir.last_scan_at)}</span>
          </div>
          <div>
            <span class="label">Recursive</span>
            <span>{dir.recursive ? 'Yes' : 'No'}</span>
          </div>
        </div>

        {#if dir.scan_error}
          <div class="scan-error-msg">{dir.scan_error}</div>
        {/if}

        <div class="actions">
          <button class="btn alt" type="button" onclick={() => openEditor(dir)}>Edit</button>
          <button
            class="btn alt"
            type="button"
            onclick={() => handleScan(dir.id)}
            disabled={scanningIds.has(dir.id) || dir.scan_status === 'scanning'}
          >
            {scanningIds.has(dir.id) ? 'Scanning...' : 'Scan Now'}
          </button>
          {#if dir.scan_status === 'error'}
            <button
              class="btn alt"
              type="button"
              onclick={() => handleRetry(dir.id)}
              disabled={scanningIds.has(dir.id)}
            >
              Retry
            </button>
          {/if}
          <button class="btn ghost" type="button" onclick={() => handleDelete(dir.id)}
            >Delete</button
          >
        </div>
      </article>
    {/each}
  {/if}
</div>

{#if editorOpen}
  <WatchDirectoryEditModal directory={editing} onClose={closeEditor} onSaved={handleSaved} />
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

  .header-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      align-items: stretch;
    }
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

  .card-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 16px;
  }

  @media (max-width: 768px) {
    .card-grid {
      grid-template-columns: 1fr;
    }
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
  }

  .path-text {
    font-size: 12px;
    font-family: monospace;
    word-break: break-all;
  }

  .badges {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }

  .badge {
    font-size: 11px;
    padding: 3px 8px;
    border-radius: 6px;
    background: var(--tag-bg, rgba(255, 255, 255, 0.08));
    color: var(--tag-text, var(--muted));
    white-space: nowrap;
  }

  .badge.type {
    background: rgba(211, 140, 90, 0.18);
    color: var(--accent);
  }

  .badge.enabled {
    background: rgba(40, 167, 69, 0.15);
    color: #28a745;
  }

  .badge.disabled {
    background: rgba(220, 53, 69, 0.15);
    color: #dc3545;
  }

  .actions {
    display: flex;
    gap: 10px;
    margin-top: 16px;
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
    transition:
      background 0.15s,
      opacity 0.15s;
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

  .btn.alt:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.06);
  }

  :global([data-theme='light']) .btn.alt:hover:not(:disabled) {
    background: rgba(0, 0, 0, 0.06);
  }

  .btn.ghost {
    background: transparent;
    border-color: transparent;
    color: var(--muted);
  }

  .btn.ghost:hover:not(:disabled) {
    color: #dc3545;
  }

  .meta-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(90px, 1fr));
    gap: 12px;
    margin-top: 12px;
    font-size: 12px;
  }

  .meta-grid .label {
    display: block;
    color: var(--muted);
    margin-bottom: 2px;
  }

  .scan-status {
    text-transform: capitalize;
  }

  .scan-status.scanning {
    color: var(--accent);
  }

  .scan-status.scan-error {
    color: #dc3545;
  }

  .scan-error-msg {
    margin-top: 10px;
    padding: 8px 12px;
    border-radius: 8px;
    font-size: 12px;
    background: rgba(220, 53, 69, 0.1);
    border: 1px solid rgba(220, 53, 69, 0.2);
    color: #dc3545;
    word-break: break-word;
  }

  .error {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    text-align: left;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .empty-state {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
    grid-column: 1 / -1;
  }

  .empty-state p {
    margin: 0 0 8px;
  }

  .empty-state p:last-child {
    margin-bottom: 0;
  }
</style>
