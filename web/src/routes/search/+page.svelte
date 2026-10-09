<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import {
    searchAndDownload,
    createDownload,
    listQualityProfiles,
    listMediaTypes,
    type MediaType
  } from '$lib/api/client';
  import type { ProcessedRelease, QualityProfile } from '$lib/types';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import { SearchSelect, type SearchSelectItem } from '@koblibs/ui';

  let query = $state('');
  let mediaTypeKey = $state('');
  let qualityProfileId = $state('');
  let results = $state<ProcessedRelease[]>([]);
  let mediaTypes = $state<MediaType[]>([]);
  let qualityProfiles = $state<QualityProfile[]>([]);
  let searching = $state(false);
  let error = $state('');
  let downloadingGuids = $state<Set<string>>(new Set());

  // Map media type key to quality profile media_kind
  const KIND_MAP: Record<string, string> = {
    ebooks: 'book',
    audiobooks: 'book',
    movies: 'movie',
    tv: 'tv',
    sports: 'tv'
  };

  // Resolve media_kind for currently selected media type
  let selectedMediaKind = $derived(() => {
    if (!mediaTypeKey) return undefined;
    return KIND_MAP[mediaTypeKey];
  });

  // Filter quality profiles by media_kind; hide entirely for books
  let filteredQualityProfiles = $derived(() => {
    const mediaKind = selectedMediaKind();
    if (!mediaKind || mediaKind === 'book') return [];
    return qualityProfiles.filter((qp) => qp.media_kind === mediaKind);
  });

  const mediaTypeSelectItems = $derived<SearchSelectItem[]>([
    { value: '', label: 'All' },
    ...mediaTypes.map((mt) => ({ value: mt.key, label: mt.name }))
  ]);

  const qualityProfileSelectItems = $derived<SearchSelectItem[]>([
    { value: '', label: 'Quality: Any' },
    ...filteredQualityProfiles().map((qp) => ({ value: qp.id, label: qp.name }))
  ]);

  async function loadData() {
    try {
      const [mt, qp] = await Promise.all([listMediaTypes(), listQualityProfiles()]);
      mediaTypes = mt;
      qualityProfiles = qp;
    } catch {
      // Data may not be loaded yet
    }
  }

  async function handleSearch() {
    if (!query.trim()) return;

    searching = true;
    error = '';
    results = [];

    try {
      const data = await searchAndDownload({
        patterns: [query.trim()],
        media_type: mediaTypeKey || undefined,
        auto_download: false
      });
      results = data.results;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Search failed';
    } finally {
      searching = false;
    }
  }

  async function handleDownload(result: ProcessedRelease) {
    if (!result.download_url && !result.magnet_url) return;
    downloadingGuids = new Set([...downloadingGuids, result.guid]);

    try {
      await createDownload({
        source: result.indexer,
        name: result.title,
        protocol: result.protocol,
        download_url: (result.download_url || result.magnet_url)!,
        callback_url: '',
        metadata: {},
        // Carry the selected media type so the backend parses the release with
        // the right parser (e.g. "sports" keeps location/round/session instead
        // of parsing a MotoGP race as a movie titled "MotoGP").
        media_kind: mediaTypeKey || undefined
      });
    } catch (err) {
      error = err instanceof Error ? err.message : 'Download failed';
    } finally {
      downloadingGuids = new Set([...downloadingGuids].filter((g) => g !== result.guid));
    }
  }

  function formatAge(dateStr: string | null | undefined): string {
    if (!dateStr) return '';
    const days = Math.floor((Date.now() - Date.parse(dateStr)) / 86400000);
    if (days < 1) return 'today';
    if (days === 1) return '1d ago';
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  }

  function formatSize(bytes: number): string {
    if (bytes === 0) return '\u2014';
    const units = ['B', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`;
  }

  const handleVisibility = () => {
    if (document.visibilityState === 'visible') {
      loadData();
    }
  };

  onMount(() => {
    loadData();
    document.addEventListener('visibilitychange', handleVisibility);
  });

  onDestroy(() => {
    document.removeEventListener('visibilitychange', handleVisibility);
  });
</script>

<div class="page-header">
  <div>
    <h1>Search</h1>
    <p class="muted">Search across all configured indexers.</p>
  </div>
</div>

<section class="card">
  <form
    class="search-form"
    onsubmit={(e) => {
      e.preventDefault();
      handleSearch();
    }}
  >
    <div class="search-row">
      <input
        type="text"
        class="search-input"
        placeholder="Search for releases..."
        bind:value={query}
        autocomplete="off"
      />
      <div class="profile-select">
        <SearchSelect
          id="media-type-filter"
          items={mediaTypeSelectItems}
          value={mediaTypeKey}
          onchange={(v) => {
            mediaTypeKey = v;
          }}
          placeholder="All"
        />
      </div>
      {#if filteredQualityProfiles().length > 0}
        <div class="profile-select">
          <SearchSelect
            id="quality-profile-filter"
            items={qualityProfileSelectItems}
            value={qualityProfileId}
            onchange={(v) => {
              qualityProfileId = v;
            }}
            placeholder="Quality: Any"
          />
        </div>
      {/if}
      <button class="btn" type="submit" disabled={searching || !query.trim()}>
        {searching ? 'Searching...' : 'Search'}
      </button>
    </div>
  </form>
</section>

{#if error}
  <div class="error-banner">
    <svg
      viewBox="0 0 24 24"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      ><circle cx="12" cy="12" r="10"></circle><line x1="12" y1="8" x2="12" y2="12"></line><line
        x1="12"
        y1="16"
        x2="12.01"
        y2="16"
      ></line></svg
    >
    {error}
  </div>
{/if}

{#if results.length > 0}
  <section class="card" style="margin-bottom: 18px;">
    <h2>{results.length} Results</h2>
    <div class="results-list">
      {#each results as result (result.guid)}
        <div class="result-row">
          <div class="result-info">
            <div class="result-title">{result.title}</div>
            <div class="result-meta">
              <span class="badge protocol">{result.protocol}</span>
              <span class="size">{formatSize(result.size)}</span>
              <span class="indexer">{result.indexer}</span>
              {#if result.publish_date}
                <span class="age">{formatAge(result.publish_date)}</span>
              {/if}
              <span class="score">Score: {(result.total_score ?? 0).toFixed(2)}</span>
            </div>
          </div>
          <button
            class="btn download-btn"
            type="button"
            onclick={() => handleDownload(result)}
            disabled={downloadingGuids.has(result.guid) ||
              (!result.download_url && !result.magnet_url)}
          >
            {downloadingGuids.has(result.guid) ? '...' : '\u2193'}
          </button>
        </div>
      {/each}
    </div>
  </section>
{:else if !searching && query}
  <EmptyState
    icon="search"
    title="No results found"
    message="Try different search terms or filters."
  />
{/if}

<style>
  .card h2 {
    margin: 0 0 12px;
    font-size: 16px;
  }

  .search-form {
    margin: 0;
  }

  .search-row {
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }

  @media (max-width: 768px) {
    .search-row {
      flex-direction: column;
      align-items: stretch;
    }

    .search-input {
      min-width: 0;
    }

    .profile-select {
      min-width: 0;
    }
  }

  .search-input {
    flex: 1;
    min-width: 200px;
    padding: 10px 14px;
    border-radius: 8px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 14px;
  }

  .profile-select {
    min-width: 140px;
  }

  .results-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .result-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 12px;
    border-radius: 8px;
    gap: 12px;
  }

  .result-row:hover {
    background: rgba(255, 255, 255, 0.03);
  }

  :global([data-theme='light']) .result-row:hover {
    background: rgba(0, 0, 0, 0.03);
  }

  .result-info {
    flex: 1;
    min-width: 0;
  }

  .result-title {
    font-size: 13px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .result-meta {
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

  .badge.protocol {
    background: rgba(59, 130, 246, 0.15);
    color: #3b82f6;
  }

  .download-btn {
    padding: 6px 12px;
    font-size: 16px;
    min-width: 36px;
    flex-shrink: 0;
  }
</style>
