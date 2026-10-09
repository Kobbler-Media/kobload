<script lang="ts">
  import { onMount } from 'svelte';
  import { Modal } from '@koblibs/ui';
  import {
    listIndexerSources,
    deleteIndexerSource,
    resetIndexerHealth,
    syncProwlarrIndexers,
    previewProwlarrMigration,
    confirmProwlarrMigration,
    listDefinitions,
    listIndexerHandlers,
    listMediaTypes,
    type SyncProwlarrResponse,
    type DefinitionSummary,
    type IndexerHandlerInfo,
    type MediaType,
    type IndexerMigrationItem,
    type DownloadClientMatch,
    type MigrationResult
  } from '$lib/api/client';
  import type { IndexerSource } from '$lib/types';
  import { showToast } from '$lib/stores/toast.svelte';
  import IndexerEditModal from '$lib/components/settings/IndexerEditModal.svelte';

  let loading = $state(false);
  let error = $state('');
  let sources = $state<IndexerSource[]>([]);
  let allSources = $state<IndexerSource[]>([]);
  let definitions = $state<DefinitionSummary[]>([]);
  let handlers = $state<IndexerHandlerInfo[]>([]);
  let mediaTypes = $state<MediaType[]>([]);
  let syncResult = $state<SyncProwlarrResponse | null>(null);
  let syncing = $state(false);

  let editorOpen = $state(false);
  let editing = $state<IndexerSource | null>(null);

  // Migration state
  let migrateModalOpen = $state(false);
  let migrateSourceId = $state<string | null>(null);
  let migrateLoading = $state(false);
  let migrateError = $state('');
  let migrateItems = $state<IndexerMigrationItem[]>([]);
  let migrateClients = $state<DownloadClientMatch[]>([]);
  let migrateSelected = $state<Set<number>>(new Set());
  let migrateConfirming = $state(false);
  let migrateResult = $state<MigrationResult | null>(null);

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      const [allSrc, defs, hdlrs, mts] = await Promise.all([
        listIndexerSources(),
        listDefinitions(),
        listIndexerHandlers(),
        listMediaTypes()
      ]);
      allSources = allSrc;
      sources = allSrc.filter((s) => s.handler_type !== 'prowlarr_rss');
      definitions = defs;
      handlers = hdlrs;
      mediaTypes = mts;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load indexers';
    } finally {
      loading = false;
    }
  };

  const openEditor = (source: IndexerSource | null) => {
    editing = source;
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

  const rssCountForProwlarr = (source: IndexerSource): number => {
    const baseUrl = (source.handler_config as Record<string, unknown>).base_url as string;
    if (!baseUrl) return 0;
    return allSources.filter(
      (s) =>
        s.handler_type === 'prowlarr_rss' &&
        (s.handler_config as Record<string, unknown>).base_url === baseUrl
    ).length;
  };

  const handleDelete = async (id: string) => {
    error = '';
    try {
      await deleteIndexerSource(id);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete';
    }
  };

  const handleSync = async () => {
    syncing = true;
    syncResult = null;
    try {
      syncResult = await syncProwlarrIndexers();
      await refresh();
    } catch (err) {
      syncResult = null;
      error = err instanceof Error ? err.message : 'Sync failed';
    } finally {
      syncing = false;
    }
  };

  const openMigrateModal = async (sourceId: string) => {
    migrateSourceId = sourceId;
    migrateModalOpen = true;
    migrateLoading = true;
    migrateError = '';
    migrateItems = [];
    migrateClients = [];
    migrateSelected = new Set();
    migrateResult = null;
    migrateConfirming = false;
    try {
      const preview = await previewProwlarrMigration(sourceId);
      migrateItems = preview.indexers;
      migrateClients = preview.download_clients;
      const sel = new Set<number>();
      for (const item of preview.indexers) {
        if (item.status.type === 'will_create') sel.add(item.prowlarr_id);
      }
      migrateSelected = sel;
    } catch (err) {
      migrateError = err instanceof Error ? err.message : 'Failed to load migration preview';
    } finally {
      migrateLoading = false;
    }
  };

  const toggleMigrateItem = (prowlarrId: number) => {
    const next = new Set(migrateSelected);
    if (next.has(prowlarrId)) {
      next.delete(prowlarrId);
    } else {
      next.add(prowlarrId);
    }
    migrateSelected = next;
  };

  const handleMigrateConfirm = async () => {
    if (!migrateSourceId) return;
    migrateConfirming = true;
    migrateError = '';
    try {
      const items = migrateItems
        .filter((i) => migrateSelected.has(i.prowlarr_id) && i.status.type === 'will_create')
        .map((i) => ({
          prowlarr_id: i.prowlarr_id,
          name: i.name,
          handler_type: i.handler_type,
          handler_config: i.handler_config ?? {},
          category_ids: i.category_ids
        }));
      migrateResult = await confirmProwlarrMigration(migrateSourceId, items);
      await refresh();
    } catch (err) {
      migrateError = err instanceof Error ? err.message : 'Migration failed';
    } finally {
      migrateConfirming = false;
    }
  };

  const closeMigrateModal = () => {
    migrateModalOpen = false;
    migrateSourceId = null;
  };

  const migrateCreatable = $derived(migrateItems.filter((i) => i.status.type === 'will_create'));

  const migrateStatusLabel = (item: IndexerMigrationItem): string => {
    switch (item.status.type) {
      case 'will_create':
        return 'New';
      case 'already_exists':
        return `Exists: ${item.existing_source_name ?? 'unknown'}`;
      case 'unsupported':
        return `Unsupported: ${item.status.reason}`;
      case 'missing_fields':
        return `Missing: ${item.status.fields.join(', ')}`;
      default:
        return 'Unknown';
    }
  };

  const migrateStatusClass = (item: IndexerMigrationItem): string => {
    switch (item.status.type) {
      case 'will_create':
        return 'migrate-new';
      case 'already_exists':
        return 'migrate-exists';
      default:
        return 'migrate-skip';
    }
  };

  const handlerLabel = (type: string): string => handlers.find((h) => h.id === type)?.name ?? type;

  const sourceDescription = (source: IndexerSource): string => {
    const config = source.handler_config as Record<string, unknown>;
    switch (source.handler_type) {
      case 'prowlarr':
      case 'newznab':
      case 'torznab':
        return String(config.base_url ?? '');
      case 'cardigann':
        return String(config.definition ?? '');
      default:
        return source.handler_type;
    }
  };

  onMount(() => {
    refresh();
  });
</script>

<div class="page-header">
  <div>
    <h1>Indexers</h1>
    <p class="muted">Configure indexer sources for searching and monitoring releases.</p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add indexer</button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

{#if syncResult}
  <div class="test-result success">
    Synced: {syncResult.created} created, {syncResult.updated} updated, {syncResult.disabled} disabled
  </div>
{/if}

<div class="card-grid">
  {#if sources.length === 0}
    <div class="empty-state">No indexers configured yet.</div>
  {:else}
    {#each sources as source (source.id)}
      <article class="card">
        <div class="card-header">
          <div>
            <h3>{source.name}</h3>
            <p class="muted">
              {handlerLabel(source.handler_type)} · {sourceDescription(source)}
            </p>
          </div>
          {#if source.auth_failed_at}
            <span class="status-chip auth-failed">sign-in expired</span>
          {:else}
            <span class="status-chip">{source.enabled ? 'enabled' : 'disabled'}</span>
          {/if}
        </div>

        {#if source.auth_failed_at}
          <p class="auth-notice">
            {source.name} rejected the stored credentials on {new Date(
              source.auth_failed_at
            ).toLocaleString()}. Retrying won't help — edit this indexer and enter a fresh
            {source.handler_type === 'mam'
              ? 'MAM ID cookie'
              : source.handler_type === 'torrentday'
                ? 'cookie'
                : 'API key'}.
          </p>
        {/if}

        <div class="meta-grid">
          {#if source.handler_type === 'prowlarr'}
            <div>
              <span class="label">Synced feeds</span>
              <span>{rssCountForProwlarr(source)}</span>
            </div>
          {/if}
          {#if source.last_check_at}
            <div>
              <span class="label">Last check</span>
              <span>{new Date(source.last_check_at).toLocaleString()}</span>
            </div>
          {/if}
          {#if source.consecutive_failures > 0}
            <div>
              <span class="label">Failures</span>
              <span>{source.consecutive_failures}</span>
            </div>
          {/if}
          {#if source.seed_ratio != null || source.seed_time_minutes != null}
            <div>
              <span class="label">Seeding</span>
              <span>
                {source.seed_ratio != null ? `${source.seed_ratio}x` : ''}
                {source.seed_ratio != null && source.seed_time_minutes != null ? ' / ' : ''}
                {source.seed_time_minutes != null ? `${source.seed_time_minutes}m` : ''}
              </span>
            </div>
          {/if}
          {#if source.api_limit_daily}
            <div>
              <span class="label">API usage</span>
              <span>{source.api_calls_today} / {source.api_limit_daily}</span>
            </div>
          {/if}
          {#if source.search_mode !== 'full'}
            <div>
              <span class="label">Search mode</span>
              <span>{source.search_mode === 'rss_only' ? 'RSS only' : 'Backlog only'}</span>
            </div>
          {/if}
          {#if source.debrid_mode !== 'never'}
            <div>
              <span class="label">Debrid</span>
              <span>{source.debrid_mode === 'always' ? 'Debrid only' : 'Both'}</span>
            </div>
          {/if}
          {#if source.caps_fetched_at}
            <div>
              <span class="label">Caps</span>
              <span>{new Date(source.caps_fetched_at).toLocaleDateString()}</span>
            </div>
          {/if}
          {#if source.last_error}
            <div class="full-width">
              <span class="label">Last error</span>
              <span class="error-text">{source.last_error}</span>
            </div>
          {/if}
        </div>

        <div class="actions">
          <button class="btn alt" type="button" onclick={() => openEditor(source)}>Edit</button>
          {#if source.consecutive_failures > 0}
            <button
              class="btn alt"
              type="button"
              onclick={async () => {
                try {
                  await resetIndexerHealth(source.id);
                  showToast('Indexer health reset');
                  await refresh();
                } catch (err) {
                  error = err instanceof Error ? err.message : 'Failed to reset';
                }
              }}
            >
              Reset & Retry
            </button>
          {/if}
          {#if source.handler_type === 'prowlarr'}
            <button class="btn alt" type="button" onclick={handleSync} disabled={syncing}>
              {syncing ? 'Syncing...' : 'Sync'}
            </button>
            <button class="btn alt" type="button" onclick={() => openMigrateModal(source.id)}>
              Migrate to Native
            </button>
          {/if}
          <button class="btn ghost" type="button" onclick={() => handleDelete(source.id)}>
            Delete
          </button>
        </div>
      </article>
    {/each}
  {/if}
</div>

{#if editorOpen}
  <IndexerEditModal
    source={editing}
    {definitions}
    {handlers}
    {mediaTypes}
    onClose={closeEditor}
    onSaved={handleSaved}
  />
{/if}

{#if migrateModalOpen}
  <Modal
    title="Migrate Prowlarr Indexers to Native"
    width="860px"
    sheet
    onClose={closeMigrateModal}
  >
    {#if migrateLoading}
      <div class="migrate-loading">Loading indexers from Prowlarr...</div>
    {:else if migrateError}
      <div class="migrate-error">{migrateError}</div>
    {:else if migrateResult}
      <div class="migrate-result">
        <p class="migrate-success">
          {migrateResult.created} native indexer source{migrateResult.created !== 1 ? 's' : ''} created.
        </p>
        {#if migrateResult.errors.length > 0}
          <div class="migrate-errors">
            <p>Errors:</p>
            <ul>
              {#each migrateResult.errors as err, i (i)}
                <li>{err}</li>
              {/each}
            </ul>
          </div>
        {/if}
      </div>
    {:else}
      <div class="migrate-summary">
        <span class="chip migrate-new">{migrateCreatable.length} new</span>
        <span class="chip migrate-exists"
          >{migrateItems.filter((i) => i.status.type === 'already_exists').length} existing</span
        >
        {#if migrateItems.some((i) => i.status.type === 'unsupported' || i.status.type === 'missing_fields')}
          <span class="chip migrate-skip"
            >{migrateItems.filter(
              (i) => i.status.type === 'unsupported' || i.status.type === 'missing_fields'
            ).length} skipped</span
          >
        {/if}
      </div>

      <div class="migrate-table-wrap">
        <table class="migrate-table">
          <thead>
            <tr>
              <th class="col-check">
                <input
                  type="checkbox"
                  aria-label="Select all"
                  checked={migrateSelected.size > 0 &&
                    migrateSelected.size === migrateCreatable.length}
                  onchange={() => {
                    migrateSelected =
                      migrateSelected.size === migrateCreatable.length
                        ? new Set()
                        : new Set(migrateCreatable.map((i) => i.prowlarr_id));
                  }}
                />
              </th>
              <th>Name</th>
              <th>Type</th>
              <th>Protocol</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {#each migrateItems as item (item.prowlarr_id)}
              <tr class={migrateStatusClass(item)}>
                <td class="col-check">
                  {#if item.status.type === 'will_create'}
                    <input
                      type="checkbox"
                      aria-label="Select {item.name}"
                      checked={migrateSelected.has(item.prowlarr_id)}
                      onchange={() => toggleMigrateItem(item.prowlarr_id)}
                    />
                  {/if}
                </td>
                <td>{item.name}</td>
                <td><span class="type-tag">{item.handler_type}</span></td>
                <td>{item.protocol}</td>
                <td><span class="status-text">{migrateStatusLabel(item)}</span></td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      {#if migrateClients.length > 0}
        <details class="migrate-clients-details">
          <summary
            >Download Clients ({migrateClients.filter((c) => c.match_status === 'matched')
              .length}/{migrateClients.length} matched)</summary
          >
          <table class="migrate-table">
            <thead>
              <tr>
                <th>Prowlarr Client</th>
                <th>Type</th>
                <th>Kobload Match</th>
              </tr>
            </thead>
            <tbody>
              {#each migrateClients as client (client.prowlarr_name)}
                <tr>
                  <td>{client.prowlarr_name}</td>
                  <td>{client.prowlarr_implementation} ({client.prowlarr_protocol})</td>
                  <td>
                    {#if client.match_status === 'matched'}
                      <span class="match-ok">{client.kobload_client_name}</span>
                    {:else}
                      <span class="match-none">No match</span>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </details>
      {/if}
    {/if}

    {#snippet footer()}
      {#if migrateResult || migrateLoading || migrateError}
        <button class="btn compact alt" type="button" onclick={closeMigrateModal}>Close</button>
      {:else}
        <button class="btn compact alt" type="button" onclick={closeMigrateModal}>Cancel</button>
        <button
          class="btn compact"
          type="button"
          disabled={migrateSelected.size === 0 || migrateConfirming}
          onclick={handleMigrateConfirm}
        >
          {migrateConfirming
            ? 'Migrating...'
            : `Migrate ${migrateSelected.size} indexer${migrateSelected.size !== 1 ? 's' : ''}`}
        </button>
      {/if}
    {/snippet}
  </Modal>
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
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 16px;
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
    white-space: nowrap;
  }

  .status-chip.auth-failed {
    background: rgba(220, 53, 69, 0.15);
    color: #dc3545;
  }

  .auth-notice {
    margin: 12px 0 0;
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 12px;
    line-height: 1.5;
    background: rgba(220, 53, 69, 0.1);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: var(--ink);
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

  .meta-grid .full-width {
    grid-column: 1 / -1;
  }

  .error-text {
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

  .test-result {
    margin-bottom: 16px;
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    background: rgba(40, 167, 69, 0.15);
    border: 1px solid rgba(40, 167, 69, 0.3);
    color: #28a745;
  }

  /* Migration modal */
  .migrate-loading,
  .migrate-error {
    padding: 24px;
    text-align: center;
    color: var(--muted);
  }

  .migrate-error {
    color: #dc3545;
  }

  .migrate-summary {
    display: flex;
    gap: 8px;
    margin-bottom: 16px;
    flex-wrap: wrap;
  }

  .chip {
    font-size: 12px;
    padding: 4px 10px;
    border-radius: 6px;
    font-weight: 500;
  }

  .chip.migrate-new {
    background: rgba(40, 167, 69, 0.15);
    color: #28a745;
  }

  .chip.migrate-exists {
    background: rgba(255, 193, 7, 0.15);
    color: #ffc107;
  }

  .chip.migrate-skip {
    background: rgba(108, 117, 125, 0.15);
    color: #6c757d;
  }

  .migrate-table-wrap {
    overflow-x: auto;
  }

  .migrate-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }

  .migrate-table th,
  .migrate-table td {
    padding: 8px 12px;
    text-align: left;
    border-bottom: 1px solid var(--ring);
  }

  .migrate-table th {
    color: var(--muted);
    font-weight: 500;
    font-size: 12px;
  }

  .col-check {
    width: 32px;
  }

  .type-tag {
    background: var(--tag-bg);
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
  }

  tr.migrate-exists,
  tr.migrate-skip {
    opacity: 0.5;
  }

  .migrate-clients-details {
    margin-top: 16px;
    font-size: 13px;
  }

  .migrate-clients-details summary {
    cursor: pointer;
    color: var(--muted);
    margin-bottom: 8px;
  }

  .match-ok {
    color: #28a745;
  }

  .match-none {
    color: var(--muted);
  }

  .migrate-result {
    padding: 16px 0;
  }

  .migrate-success {
    color: #28a745;
    font-size: 15px;
    margin-bottom: 12px;
  }

  .migrate-errors {
    color: #dc3545;
    font-size: 13px;
  }

  .migrate-errors ul {
    margin: 4px 0;
    padding-left: 20px;
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
