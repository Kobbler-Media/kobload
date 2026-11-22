<script lang="ts">
  import { onMount } from 'svelte';
  import { getUsenetServers, deleteUsenetServer, testUsenetServerById } from '#lib/api/client.js';
  import type { UsenetServer, UsenetTestResult } from '#lib/types.js';
  import { showToast } from '#lib/stores/toast.svelte.js';
  import LoadingState from '#lib/components/LoadingState.svelte';
  import EmptyState from '#lib/components/EmptyState.svelte';
  import UsenetServerEditModal from '#lib/components/settings/UsenetServerEditModal.svelte';

  const EXPIRY_WARN_DAYS = 14;
  const DAY_MS = 86_400_000;

  let loading = $state(false);
  let loaded = $state(false);
  let error = $state('');
  let servers = $state<UsenetServer[]>([]);
  let editorOpen = $state(false);
  let editing = $state<UsenetServer | null>(null);
  let testResults = $state<Record<string, UsenetTestResult>>({});
  let testing = $state<Record<string, boolean>>({});

  const sortedServers = $derived(
    [...servers].sort((a, b) => a.priority - b.priority || a.name.localeCompare(b.name))
  );

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      servers = await getUsenetServers();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load usenet servers';
    } finally {
      loading = false;
      loaded = true;
    }
  };

  const openEditor = (s: UsenetServer | null) => {
    editing = s;
    editorOpen = true;
  };

  const closeEditor = () => {
    editorOpen = false;
    editing = null;
  };

  const handleSaved = async () => {
    const wasEditing = editing !== null;
    closeEditor();
    showToast(wasEditing ? 'Usenet server updated' : 'Usenet server added');
    await refresh();
  };

  const handleDelete = async (s: UsenetServer) => {
    if (!confirm(`Delete usenet server "${s.name}"?`)) return;
    error = '';
    try {
      await deleteUsenetServer(s.id);
      showToast('Usenet server deleted');
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete usenet server';
    }
  };

  const handleTest = async (id: string) => {
    testing = { ...testing, [id]: true };
    try {
      const result = await testUsenetServerById(id);
      testResults = { ...testResults, [id]: result };
    } catch (err) {
      testResults = {
        ...testResults,
        [id]: { success: false, message: err instanceof Error ? err.message : 'Test failed' }
      };
    } finally {
      testing = { ...testing, [id]: false };
    }
  };

  const expiryState = (expiresOn: string | null): 'expired' | 'soon' | null => {
    if (!expiresOn) return null;
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const days = (new Date(`${expiresOn}T00:00:00`).getTime() - today.getTime()) / DAY_MS;
    if (days < 0) return 'expired';
    if (days <= EXPIRY_WARN_DAYS) return 'soon';
    return null;
  };

  onMount(() => {
    refresh();
  });
</script>

<div class="page-header">
  <div>
    <h1>Usenet Servers</h1>
    <p class="muted">NNTP accounts the kobload usenet engine downloads and streams from.</p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add server</button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

<div class="info-note">
  SABnzbd keeps its own connections to the same accounts while it runs, so the engine's connections
  plus SABnzbd's must stay within each account's connection cap.
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

{#if loading && !loaded}
  <LoadingState message="Loading usenet servers..." />
{:else if servers.length === 0}
  <EmptyState
    icon="download"
    title="No usenet servers yet"
    message="Add a provider account to let the usenet engine fetch articles."
  />
{:else}
  <div class="card-grid">
    {#each sortedServers as server (server.id)}
      {@const expiry = expiryState(server.expires_on)}
      <article class="card">
        <div class="card-header">
          <div>
            <h3>{server.name}</h3>
            <p class="muted host">
              {server.host}:{server.port}
              {#if server.tls}<span class="badge">TLS</span>{/if}
            </p>
          </div>
          <span class="status-chip" class:enabled={server.enabled}>
            {server.enabled ? 'enabled' : 'disabled'}
          </span>
        </div>
        <div class="meta-grid">
          <div>
            <span class="label">Connections</span>
            <span>{server.connections}</span>
          </div>
          <div>
            <span class="label">Priority</span>
            <span>{server.priority}</span>
          </div>
          <div>
            <span class="label">Backbone</span>
            <span>{server.backbone ?? '—'}</span>
          </div>
          <div>
            <span class="label">Expires</span>
            <span class:expired={expiry === 'expired'} class:expiring={expiry === 'soon'}>
              {server.expires_on ?? '—'}
            </span>
          </div>
        </div>
        <div class="actions">
          <button class="btn alt" type="button" onclick={() => openEditor(server)}>Edit</button>
          <button
            class="btn alt"
            type="button"
            onclick={() => handleTest(server.id)}
            disabled={testing[server.id]}
          >
            {testing[server.id] ? 'Testing...' : 'Test'}
          </button>
          <button class="btn ghost" type="button" onclick={() => handleDelete(server)}>
            Delete
          </button>
        </div>
        {#if testResults[server.id]}
          <div class="test-result" class:success={testResults[server.id].success}>
            {testResults[server.id].message}
          </div>
        {/if}
      </article>
    {/each}
  </div>
{/if}

{#if editorOpen}
  <UsenetServerEditModal server={editing} onClose={closeEditor} onSaved={handleSaved} />
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

  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
  }

  .info-note {
    padding: 10px 14px;
    border-radius: 10px;
    margin-bottom: 16px;
    font-size: 13px;
    line-height: 1.5;
    color: var(--muted);
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid var(--ring);
  }

  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 18px;
  }

  .card h3 {
    margin: 0 0 8px;
    font-size: 16px;
  }

  .card-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 16px;
  }

  @media (max-width: 768px) {
    .card-grid {
      grid-template-columns: 1fr;
    }
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      align-items: stretch;
    }
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
  }

  .host {
    display: flex;
    align-items: center;
    gap: 8px;
    word-break: break-all;
  }

  .badge {
    font-size: 10px;
    font-weight: 600;
    padding: 2px 6px;
    border-radius: 5px;
    background: var(--tag-bg);
    color: var(--tag-text);
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

  .status-chip {
    font-size: 11px;
    padding: 4px 8px;
    border-radius: 6px;
    background: var(--tag-bg);
    color: var(--tag-text);
  }

  .status-chip.enabled {
    background: rgba(40, 167, 69, 0.15);
    color: #28a745;
  }

  .meta-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(100px, 1fr));
    gap: 12px;
    margin-top: 12px;
    font-size: 12px;
  }

  .meta-grid .label {
    display: block;
    color: var(--muted);
    margin-bottom: 2px;
  }

  .expiring {
    color: #d38c5a;
    font-weight: 600;
  }

  .expired {
    color: #dc3545;
    font-weight: 600;
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

  .test-result {
    margin-top: 12px;
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .test-result.success {
    background: rgba(40, 167, 69, 0.15);
    border-color: rgba(40, 167, 69, 0.3);
    color: #28a745;
  }
</style>
