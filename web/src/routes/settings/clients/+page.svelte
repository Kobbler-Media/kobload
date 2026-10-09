<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getConfig,
    deleteDownloadClient,
    testDownloadClientConnection,
    listDownloadClientTypes,
    type DownloadClientConfig,
    type TestConnectionResult,
    type DownloadClientTypeInfo
  } from '$lib/api/client';
  import DownloadClientEditModal from '$lib/components/settings/DownloadClientEditModal.svelte';

  let loading = $state(false);
  let error = $state('');
  let downloadClients = $state<DownloadClientConfig[]>([]);
  let downloadClientTypes = $state<DownloadClientTypeInfo[]>([]);
  let clientTestResults = $state<Record<string, TestConnectionResult>>({});
  let clientTesting = $state<Record<string, boolean>>({});

  let editorOpen = $state(false);
  let editing = $state<DownloadClientConfig | null>(null);

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      const [config, clientTypes] = await Promise.all([getConfig(), listDownloadClientTypes()]);
      downloadClients = config.download_clients ?? [];
      downloadClientTypes = clientTypes ?? [];
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load settings';
    } finally {
      loading = false;
    }
  };

  const openEditor = (client: DownloadClientConfig | null) => {
    editing = client;
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

  const handleDeleteClient = async (clientId: string) => {
    error = '';
    try {
      await deleteDownloadClient(clientId);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete download client';
    }
  };

  const handleTestDownloadClient = async (clientId: string) => {
    clientTesting = { ...clientTesting, [clientId]: true };
    try {
      const result = await testDownloadClientConnection(clientId);
      clientTestResults = { ...clientTestResults, [clientId]: result };
    } catch (err) {
      clientTestResults = {
        ...clientTestResults,
        [clientId]: {
          success: false,
          message: err instanceof Error ? err.message : 'Test failed'
        }
      };
    } finally {
      clientTesting = { ...clientTesting, [clientId]: false };
    }
  };

  onMount(() => {
    refresh();
  });
</script>

<div class="page-header">
  <div>
    <h1>Download Clients</h1>
    <p class="muted">Add the clients Kobload can send NZBs or torrents to.</p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add client</button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<div class="card-grid">
  {#if downloadClients.length === 0}
    <div class="empty-state">No download clients configured yet.</div>
  {:else}
    {#each downloadClients as client (client.id)}
      <article class="card">
        <div class="card-header">
          <div>
            <h3>{client.name}</h3>
            <p class="muted">
              {client.client_type} · {client.protocol} · {client.base_url}
            </p>
          </div>
          <span class="status-chip">{client.enabled ? 'enabled' : 'disabled'}</span>
        </div>
        <div class="meta-grid">
          <div>
            <span class="label">Default</span>
            <span>{client.is_default ? 'Yes' : 'No'}</span>
          </div>
          {#each downloadClientTypes
            .find((t) => t.id === client.client_type)
            ?.fields.filter((f) => !['base_url', 'api_key', 'username', 'password'].includes(f.name)) ?? [] as field (field.name)}
            <div>
              <span class="label">{field.label}</span>
              <span>{client[field.name as keyof typeof client] || '—'}</span>
            </div>
          {/each}
        </div>
        <div class="actions">
          <button class="btn alt" type="button" onclick={() => openEditor(client)}> Edit </button>
          <button
            class="btn alt"
            type="button"
            onclick={() => handleTestDownloadClient(client.id)}
            disabled={clientTesting[client.id]}
          >
            {clientTesting[client.id] ? 'Testing...' : 'Test'}
          </button>
          <button class="btn ghost" type="button" onclick={() => handleDeleteClient(client.id)}>
            Delete
          </button>
        </div>
        {#if clientTestResults[client.id]}
          <div class="test-result" class:success={clientTestResults[client.id].success}>
            {clientTestResults[client.id].message}
          </div>
        {/if}
      </article>
    {/each}
  {/if}
</div>

{#if editorOpen}
  <DownloadClientEditModal
    client={editing}
    clientTypes={downloadClientTypes}
    existingClients={downloadClients}
    onClose={closeEditor}
    onSaved={handleSaved}
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
    border: 1px solid rgba(40, 167, 69, 0.3);
    color: #28a745;
  }
</style>
