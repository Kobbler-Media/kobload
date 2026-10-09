<script lang="ts">
  import { ExpandableSidebar } from '@koblibs/ui';
  import { getPatternDetail, searchPatternNow, grabHeldMatch } from '$lib/api/client';
  import type { WatchPattern, WatchMatch } from '$lib/types';
  import { getEffectiveStatus, getPatternTitle, getSearchTermsFromMediaWant } from './grouping';
  import { formatAge } from '@koblibs/ui';
  import {
    getDetailStatusColor,
    getDetailStatusLabel,
    getMatchActionStyle
  } from '$lib/styles/statusColors';

  let {
    pattern,
    onClose,
    onEdit,
    onSearch,
    onTest,
    onDelete,
    onViewMatch
  }: {
    pattern: WatchPattern;
    onClose: () => void;
    onEdit: (p: WatchPattern) => void;
    onSearch: (p: WatchPattern) => void;
    onTest: (p: WatchPattern) => void;
    onDelete: (p: WatchPattern) => void;
    onViewMatch?: (m: WatchMatch) => void;
  } = $props();

  let matches = $state<WatchMatch[]>([]);
  let loading = $state(true);
  let error = $state('');
  let searchingNow = $state(false);
  let grabbingHeld = $state(false);

  const title = $derived(getPatternTitle(pattern));
  const effectiveStatus = $derived(getEffectiveStatus(pattern));

  async function loadDetail() {
    loading = true;
    error = '';
    try {
      const detail = await getPatternDetail(pattern.source_system, pattern.source_id);
      matches = detail.matches;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load details';
    } finally {
      loading = false;
    }
  }

  async function handleSearchNow() {
    searchingNow = true;
    try {
      await searchPatternNow(pattern.source_system, pattern.source_id);
    } finally {
      searchingNow = false;
    }
  }

  async function handleGrabHeld() {
    grabbingHeld = true;
    try {
      await grabHeldMatch(pattern.source_system, pattern.source_id);
    } finally {
      grabbingHeld = false;
    }
  }

  $effect(() => {
    // Re-load when pattern changes
    void pattern.id;
    loadDetail();
  });

  const mediaWantFields = $derived.by(() => {
    const mw = pattern.media_want;
    if (!mw) return [];
    const fields: { label: string; value: string }[] = [];
    fields.push({ label: 'Kind', value: mw.kind.replace('_', ' ') });
    if ('title' in mw && mw.title) fields.push({ label: 'Title', value: mw.title });
    if ('show_name' in mw) fields.push({ label: 'Show', value: mw.show_name });
    if ('league_name' in mw) fields.push({ label: 'League', value: mw.league_name });
    if ('artist' in mw) fields.push({ label: 'Artist', value: mw.artist });
    if ('name' in mw && mw.kind === 'software') fields.push({ label: 'Name', value: mw.name });
    if ('author' in mw && mw.author) fields.push({ label: 'Author', value: mw.author });
    if ('narrator' in mw && mw.narrator) fields.push({ label: 'Narrator', value: mw.narrator });
    if ('season' in mw && mw.season != null)
      fields.push({ label: 'Season', value: String(mw.season) });
    if ('episode' in mw && mw.episode != null)
      fields.push({ label: 'Episode', value: String(mw.episode) });
    if ('round' in mw && mw.round != null) fields.push({ label: 'Round', value: String(mw.round) });
    if ('year' in mw && mw.year) fields.push({ label: 'Year', value: String(mw.year) });
    if ('album' in mw && mw.album) fields.push({ label: 'Album', value: mw.album });
    if ('isbn' in mw && mw.isbn) fields.push({ label: 'ISBN', value: mw.isbn });
    if ('series' in mw && mw.series) fields.push({ label: 'Series', value: mw.series });
    return fields;
  });

  const metadataFields = $derived.by(() => {
    const meta = pattern.metadata as Record<string, unknown> | null;
    if (!meta) return [];
    return Object.entries(meta)
      .filter(([, v]) => v != null && v !== '')
      .map(([k, v]) => ({
        label: k,
        value: typeof v === 'object' ? JSON.stringify(v, null, 2) : String(v),
        isObject: typeof v === 'object'
      }));
  });
</script>

