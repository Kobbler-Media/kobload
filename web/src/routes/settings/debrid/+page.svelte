<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getDebridProviders,
    deleteDebridProvider,
    testDebridProviderById,
    setupPublicTrackers,
    getHashIndexStats,
    triggerHashIndexImport,
    bootstrapHashIndex,
    type DebridProvider,
    type DebridTestResult,
    type HashIndexStats
  } from '$lib/api/client';
  import { showToast } from '$lib/stores/toast.svelte';
  import type { SearchSelectItem } from '@koblibs/ui';
  import DebridProviderEditModal from '$lib/components/settings/DebridProviderEditModal.svelte';
  import DebridTestResultView from '$lib/components/settings/DebridTestResultView.svelte';

  let loading = $state(false);
  let error = $state('');
  let providers = $state<DebridProvider[]>([]);
  let editorOpen = $state(false);
  let editing = $state<DebridProvider | null>(null);
  let providerTestResults = $state<Record<string, DebridTestResult>>({});
  let providerTesting = $state<Record<string, boolean>>({});
  let setupLoading = $state(false);
  let setupResult = $state<{ created: number; skipped: number; details: string[] } | null>(null);

  // Hash Index state
  let hashStats = $state<HashIndexStats | null>(null);
  let hashStatsLoading = $state(false);
  let hashImporting = $state<Record<string, boolean>>({});

  const providerTypes: SearchSelectItem[] = [
    { value: 'real_debrid', label: 'Real-Debrid' },
    { value: 'alldebrid', label: 'AllDebrid' },
    { value: 'premiumize', label: 'Premiumize' },
    { value: 'torbox', label: 'TorBox' }
  ];

  const providerDisplayNames: Record<string, string> = {
    real_debrid: 'Real-Debrid',
    alldebrid: 'AllDebrid',
    premiumize: 'Premiumize',
    torbox: 'TorBox'
  };

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      providers = await getDebridProviders();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load debrid providers';
    } finally {
      loading = false;
    }
  };

  const openEditor = (p: DebridProvider | null) => {
    editing = p;
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
      await deleteDebridProvider(id);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete debrid provider';
    }
  };

  const handleTestExisting = async (id: string) => {
    providerTesting = { ...providerTesting, [id]: true };
    try {
      const result = await testDebridProviderById(id);
      providerTestResults = { ...providerTestResults, [id]: result };
    } catch (err) {
      providerTestResults = {
        ...providerTestResults,
        [id]: {
          success: false,
          message: err instanceof Error ? err.message : 'Test failed'
        }
      };
    } finally {
      providerTesting = { ...providerTesting, [id]: false };
    }
  };

  const handleSetupPublicTrackers = async () => {
    setupLoading = true;
    setupResult = null;
    try {
      setupResult = await setupPublicTrackers();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to setup public trackers';
    } finally {
      setupLoading = false;
    }
  };

  const importSources = [
    { key: 'rarbg', label: 'Import RARBG' },
    { key: 'tpb', label: 'Import TPB' },
    { key: 'dmm', label: 'Import DMM' },
    { key: 'magnetdb', label: 'Import MagnetDB' },
    { key: 'all', label: 'Import All' }
  ];

  const sourceDisplayNames: Record<string, string> = {
    rarbg: 'RARBG',
    tpb: 'TPB',
    dmm: 'DMM',
    magnetdb: 'MagnetDB',
    live_search: 'Live Search',
    eztv: 'EZTV',
    yts: 'YTS',
    torrentio: 'Torrentio',
    zilean: 'Zilean',
    bitmagnet: 'Bitmagnet'
  };

  const formatCount = (n: number): string => n.toLocaleString();

  const refreshHashStats = async () => {
    hashStatsLoading = true;
    try {
      hashStats = await getHashIndexStats();
    } catch {
      hashStats = null;
    } finally {
      hashStatsLoading = false;
    }
  };

  let bootstrapUploading = $state(false);
  let bootstrapFileInput: HTMLInputElement | undefined = $state();

  const handleBootstrap = async () => {
    const file = bootstrapFileInput?.files?.[0];
    if (!file) {
      showToast('Select a .tsv.gz file first');
      return;
    }
    bootstrapUploading = true;
    try {
      const result = await bootstrapHashIndex(file);
      showToast(`Bootstrap started (${result.file_size_mb} MB uploaded)`);
      // Refresh stats after a short delay to let the job start
      setTimeout(() => refreshHashStats(), 3000);
    } catch (err) {
      const msg = err instanceof Error ? err.message : 'Bootstrap failed';
      showToast(msg, 5000);
    } finally {
      bootstrapUploading = false;
    }
  };

  const handleImport = async (source: string) => {
    hashImporting = { ...hashImporting, [source]: true };
    try {
      await triggerHashIndexImport(source);
      showToast(`Import started: ${sourceDisplayNames[source] ?? source}`);
    } catch (err) {
      const msg = err instanceof Error ? err.message : 'Import failed';
      showToast(msg, 5000);
    } finally {
      hashImporting = { ...hashImporting, [source]: false };
    }
  };

  onMount(() => {
    refresh();
    refreshHashStats();
  });
