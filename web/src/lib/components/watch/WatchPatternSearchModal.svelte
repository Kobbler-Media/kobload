<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import { searchAndDownload, createDownload, listQualityProfiles } from '#lib/api/client.js';
  import type { MediaWant, QualityProfile, ProcessedRelease, WatchPattern } from '#lib/types.js';
  import { getPatternTitle } from './grouping';

  let {
    pattern,
    onClose
  }: {
    pattern: WatchPattern;
    onClose: () => void;
  } = $props();

  let searchQuery = $state(untrack(() => pattern.patterns.join(' ')));
  let searchLoading = $state(false);
  let searchError = $state('');
  let searchResults = $state<ProcessedRelease[]>([]);
  let searchMode = $state<'patterns' | 'media'>('patterns');
  let callbackUrl = $state(untrack(() => pattern.callback_url ?? ''));
  let downloadError = $state('');
  let downloadingGuid = $state<string | null>(null);
  let mediaKind = $state<
    | 'book'
    | 'audiobook'
    | 'movie'
    | 'tv_episode'
    | 'tv_season'
    | 'comic'
    | 'music'
    | 'game'
    | 'software'
  >('book');
  let mediaForm = $state({
    title: '',
    author: '',
    isbn: '',
    year: '',
    show_name: '',
    season: '',
    episode: '',
    quality_profile_id: '',
    narrator: '',
    issue: '',
    series: '',
    artist: '',
    album: '',
    platform: '',
    name: '',
    version: ''
  });
  let qualityProfiles = $state<QualityProfile[]>([]);
  let qualityLoading = $state(false);
  let qualityError = $state('');

  const mediaKindItems: SearchSelectItem[] = [
    { value: 'book', label: 'Book' },
    { value: 'audiobook', label: 'Audiobook' },
    { value: 'movie', label: 'Movie' },
    { value: 'tv_episode', label: 'TV Episode' },
    { value: 'tv_season', label: 'TV Season' },
    { value: 'comic', label: 'Comic' },
    { value: 'music', label: 'Music' },
    { value: 'game', label: 'Game' },
    { value: 'software', label: 'Software' }
  ];

  const qualityProfileItems = $derived<SearchSelectItem[]>([
    { value: '', label: 'None' },
    ...qualityProfiles.map((p) => ({ value: p.id, label: p.name }))
  ]);

  const loadQualityProfiles = async () => {
    if (qualityLoading || qualityProfiles.length > 0) return;
    qualityLoading = true;
    qualityError = '';
    try {
      qualityProfiles = await listQualityProfiles();
    } catch (err) {
      qualityError = err instanceof Error ? err.message : 'Failed to load quality profiles';
    } finally {
      qualityLoading = false;
    }
  };

  // Load quality profiles on mount
  $effect(() => {
    loadQualityProfiles();
  });

  const formatAge = (dateStr: string | null | undefined): string => {
    if (!dateStr) return '';
    const diffMs = Date.now() - Date.parse(dateStr);
    const days = Math.floor(diffMs / 86400000);
    if (days < 1) return 'today';
    if (days === 1) return '1d ago';
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.floor(days / 30)}mo ago`;
    return `${Math.floor(days / 365)}y ago`;
  };

  const scoreColor = (score: number | null | undefined): string => {
    if (score == null) return 'var(--muted)';
    if (score >= 0.9) return '#22c55e';
    if (score >= 0.75) return '#eab308';
    return '#ef4444';
  };

  const buildMediaWant = (): MediaWant | null => {
    const qp = mediaForm.quality_profile_id || null;
    if (mediaKind === 'book') {
      if (!mediaForm.title.trim()) return null;
      return {
        kind: 'book',
        title: mediaForm.title.trim(),
        author: mediaForm.author.trim() || null,
        isbn: mediaForm.isbn.trim() || null
      };
    }
    if (mediaKind === 'movie') {
      if (!mediaForm.title.trim()) return null;
      return {
        kind: 'movie',
        title: mediaForm.title.trim(),
        year: mediaForm.year ? Number.parseInt(mediaForm.year, 10) : null,
        quality_profile_id: qp
      };
    }
    if (mediaKind === 'tv_episode') {
      if (!mediaForm.show_name.trim() || !mediaForm.season || !mediaForm.episode) return null;
      return {
        kind: 'tv_episode',
        show_name: mediaForm.show_name.trim(),
        season: Number.parseInt(mediaForm.season, 10),
        episode: Number.parseInt(mediaForm.episode, 10),
        year: mediaForm.year ? Number.parseInt(mediaForm.year, 10) : null,
        quality_profile_id: qp
      };
    }
    if (mediaKind === 'tv_season') {
      if (!mediaForm.show_name.trim() || !mediaForm.season) return null;
      return {
        kind: 'tv_season',
        show_name: mediaForm.show_name.trim(),
        season: Number.parseInt(mediaForm.season, 10),
        year: mediaForm.year ? Number.parseInt(mediaForm.year, 10) : null,
        quality_profile_id: qp
      };
    }
    if (mediaKind === 'audiobook') {
      if (!mediaForm.title.trim()) return null;
      return {
        kind: 'audiobook',
        title: mediaForm.title.trim(),
        author: mediaForm.author.trim() || null,
        narrator: mediaForm.narrator.trim() || null
      };
    }
    if (mediaKind === 'comic') {
      if (!mediaForm.title.trim()) return null;
      return {
        kind: 'comic',
        title: mediaForm.title.trim(),
        issue: mediaForm.issue ? Number.parseInt(mediaForm.issue, 10) : null,
        series: mediaForm.series.trim() || null,
        year: mediaForm.year ? Number.parseInt(mediaForm.year, 10) : null
      };
    }
    if (mediaKind === 'music') {
      if (!mediaForm.artist.trim()) return null;
      return {
        kind: 'music',
        artist: mediaForm.artist.trim(),
        album: mediaForm.album.trim() || null,
        year: mediaForm.year ? Number.parseInt(mediaForm.year, 10) : null
      };
    }
    if (mediaKind === 'game') {
      if (!mediaForm.title.trim()) return null;
      return {
        kind: 'game',
        title: mediaForm.title.trim(),
        platform: mediaForm.platform.trim() || null,
        year: mediaForm.year ? Number.parseInt(mediaForm.year, 10) : null
      };
    }
    if (mediaKind === 'software') {
      if (!mediaForm.name.trim()) return null;
      return {
        kind: 'software',
        name: mediaForm.name.trim(),
        version: mediaForm.version.trim() || null,
        platform: mediaForm.platform.trim() || null
      };
    }
    return null;
  };

  const runSearch = async () => {
    searchLoading = true;
    searchError = '';
    downloadError = '';
    try {
      const mediaWant = searchMode === 'media' ? buildMediaWant() : null;
      if (searchMode === 'media' && !mediaWant) {
        searchError = 'Please fill in the required fields for structured search.';
        return;
      }
      const result = await searchAndDownload({
        patterns:
          searchMode === 'media'
            ? []
            : searchQuery
                .split(/[,\n]/)
                .map((p) => p.trim())
                .filter((p) => p.length > 0),
        media_want: mediaWant,
        media_type: pattern.media_type ?? undefined,
        auto_download: false,
        min_score: 0
      });
      searchResults = result.results;
    } catch (err) {
      searchError = err instanceof Error ? err.message : 'Search failed';
    } finally {
      searchLoading = false;
    }
  };

  const submitDownload = async (result: ProcessedRelease) => {
    downloadError = '';
    downloadingGuid = result.guid;

    const callback = callbackUrl.trim();
    if (!callback) {
      downloadError = 'Callback URL is required to receive completion events.';
      downloadingGuid = null;
      return;
    }

    const url = result.magnet_url || result.download_url;
    if (!url) {
      downloadError = 'Selected result has no download URL.';
      downloadingGuid = null;
      return;
    }

    try {
      await createDownload({
        source: 'manual',
        protocol: result.protocol,
        name: result.file_name ?? result.title,
        download_url: url,
        callback_url: callback,
        metadata: {
          ...pattern.metadata,
          origin: 'manual-search',
          pattern_id: pattern.id,
          media_want: searchMode === 'media' ? buildMediaWant() : null
        },
        release: {
          guid: result.guid,
          title: result.title,
          file_name: result.file_name,
          size: result.size,
          indexer: result.indexer,
          protocol: result.protocol,
          download_url: result.download_url,
          magnet_url: result.magnet_url,
          info_url: result.info_url,
          publish_date: result.publish_date,
          indexer_source_id: result.indexer_source_id
        }
      });
    } catch (err) {
      downloadError = err instanceof Error ? err.message : 'Download failed';
    } finally {
      downloadingGuid = null;
    }
  };
</script>

<Modal title="Manual Search" {onClose} width="min(640px, 90vw)">
  <p class="muted">{getPatternTitle(pattern)}</p>

  {#if searchError}
    <div class="error">{searchError}</div>
  {/if}

  <form
    onsubmit={(e) => {
      e.preventDefault();
      runSearch();
    }}
  >
    <div class="mode-toggle">
      <label class="radio-label">
        <input type="radio" name="searchMode" value="patterns" bind:group={searchMode} />
        Patterns
      </label>
      <label class="radio-label">
        <input type="radio" name="searchMode" value="media" bind:group={searchMode} />
        Structured
      </label>
    </div>

    {#if searchMode === 'patterns'}
      <label>
        Search Query
        <textarea bind:value={searchQuery} rows="3" placeholder="Enter search terms..."></textarea>
      </label>
    {:else}
      <div class="media-form">
        <label>
          Media Kind
          <SearchSelect
            id="media-kind"
            items={mediaKindItems}
            value={mediaKind}
            onchange={(v) => {
              mediaKind = v as typeof mediaKind;
            }}
            placeholder="Select..."
          />
        </label>

        {#if mediaKind === 'book'}
          <label>
            Title
            <input type="text" bind:value={mediaForm.title} placeholder="Book title" />
          </label>
          <label>
            Author (optional)
            <input type="text" bind:value={mediaForm.author} placeholder="Author" />
          </label>
          <label>
            ISBN (optional)
            <input type="text" bind:value={mediaForm.isbn} placeholder="ISBN" />
          </label>
        {:else if mediaKind === 'movie'}
          <label>
            Title
            <input type="text" bind:value={mediaForm.title} placeholder="Movie title" />
          </label>
          <label>
            Year (optional)
            <input type="text" bind:value={mediaForm.year} placeholder="2024" />
          </label>
        {:else if mediaKind === 'tv_episode'}
          <label>
            Show Name
            <input type="text" bind:value={mediaForm.show_name} placeholder="Show name" />
          </label>
          <label>
            Season
            <input type="text" bind:value={mediaForm.season} placeholder="1" />
          </label>
          <label>
            Episode
            <input type="text" bind:value={mediaForm.episode} placeholder="1" />
          </label>
          <label>
            Year (optional)
            <input type="text" bind:value={mediaForm.year} placeholder="2024" />
          </label>
        {:else if mediaKind === 'tv_season'}
          <label>
            Show Name
            <input type="text" bind:value={mediaForm.show_name} placeholder="Show name" />
          </label>
          <label>
            Season
            <input type="text" bind:value={mediaForm.season} placeholder="1" />
          </label>
          <label>
            Year (optional)
            <input type="text" bind:value={mediaForm.year} placeholder="2024" />
          </label>
        {:else if mediaKind === 'audiobook'}
          <label>
            Title
            <input type="text" bind:value={mediaForm.title} placeholder="Audiobook title" />
          </label>
          <label>
            Author (optional)
            <input type="text" bind:value={mediaForm.author} placeholder="Author" />
          </label>
          <label>
            Narrator (optional)
            <input type="text" bind:value={mediaForm.narrator} placeholder="Narrator" />
          </label>
        {:else if mediaKind === 'comic'}
          <label>
            Title
            <input type="text" bind:value={mediaForm.title} placeholder="Comic title" />
          </label>
          <label>
            Issue (optional)
            <input type="text" bind:value={mediaForm.issue} placeholder="1" />
          </label>
          <label>
            Series (optional)
            <input type="text" bind:value={mediaForm.series} placeholder="Series name" />
          </label>
          <label>
            Year (optional)
            <input type="text" bind:value={mediaForm.year} placeholder="2024" />
          </label>
        {:else if mediaKind === 'music'}
          <label>
            Artist
            <input type="text" bind:value={mediaForm.artist} placeholder="Artist name" />
          </label>
          <label>
            Album (optional)
            <input type="text" bind:value={mediaForm.album} placeholder="Album name" />
          </label>
          <label>
            Year (optional)
            <input type="text" bind:value={mediaForm.year} placeholder="2024" />
          </label>
        {:else if mediaKind === 'game'}
          <label>
            Title
            <input type="text" bind:value={mediaForm.title} placeholder="Game title" />
          </label>
          <label>
            Platform (optional)
            <input type="text" bind:value={mediaForm.platform} placeholder="e.g. PC, PS5, Switch" />
          </label>
          <label>
            Year (optional)
            <input type="text" bind:value={mediaForm.year} placeholder="2024" />
          </label>
        {:else if mediaKind === 'software'}
          <label>
            Name
            <input type="text" bind:value={mediaForm.name} placeholder="Software name" />
          </label>
          <label>
            Version (optional)
            <input type="text" bind:value={mediaForm.version} placeholder="e.g. 3.0" />
          </label>
          <label>
            Platform (optional)
            <input
              type="text"
              bind:value={mediaForm.platform}
              placeholder="e.g. Windows, macOS, Linux"
            />
          </label>
        {/if}

        {#if mediaKind === 'movie' || mediaKind === 'tv_episode' || mediaKind === 'tv_season'}
          <label>
            Quality Profile (optional)
            <SearchSelect
              id="quality-profile"
              items={qualityProfileItems}
              value={mediaForm.quality_profile_id}
              onchange={(v) => {
                mediaForm.quality_profile_id = v;
              }}
              placeholder="None"
            />
          </label>

          {#if qualityError}
            <div class="error">{qualityError}</div>
          {/if}
        {/if}
      </div>
    {/if}

    <div class="form-actions">
      <button class="btn compact" type="submit" disabled={searchLoading}>
        {searchLoading ? 'Searching...' : 'Search'}
      </button>
      <button class="btn compact alt" type="button" onclick={onClose}>Close</button>
    </div>
  </form>

  <label class="callback-field">
    Callback URL (required for downloads)
    <input
      type="url"
      bind:value={callbackUrl}
      placeholder="https://client.example.com/api/kobload/callback"
    />
  </label>

  {#if downloadError}
    <div class="error">{downloadError}</div>
  {/if}

  {#if searchResults.length > 0}
    <div class="search-results">
      <h3>Results ({searchResults.length})</h3>
      <div class="results-list">
        {#each searchResults as result (result.guid)}
          <div class="result-item">
            <div class="result-header">
              <span class="result-title">{result.title}</span>
              <span class="score" style="background: {scoreColor(result.match_score ?? 0)}">
                {((result.match_score ?? 0) * 100).toFixed(0)}%
              </span>
            </div>
            <div class="result-meta">
              <span>{result.indexer}</span>
              <span>{result.protocol}</span>
              <span>{(result.size / 1024 / 1024).toFixed(1)} MB</span>
              {#if result.publish_date}
                <span>{formatAge(result.publish_date)}</span>
              {/if}
              {#if result.quality_score !== undefined}
                <span>Quality {(result.quality_score ?? 0) * 100}%</span>
              {/if}
            </div>
            <div class="result-pattern">
              {#if result.matched_pattern}Matched: <code>{result.matched_pattern}</code>{/if}
            </div>
            {#if result.total_score !== undefined}
              <div class="result-pattern">
                Total Score: {((result.total_score ?? 0) * 100).toFixed(0)}%
              </div>
            {/if}
            <div class="result-actions">
              <button
                class="btn compact alt"
                type="button"
                onclick={() => submitDownload(result)}
                disabled={downloadingGuid === result.guid}
              >
                {downloadingGuid === result.guid ? 'Downloading...' : 'Download'}
              </button>
            </div>
          </div>
        {/each}
      </div>
    </div>
  {:else if !searchLoading && (searchQuery || searchMode === 'media')}
    <p class="muted no-results">No results found. Try different search terms.</p>
  {/if}
</Modal>

<style>
  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
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

  form {
    margin-top: 16px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 12px;
  }

  input,
  textarea {
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
  }

  textarea {
    font-family: monospace;
    resize: vertical;
  }

  .mode-toggle {
    display: flex;
    gap: 16px;
    margin-bottom: 12px;
    font-size: 13px;
  }

  .radio-label {
    flex-direction: row;
    align-items: center;
    gap: 6px;
    margin-bottom: 0;
  }

  .media-form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 12px;
    margin-bottom: 12px;
  }

  .media-form label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
  }

  .media-form input {
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
  }

  .form-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .callback-field {
    margin-top: 12px;
  }

  .search-results {
    margin-top: 20px;
    border-top: 1px solid var(--ring);
    padding-top: 16px;
  }

  .search-results h3 {
    margin: 0 0 12px;
    font-size: 14px;
  }

  .results-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
    max-height: 300px;
    overflow-y: auto;
  }

  .result-item {
    padding: 12px;
    background: var(--bg);
    border-radius: 8px;
    border: 1px solid var(--ring);
  }

  .result-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 8px;
    margin-bottom: 6px;
  }

  .result-title {
    font-size: 13px;
    font-weight: 500;
    word-break: break-word;
  }

  .score {
    font-size: 11px;
    padding: 2px 6px;
    border-radius: 4px;
    color: white;
    flex-shrink: 0;
  }

  .result-meta {
    display: flex;
    gap: 12px;
    font-size: 11px;
    color: var(--muted);
    margin-bottom: 6px;
  }

  .result-pattern {
    font-size: 11px;
    color: var(--muted);
  }

  .result-pattern code {
    padding: 1px 4px;
    background: var(--card);
    border-radius: 3px;
  }

  .result-actions {
    margin-top: 8px;
    display: flex;
    justify-content: flex-end;
  }

  .no-results {
    text-align: center;
    padding: 16px;
  }
</style>