<ExpandableSidebar open={true} {title} showRefresh={true} {onClose} onRefresh={loadDetail}>
  {#if loading}
    <div class="loading-state">Loading details…</div>
  {:else if error}
    <div class="error-state">{error}</div>
  {:else}
    <!-- Status -->
    <section class="detail-section">
      <div class="status-row">
        <span class="status-pill" style="color: {getDetailStatusColor(effectiveStatus)}">
          {getDetailStatusLabel(effectiveStatus)}
        </span>
        {#if pattern.upgrade_allowed}
          <span class="upgrade-badge">Upgrade</span>
        {/if}
        {#if !pattern.enabled}
          <span class="disabled-badge">Disabled</span>
        {/if}
      </div>
      {#if pattern.episode_progress}
        <div class="episode-progress">
          <div class="progress-bar">
            <div
              class="progress-fill"
              style="width: {(pattern.episode_progress.imported / pattern.episode_progress.total) *
                100}%"
            ></div>
          </div>
          <span class="progress-text">
            {pattern.episode_progress.imported}/{pattern.episode_progress.total} episodes
          </span>
        </div>
      {/if}
    </section>

    <!-- Search Patterns -->
    <section class="detail-section">
      <h4>Search Patterns</h4>
      {#if pattern.patterns.length > 0}
        <div class="pattern-tags">
          {#each pattern.patterns as p, i (`${i}-${p}`)}
            <code class="pattern-tag">{p}</code>
          {/each}
        </div>
      {:else}
        {@const derived = getSearchTermsFromMediaWant(pattern)}
        {#if derived.length > 0}
          <p class="derived-label">Derived from media want:</p>
          <div class="pattern-tags">
            {#each derived as p, i (`derived-${i}-${p}`)}
              <code class="pattern-tag derived">{p}</code>
            {/each}
          </div>
        {:else}
          <p class="empty-text">No search patterns configured.</p>
        {/if}
      {/if}
    </section>

    <!-- Match Settings -->
    <section class="detail-section">
      <h4>Match Settings</h4>
      <div class="detail-grid">
        <div class="detail-field">
          <span class="field-label">Mode</span>
          <span>{pattern.match_mode}</span>
        </div>
        <div class="detail-field">
          <span class="field-label">Min Score</span>
          <span
            >{pattern.min_score != null ? (pattern.min_score * 100).toFixed(0) + '%' : 'None'}</span
          >
        </div>
        <div class="detail-field">
          <span class="field-label">Auto Download</span>
          <span>{pattern.auto_download ? 'Yes' : 'No'}</span>
        </div>
        {#if pattern.best_quality_score != null}
          <div class="detail-field">
            <span class="field-label">Best Quality</span>
            <span>{(pattern.best_quality_score * 100).toFixed(0)}%</span>
          </div>
        {/if}
      </div>
    </section>

    <!-- Media Want -->
    {#if mediaWantFields.length > 0}
      <section class="detail-section">
        <h4>Media Want</h4>
        <div class="detail-grid">
          {#each mediaWantFields as field (field.label)}
            <div class="detail-field">
              <span class="field-label">{field.label}</span>
              <span>{field.value}</span>
            </div>
          {/each}
        </div>
      </section>
    {/if}

    <!-- Metadata -->
    {#if metadataFields.length > 0}
      <section class="detail-section">
        <h4>Metadata</h4>
        <div class="detail-grid">
          {#each metadataFields as field (field.label)}
            <div class="detail-field">
              <span class="field-label">{field.label}</span>
              {#if field.isObject}
                <pre class="field-value metadata-pre">{field.value}</pre>
              {:else}
                <span class="field-value">{field.value}</span>
              {/if}
            </div>
          {/each}
        </div>
      </section>
    {/if}

    <!-- Source Info -->
    <section class="detail-section">
      <h4>Source</h4>
      <div class="detail-grid">
        <div class="detail-field">
          <span class="field-label">System</span>
          <span>{pattern.source_system}</span>
        </div>
        <div class="detail-field">
          <span class="field-label">Source ID</span>
          <span class="field-value mono">{pattern.source_id}</span>
        </div>
        <div class="detail-field">
          <span class="field-label">Created</span>
          <span>{formatAge(pattern.created_at)}</span>
        </div>
        <div class="detail-field">
          <span class="field-label">Updated</span>
          <span>{formatAge(pattern.updated_at)}</span>
        </div>
      </div>
    </section>

    <!-- Recent Matches -->
    <section class="detail-section">
      <h4>Recent Matches ({matches.length})</h4>
      {#if matches.length === 0}
        <p class="empty-text">No matches yet.</p>
      {:else}
        <div class="matches-list">
          {#each matches.slice(0, 15) as match (match.id)}
            {@const act = getMatchActionStyle(match.action)}
            <button class="match-row" type="button" onclick={() => onViewMatch?.(match)}>
              <div class="match-title">{match.release_title}</div>
              <div class="match-meta">
                <span class="match-action" style="color: {act.color}">{act.label}</span>
                <span class="match-score">{(match.match_score * 100).toFixed(0)}%</span>
                <span class="match-age">{formatAge(match.created_at)}</span>
                {#if match.rejected}
                  <span class="match-rejected">Rejected</span>
                {/if}
              </div>
            </button>
          {/each}
          {#if matches.length > 15}
            <a class="view-all-link" href="/watch/history?watch_pattern_id={pattern.id}">
              View all {matches.length} matches →
            </a>
          {/if}
        </div>
      {/if}
    </section>

    <!-- Actions -->
    <section class="detail-actions">
      <button class="btn compact" type="button" onclick={() => onSearch(pattern)}
        >Manual Search</button
      >
      <button
        class="btn compact alt"
        type="button"
        onclick={handleSearchNow}
        disabled={searchingNow}
      >
        {searchingNow ? 'Searching…' : 'Search Now'}
      </button>
      {#if effectiveStatus === 'held'}
        <button
          class="btn compact alt"
          type="button"
          onclick={handleGrabHeld}
          disabled={grabbingHeld}
        >
          {grabbingHeld ? 'Grabbing…' : 'Grab Held'}
        </button>
      {/if}
      <button class="btn compact alt" type="button" onclick={() => onTest(pattern)}>Test</button>
      <button class="btn compact alt" type="button" onclick={() => onEdit(pattern)}>Edit</button>
      <button class="btn compact danger" type="button" onclick={() => onDelete(pattern)}
        >Delete</button
      >
    </section>
  {/if}
</ExpandableSidebar>

<style>
  .loading-state,
  .error-state {
    padding: 24px;
    text-align: center;
    color: var(--muted);
    font-size: 13px;
  }

  .error-state {
    color: #dc3545;
  }

  .detail-section {
    margin-bottom: 20px;
    padding-bottom: 16px;
    border-bottom: 1px solid var(--ring);
  }

  .detail-section:last-of-type {
    border-bottom: none;
  }

  .detail-section h4 {
    margin: 0 0 10px;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .status-pill {
    font-size: 13px;
    font-weight: 600;
  }

  .upgrade-badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 6px;
    background: rgba(168, 85, 247, 0.15);
    color: #a855f7;
  }

  .disabled-badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 6px;
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
  }

  .episode-progress {
    margin-top: 8px;
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .progress-bar {
    flex: 1;
    height: 6px;
    background: var(--ring);
    border-radius: 3px;
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    background: #22c55e;
    border-radius: 3px;
    transition: width 0.3s;
  }

  .progress-text {
    font-size: 12px;
    color: var(--muted);
    white-space: nowrap;
  }

  .pattern-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .pattern-tag {
    font-size: 11px;
    padding: 4px 8px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 4px;
    word-break: break-all;
  }

  .pattern-tag.derived {
    border-style: dashed;
    opacity: 0.8;
  }

  .derived-label {
    font-size: 11px;
    color: var(--muted);
    margin: 0 0 6px;
  }

  .detail-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px 16px;
  }

  .detail-field {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .field-label {
    font-size: 11px;
    color: var(--muted);
  }

  .detail-field span:not(.field-label) {
    font-size: 13px;
  }

  .field-value {
    word-break: break-all;
  }

  .field-value.mono {
    font-family: monospace;
    font-size: 12px;
  }

  pre.metadata-pre {
    margin: 0;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.1);
    border-radius: 4px;
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-all;
  }

  .empty-text {
    color: var(--muted);
    font-size: 13px;
    margin: 0;
  }

  .matches-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 300px;
    overflow-y: auto;
  }

  .match-row {
    display: block;
    width: 100%;
    text-align: left;
    padding: 8px 10px;
    border-radius: 8px;
    background: transparent;
    border: none;
    color: var(--ink);
    cursor: pointer;
    font: inherit;
  }

  .match-row:hover {
    background: rgba(255, 255, 255, 0.03);
  }

  :global([data-theme='light']) .match-row:hover {
    background: rgba(0, 0, 0, 0.03);
  }

  .match-title {
    font-size: 13px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .match-meta {
    display: flex;
    gap: 8px;
    font-size: 11px;
    color: var(--muted);
    margin-top: 2px;
    align-items: center;
  }

  .match-action {
    font-weight: 500;
  }

  .match-rejected {
    color: #ef4444;
    font-weight: 500;
  }

  .view-all-link {
    display: block;
    text-align: center;
    padding: 8px;
    font-size: 12px;
    color: var(--accent);
    text-decoration: none;
  }

  .view-all-link:hover {
    text-decoration: underline;
  }

  .detail-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding-top: 8px;
  }
</style>