</script>

<div class="page-header">
  <div>
    <h1>Debrid Providers</h1>
    <p class="muted">
      Configure Real-Debrid, AllDebrid, Premiumize, or TorBox for cached torrent streaming.
    </p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add provider</button>
    <button
      class="btn alt"
      type="button"
      onclick={handleSetupPublicTrackers}
      disabled={setupLoading}
    >
      {setupLoading ? 'Setting up...' : 'Setup Public Trackers'}
    </button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

{#if setupResult}
  <div class="setup-result" class:success={setupResult.created > 0}>
    <strong>Public Trackers:</strong>
    {setupResult.created} created, {setupResult.skipped} skipped
    {#if setupResult.details.length > 0}
      <ul>
        {#each setupResult.details as detail, i (i)}
          <li>{detail}</li>
        {/each}
      </ul>
    {/if}
    <button
      class="dismiss-btn"
      type="button"
      onclick={() => (setupResult = null)}
      aria-label="Dismiss"
    >
      <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor">
        <path
          d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"
        ></path>
      </svg>
    </button>
  </div>
{/if}

<div class="card-grid">
  {#if providers.length === 0 && !loading}
    <div class="empty-state">No debrid providers configured yet.</div>
  {:else}
    {#each providers as provider (provider.id)}
      <article class="card">
        <div class="card-header">
          <div>
            <h3>{provider.name}</h3>
            <p class="muted">
              {providerDisplayNames[provider.provider_type] ?? provider.provider_type}
            </p>
          </div>
          <span class="status-chip" class:enabled={provider.enabled}>
            {provider.enabled ? 'enabled' : 'disabled'}
          </span>
        </div>
        <div class="meta-grid">
          <div>
            <span class="label">Priority</span>
            <span>{provider.priority}</span>
          </div>
          <div>
            <span class="label">API Key</span>
            <span class="masked-key">{provider.api_key.slice(0, 8)}...</span>
          </div>
        </div>
        <div class="actions">
          <button class="btn alt" type="button" onclick={() => openEditor(provider)}>Edit</button>
          <button
            class="btn alt"
            type="button"
            onclick={() => handleTestExisting(provider.id)}
            disabled={providerTesting[provider.id]}
          >
            {providerTesting[provider.id] ? 'Testing...' : 'Test'}
          </button>
          <button class="btn ghost" type="button" onclick={() => handleDelete(provider.id)}>
            Delete
          </button>
        </div>
        {#if providerTestResults[provider.id]}
          <DebridTestResultView result={providerTestResults[provider.id]} />
        {/if}
      </article>
    {/each}
  {/if}
</div>

<section class="card hash-index-card">
  <div class="card-header">
    <div>
      <h2>Hash Index</h2>
      <p class="muted">Pre-indexed torrent hashes for instant debrid availability checks.</p>
    </div>
    <button class="btn alt" type="button" onclick={refreshHashStats} disabled={hashStatsLoading}>
      {hashStatsLoading ? 'Loading...' : 'Refresh'}
    </button>
  </div>

  {#if hashStats}
    <div class="hash-stats">
      <div class="hash-total">
        <span class="hash-total-count">{formatCount(hashStats.total)}</span>
        <span class="hash-total-label">total indexed hashes</span>
      </div>
      {#if Object.keys(hashStats.by_source).length > 0}
        <div class="hash-source-grid">
          {#each Object.entries(hashStats.by_source).sort((a, b) => b[1] - a[1]) as [source, count] (source)}
            <div class="hash-source-item">
              <span class="hash-source-name">{sourceDisplayNames[source] ?? source}</span>
              <span class="hash-source-count">{formatCount(count)}</span>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {:else if !hashStatsLoading}
    <p class="muted">Unable to load hash index stats.</p>
  {/if}

  <div class="bootstrap-section">
    <h3>Bootstrap from File</h3>
    <p class="muted">
      Upload a pre-processed <code>.tsv.gz</code> file to bulk-load the hash index. Merges with existing
      data.
    </p>
    <div class="bootstrap-row">
      <input type="file" accept=".gz,.tsv.gz" bind:this={bootstrapFileInput} class="file-input" />
      <button class="btn" type="button" onclick={handleBootstrap} disabled={bootstrapUploading}>
        {bootstrapUploading ? 'Uploading...' : 'Bootstrap Hash Index'}
      </button>
    </div>
  </div>

  <details class="import-details">
    <summary>Import from raw dumps (advanced)</summary>
    <p class="muted">Import directly from seed files mounted at <code>data/seeds/</code>.</p>
    <div class="actions">
      {#each importSources as { key, label } (key)}
        <button
          class="btn {key === 'all' ? '' : 'alt'}"
          type="button"
          onclick={() => handleImport(key)}
          disabled={hashImporting[key]}
        >
          {hashImporting[key] ? 'Starting...' : label}
        </button>
      {/each}
    </div>
  </details>
</section>

{#if editorOpen}
  <DebridProviderEditModal
    provider={editing}
    {providerTypes}
    {providerDisplayNames}
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

  .card h2,
  .card h3 {
    margin: 0 0 8px;
  }

  .card h2 {
    font-size: 18px;
  }

  .card h3 {
    font-size: 16px;
  }

  .card p.muted {
    margin-bottom: 16px;
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

    .page-header {
      flex-direction: column;
    }

    .header-actions {
      width: 100%;
    }

    .header-actions .btn {
      flex: 1;
    }
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
  }

  input {
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
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

  .masked-key {
    font-family: monospace;
    font-size: 11px;
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

  .setup-result {
    position: relative;
    padding: 12px 36px 12px 14px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(211, 140, 90, 0.12);
    border: 1px solid rgba(211, 140, 90, 0.3);
    color: var(--ink);
  }

  .setup-result.success {
    background: rgba(40, 167, 69, 0.12);
    border-color: rgba(40, 167, 69, 0.3);
  }

  .setup-result ul {
    margin: 8px 0 0;
    padding-left: 20px;
    font-size: 12px;
    color: var(--muted);
  }

  .setup-result li {
    margin-bottom: 2px;
  }

  .dismiss-btn {
    position: absolute;
    top: 10px;
    right: 10px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    background: transparent;
    border: none;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
  }

  .dismiss-btn:hover {
    color: var(--ink);
    background: rgba(255, 255, 255, 0.08);
  }

  :global([data-theme='light']) .dismiss-btn:hover {
    background: rgba(0, 0, 0, 0.08);
  }

  .hash-index-card .card-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 16px;
  }

  .hash-stats {
    margin-bottom: 16px;
  }

  .hash-total {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-bottom: 14px;
  }

  .hash-total-count {
    font-size: 28px;
    font-weight: 600;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
    color: var(--ink);
  }

  .hash-total-label {
    font-size: 13px;
    color: var(--muted);
  }

  .hash-source-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 8px;
  }

  .hash-source-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 12px;
    border-radius: 8px;
    background: var(--bg);
    border: 1px solid var(--ring);
    font-size: 13px;
  }

  .hash-source-name {
    color: var(--muted);
  }

  .hash-source-count {
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    color: var(--ink);
  }

  .bootstrap-section {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--ring);
  }

  .bootstrap-section h3 {
    font-size: 14px;
    font-weight: 600;
    margin: 0 0 4px;
  }

  .bootstrap-section .muted {
    margin-bottom: 10px;
  }

  .bootstrap-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .file-input {
    font-size: 13px;
    color: var(--ink);
  }

  .import-details {
    margin-top: 12px;
  }

  .import-details summary {
    cursor: pointer;
    font-size: 13px;
    color: var(--muted);
    user-select: none;
  }

  .import-details .actions {
    margin-top: 10px;
  }
</style>
