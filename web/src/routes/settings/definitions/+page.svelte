<script lang="ts">
  import { onMount } from 'svelte';
  import { listDefinitions, updateDefinitions } from '#lib/api/client.js';
  import type { DefinitionSummary } from '#lib/api/client.js';

  let definitions = $state<DefinitionSummary[]>([]);
  let loading = $state(true);
  let updating = $state(false);
  let filterText = $state('');
  let error = $state('');

  const filtered = $derived(
    filterText.trim()
      ? definitions.filter(
          (d) =>
            d.name.toLowerCase().includes(filterText.toLowerCase()) ||
            d.description.toLowerCase().includes(filterText.toLowerCase())
        )
      : definitions
  );

  onMount(async () => {
    try {
      definitions = await listDefinitions();
    } catch (e) {
      error = e instanceof Error ? e.message : 'Failed to load definitions';
    } finally {
      loading = false;
    }
  });

  async function handleUpdate() {
    updating = true;
    error = '';
    try {
      const result = await updateDefinitions();
      definitions = await listDefinitions();
      error = '';
      alert(`Updated: ${result.downloaded} files downloaded, ${result.total} definitions loaded.`);
    } catch (e) {
      error = e instanceof Error ? e.message : 'Update failed';
    } finally {
      updating = false;
    }
  }
</script>

<div class="page">
  <div class="page-header">
    <div>
      <h1>Cardigann Definitions</h1>
      <p class="subtitle">
        YAML indexer definitions from Prowlarr/Indexers. Used by sources with the
        <code>cardigann</code> handler type.
      </p>
    </div>
    <button class="btn btn-primary" onclick={handleUpdate} disabled={updating}>
      {updating ? 'Updating...' : 'Update from GitHub'}
    </button>
  </div>

  {#if error}
    <div class="alert alert-error">{error}</div>
  {/if}

  <div class="filter-bar">
    <input type="text" class="input" placeholder="Filter definitions..." bind:value={filterText} />
    <span class="count">{filtered.length} of {definitions.length}</span>
  </div>

  {#if loading}
    <p class="muted">Loading...</p>
  {:else if filtered.length === 0}
    <p class="muted">
      {definitions.length === 0
        ? 'No definitions loaded. Click "Update from GitHub" to download.'
        : 'No definitions match your filter.'}
    </p>
  {:else}
    <div class="def-grid">
      {#each filtered as def (def.name)}
        <div class="def-card">
          <div class="def-header">
            <strong>{def.name}</strong>
            {#if def.language}
              <span class="badge">{def.language}</span>
            {/if}
            {#if def.site_type}
              <span class="badge badge-type">{def.site_type}</span>
            {/if}
          </div>
          <p class="def-desc">{def.description || 'No description'}</p>
          {#if def.links.length > 0}
            <div class="def-links">
              {#each def.links as link (link)}
                <a href={link} target="_blank" rel="noopener">{new URL(link).hostname}</a>
              {/each}
            </div>
          {/if}
          {#if def.settings.length > 0}
            <div class="def-settings">
              <span class="muted"
                >{def.settings.length} setting{def.settings.length > 1 ? 's' : ''}</span
              >
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .page {
    padding: 24px;
    max-width: 1200px;
  }

  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    margin-bottom: 20px;
    gap: 16px;
  }

  .page-header h1 {
    margin: 0 0 4px;
    font-size: 20px;
  }

  .subtitle {
    color: var(--muted);
    font-size: 13px;
    margin: 0;
  }

  .subtitle code {
    background: var(--ring);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 12px;
  }

  .filter-bar {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }

  .filter-bar .input {
    flex: 1;
    max-width: 400px;
  }

  .count {
    color: var(--muted);
    font-size: 12px;
    white-space: nowrap;
  }

  .def-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 12px;
  }

  @media (max-width: 768px) {
    .def-grid {
      grid-template-columns: 1fr;
    }
  }

  .def-card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 8px;
    padding: 14px;
  }

  .def-header {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
    flex-wrap: wrap;
  }

  .def-header strong {
    font-size: 14px;
  }

  .badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--ring);
    color: var(--muted);
    text-transform: uppercase;
  }

  .badge-type {
    background: rgba(211, 140, 90, 0.15);
    color: var(--accent);
  }

  .def-desc {
    font-size: 12px;
    color: var(--muted);
    margin: 0 0 8px;
    line-height: 1.4;
  }

  .def-links {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .def-links a {
    font-size: 11px;
    color: var(--accent);
  }

  .def-settings {
    margin-top: 6px;
    font-size: 11px;
  }

  .muted {
    color: var(--muted);
  }

  .alert-error {
    background: rgba(220, 50, 50, 0.1);
    border: 1px solid rgba(220, 50, 50, 0.3);
    border-radius: 6px;
    padding: 10px 14px;
    color: #dc3232;
    margin-bottom: 16px;
    font-size: 13px;
  }
</style>
