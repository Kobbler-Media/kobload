<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import {
    InfiniteScroll,
    MultiSelectBar,
    DeleteConfirmModal,
    ScrollToTop,
    SearchSelect
  } from '@koblibs/ui';
  import type { SearchSelectItem } from '@koblibs/ui';
  import { createDebouncedSseHandler } from '@koblibs/ui';
  import {
    listWatchMatches,
    listWatchPatterns,
    rejectWatchMatch,
    bulkRejectWatchMatches,
    bulkDeleteWatchMatches
  } from '$lib/api/client';
  import { createSseConnection, type SseConnection } from '$lib/sse';
  import type { WatchMatch, WatchPattern } from '$lib/types';
  import { getPatternTitle } from '$lib/components/watch/grouping';
  import { getScoreColor, getMatchActionStyle } from '$lib/styles/statusColors';
  import { showToast } from '$lib/stores/toast.svelte';

  let loading = $state(false);
  let error = $state('');
  let matches = $state<WatchMatch[]>([]);
  let patterns = $state<WatchPattern[]>([]);
  let nextCursor = $state<string | null>(null);
  let hasMore = $state(true);

  // Filters from URL
  let filterPatternId = $state('');
  let filterAction = $state('');
  let filterRejected = $state<'' | 'true' | 'false'>('');
  let filterSearch = $state('');
  let searchTimeout: ReturnType<typeof setTimeout> | null = null;

  // Selection
  let selectedIds = $state(new Set<string>());
  let showBulkRejectConfirm = $state(false);
  let showBulkDeleteConfirm = $state(false);
  let bulkLoading = $state(false);

  let rejectingId = $state<string | null>(null);
  let sseConnection: SseConnection | null = null;

  const selectionCount = $derived(selectedIds.size);

  const patternMap = $derived(new Map(patterns.map((p) => [p.id, p])));

  const patternFilterItems = $derived<SearchSelectItem[]>([
    { value: '', label: 'All patterns' },
    ...patterns.map((p) => ({ value: p.id, label: getPatternTitle(p) }))
  ]);

  const actionItems: SearchSelectItem[] = [
    { value: '', label: 'All' },
    { value: 'downloaded', label: 'Downloaded' },
    { value: 'callback_sent', label: 'Notified' },
    { value: 'held', label: 'Held' },
    { value: 'skipped', label: 'Skipped' },
    { value: 'error', label: 'Error' }
  ];

  const rejectedItems: SearchSelectItem[] = [
    { value: '', label: 'All' },
    { value: 'true', label: 'Rejected only' },
    { value: 'false', label: 'Not rejected' }
  ];

  function getPatternName(patternId: string | null): string {
    if (!patternId) return 'Manual';
    const p = patternMap.get(patternId);
    if (!p) return 'Unknown';
    return getPatternTitle(p);
  }

  async function loadMatches(reset = false) {
    if (loading) return;
    loading = true;
    error = '';
    try {
      const params: Parameters<typeof listWatchMatches>[0] = { limit: 50 };
      if (filterPatternId) params.watch_pattern_id = filterPatternId;
      if (filterAction) params.action = filterAction;
      if (filterRejected === 'true') params.rejected = true;
      else if (filterRejected === 'false') params.rejected = false;
      if (filterSearch) params.search = filterSearch;
      if (!reset && nextCursor) params.cursor = nextCursor;

      const response = await listWatchMatches(params);
      if (reset) {
        matches = response.items;
      } else {
        matches = [...matches, ...response.items];
      }
      nextCursor = response.next_cursor;
      hasMore = response.next_cursor !== null;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load match history';
    } finally {
      loading = false;
    }
  }

  async function loadPatterns() {
    try {
      const resp = await listWatchPatterns({ limit: 200 });
      patterns = resp.items;
    } catch {
      // Non-critical
    }
  }

  function refresh() {
    nextCursor = null;
    loadMatches(true);
  }

  function updateUrlParams() {
    const url = new URL($page.url);
    if (filterPatternId) url.searchParams.set('watch_pattern_id', filterPatternId);
    else url.searchParams.delete('watch_pattern_id');
    if (filterAction) url.searchParams.set('action', filterAction);
    else url.searchParams.delete('action');
    if (filterRejected) url.searchParams.set('rejected', filterRejected);
    else url.searchParams.delete('rejected');
    goto(url.toString(), { replaceState: true, keepFocus: true });
  }

  function restoreFromUrl() {
    filterPatternId = $page.url.searchParams.get('watch_pattern_id') ?? '';
    filterAction = $page.url.searchParams.get('action') ?? '';
    const rej = $page.url.searchParams.get('rejected');
    filterRejected = rej === 'true' ? 'true' : rej === 'false' ? 'false' : '';
  }

  function applyFilter() {
    updateUrlParams();
    refresh();
  }

  // SSE
  function connectSse() {
    const handler = createDebouncedSseHandler<WatchMatch[]>(
      {
        onSnapshot: (snapshotItems) => {
          if (matches.length === 0) {
            matches = snapshotItems;
            hasMore = snapshotItems.length >= 50;
          } else {
            const snapshotIds = new Set(snapshotItems.map((m) => m.id));
            const beyond = matches.filter((m) => !snapshotIds.has(m.id));
            matches = [...snapshotItems, ...beyond];
          }
        },
        onMutation: (events) => {
          for (const evt of events) {
            const type = evt.type as string;
            if (type === 'created') {
              const m = evt.watch_match as WatchMatch;
              matches = [m, ...matches.filter((existing) => existing.id !== m.id)];
            }
          }
        }
      },
      'matches'
    );

    sseConnection = createSseConnection(
      '/api/v1/watch-matches/stream',
      (payload) => handler(payload),
      (err) => {
        if (err) error = err;
      }
    );
  }

  // Actions
  async function handleReject(matchId: string) {
    rejectingId = matchId;
    try {
      await rejectWatchMatch(matchId);
      matches = matches.map((m) =>
        m.id === matchId ? { ...m, rejected: true, rejected_at: new Date().toISOString() } : m
      );
      showToast('Match rejected');
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to reject match';
    } finally {
      rejectingId = null;
    }
  }

  function toggleSelect(matchId: string) {
    if (selectedIds.has(matchId)) {
      selectedIds.delete(matchId);
    } else {
      selectedIds.add(matchId);
    }
    selectedIds = new Set(selectedIds);
  }

  function clearSelection() {
    selectedIds = new Set();
  }

  async function handleBulkReject() {
    bulkLoading = true;
    try {
      const count = selectedIds.size;
      await bulkRejectWatchMatches([...selectedIds]);
      matches = matches.map((m) =>
        selectedIds.has(m.id) ? { ...m, rejected: true, rejected_at: new Date().toISOString() } : m
      );
      selectedIds = new Set();
      showBulkRejectConfirm = false;
      showToast(`${count} match${count !== 1 ? 'es' : ''} rejected`);
    } catch (err) {
      error = err instanceof Error ? err.message : 'Bulk reject failed';
    } finally {
      bulkLoading = false;
    }
  }

  async function handleBulkDelete() {
    bulkLoading = true;
    try {
      const count = selectedIds.size;
      await bulkDeleteWatchMatches([...selectedIds]);
      matches = matches.filter((m) => !selectedIds.has(m.id));
      selectedIds = new Set();
      showBulkDeleteConfirm = false;
      showToast(`${count} match${count !== 1 ? 'es' : ''} deleted`);
    } catch (err) {
      error = err instanceof Error ? err.message : 'Bulk delete failed';
    } finally {
      bulkLoading = false;
    }
  }

  const formatRelative = (date: string): string => {
    const diff = Date.now() - new Date(date).getTime();
    const minutes = Math.floor(diff / 60000);
    if (minutes < 1) return 'Just now';
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  };

  onMount(() => {
    restoreFromUrl();
    loadMatches(true);
    loadPatterns();
    connectSse();
  });

  onDestroy(() => {
    sseConnection?.stop();
    if (searchTimeout) clearTimeout(searchTimeout);
  });

  function handleSearchInput(e: Event) {
    const value = (e.target as HTMLInputElement).value;
    filterSearch = value;
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {
      applyFilter();
    }, 300);
  }
</script>

{#if filterPatternId && patternMap.get(filterPatternId)}
  {@const fp = patternMap.get(filterPatternId)}
  <nav class="breadcrumbs">
    <a href="/watch/patterns">Patterns</a>
    <span class="sep">›</span>
    {#if fp}
      <a href="/watch/patterns?detail={fp.source_system}/{fp.source_id}">{getPatternTitle(fp)}</a>
      <span class="sep">›</span>
    {/if}
    <span class="current">Matches</span>
  </nav>
{/if}

<div class="page-header">
  <div>
    <h1>Match History</h1>
    <p class="muted">
      {matches.length} matches
      {#if filterPatternId && patternMap.get(filterPatternId)}
        for {getPatternTitle(patternMap.get(filterPatternId)!)}
      {/if}
    </p>
  </div>
  <button class="btn compact alt" type="button" onclick={refresh} disabled={loading}>Refresh</button
  >
</div>

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

<div class="filters">
  <input
    type="text"
    class="search-input"
    placeholder="Search releases..."
    value={filterSearch}
    oninput={handleSearchInput}
  />
  <label>
    Pattern
    <SearchSelect
      id="filter-pattern"
      items={patternFilterItems}
      value={filterPatternId}
      onchange={(v) => {
        filterPatternId = v;
        applyFilter();
      }}
      placeholder="All patterns"
    />
  </label>
  <label>
    Action
    <SearchSelect
      id="filter-action"
      items={actionItems}
      value={filterAction}
      onchange={(v) => {
        filterAction = v;
        applyFilter();
      }}
      placeholder="All"
    />
  </label>
  <label>
    Rejected
    <SearchSelect
      id="filter-rejected"
      items={rejectedItems}
      value={filterRejected}
      onchange={(v) => {
        filterRejected = v as '' | 'true' | 'false';
        applyFilter();
      }}
      placeholder="All"
    />
  </label>
</div>

<div class="timeline">
  {#if loading && matches.length === 0}
    <div class="loading-state">Loading…</div>
  {:else if matches.length === 0}
    <div class="empty-state">No matches found.</div>
  {:else}
    {#each matches as match (match.id)}
      {@const act = getMatchActionStyle(match.action)}
      {@const mp = match.watch_pattern_id ? patternMap.get(match.watch_pattern_id) : undefined}
      <article class="match-item" class:rejected={match.rejected}>
        <div class="match-select">
          {#if selectionCount > 0}
            <input
              type="checkbox"
              checked={selectedIds.has(match.id)}
              onchange={() => toggleSelect(match.id)}
            />
          {/if}
        </div>
        <div class="match-time">
          <span class="time" title={new Date(match.created_at).toLocaleString()}>
            {formatRelative(match.created_at)}
          </span>
        </div>
        <div class="match-content">
          <div class="match-header">
            <h3 class="release-title">{match.release_title}</h3>
            <div class="match-badges">
              {#if match.quality_score !== null}
                <span class="quality-badge">Q:{(match.quality_score * 100).toFixed(0)}</span>
              {/if}
              <span class="score-badge" style="background: {getScoreColor(match.match_score)}">
                {(match.match_score * 100).toFixed(0)}%
              </span>
              <span class="action-badge" style="background: {act.bg}">{act.label}</span>
              {#if match.rejected}
                <span class="rejected-badge">Rejected</span>
              {/if}
            </div>
          </div>

          <div class="match-meta">
            {#if mp}
              <a
                class="pattern-link"
                href="/watch/patterns?detail={mp.source_system}/{mp.source_id}"
              >
                {getPatternName(match.watch_pattern_id)}
              </a>
            {:else}
              <span>{getPatternName(match.watch_pattern_id)}</span>
            {/if}
            {#if match.matched_pattern}
              <span class="matched">Matched: <code>{match.matched_pattern}</code></span>
            {/if}
            {#if match.episode_key}
              <span class="episode-key">{match.episode_key}</span>
            {/if}
          </div>

          {#if match.rejected && match.rejected_reason}
            <div class="rejection-info">
              <span class="label">Rejection Reason</span>
              <p>{match.rejected_reason}</p>
            </div>
          {/if}

          <div class="match-footer">
            {#if match.download_id}
              <a class="link" href="/downloads?id={match.download_id}">View Download</a>
            {/if}
            {#if match.watch_pattern_id}
              {@const p = patternMap.get(match.watch_pattern_id)}
              {#if p}
                <a class="link" href="/watch/patterns?detail={p.source_system}/{p.source_id}"
                  >View Pattern</a
                >
              {/if}
            {/if}
            {#if !match.rejected}
              <button
                class="btn reject-btn"
                type="button"
                onclick={() => handleReject(match.id)}
                disabled={rejectingId === match.id}
              >
                {rejectingId === match.id ? 'Rejecting…' : 'Reject'}
              </button>
            {/if}
          </div>

          {#if match.callback_error}
            <div class="callback-error">
              <span class="label">Callback Error</span>
              <p>{match.callback_error}</p>
            </div>
          {/if}
        </div>
      </article>
    {/each}
  {/if}
</div>

<InfiniteScroll {hasMore} {loading} onLoadMore={() => loadMatches()} />
<ScrollToTop />

<MultiSelectBar
  count={selectionCount}
  visible={selectionCount > 0}
  onReject={() => {
    showBulkRejectConfirm = true;
  }}
  onDelete={() => {
    showBulkDeleteConfirm = true;
  }}
  onClear={clearSelection}
/>

{#if showBulkRejectConfirm}
  <DeleteConfirmModal
    title="Reject {selectionCount} match{selectionCount === 1 ? '' : 'es'}?"
    message="This will mark the selected matches as rejected."
    confirmLabel="Reject"
    loading={bulkLoading}
    onConfirm={handleBulkReject}
    onCancel={() => {
      showBulkRejectConfirm = false;
    }}
  />
{/if}

{#if showBulkDeleteConfirm}
  <DeleteConfirmModal
    title="Delete {selectionCount} match{selectionCount === 1 ? '' : 'es'}?"
    message="This will permanently delete the selected matches from the history."
    loading={bulkLoading}
    onConfirm={handleBulkDelete}
    onCancel={() => {
      showBulkDeleteConfirm = false;
    }}
  />
{/if}

<style>
  .breadcrumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    margin-bottom: 12px;
  }

  .breadcrumbs a {
    color: var(--accent);
    text-decoration: none;
  }

  .breadcrumbs a:hover {
    text-decoration: underline;
  }

  .breadcrumbs .sep {
    color: var(--muted);
  }

  .breadcrumbs .current {
    color: var(--muted);
  }

  .timeline {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .loading-state,
  .empty-state {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
  }

  .match-item {
    display: flex;
    gap: 12px;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
    padding: 14px 16px;
    transition: opacity 0.2s;
  }

  .match-item.rejected {
    opacity: 0.5;
  }

  .match-item.rejected .release-title {
    text-decoration: line-through;
  }

  .match-select {
    flex-shrink: 0;
    display: flex;
    align-items: flex-start;
    padding-top: 2px;
  }

  .match-time {
    flex-shrink: 0;
    width: 70px;
    text-align: right;
  }

  .match-time .time {
    font-size: 11px;
    color: var(--muted);
  }

  .match-content {
    flex: 1;
    min-width: 0;
  }

  .match-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 6px;
  }

  .release-title {
    margin: 0;
    font-size: 13px;
    font-weight: 500;
    word-break: break-word;
  }

  .match-badges {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  .score-badge,
  .action-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    color: white;
    font-weight: 500;
  }

  .quality-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(59, 130, 246, 0.15);
    color: #3b82f6;
    font-weight: 500;
  }

  .rejected-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
    font-weight: 500;
  }

  .match-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 11px;
    color: var(--muted);
    margin-bottom: 6px;
  }

  .pattern-link {
    color: var(--accent);
    text-decoration: none;
    font-weight: 500;
  }

  .pattern-link:hover {
    text-decoration: underline;
  }

  .matched code {
    font-size: 10px;
    padding: 1px 4px;
    background: var(--bg);
    border-radius: 3px;
    border: 1px solid var(--ring);
  }

  .rejection-info {
    margin-top: 6px;
    padding: 6px 8px;
    background: rgba(239, 68, 68, 0.08);
    border-radius: 6px;
  }

  .rejection-info .label {
    font-size: 10px;
    color: #ef4444;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .rejection-info p {
    margin: 2px 0 0;
    font-size: 12px;
    color: var(--muted);
  }

  .match-footer {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 6px;
  }

  .link {
    font-size: 11px;
    color: var(--accent);
    text-decoration: none;
  }

  .link:hover {
    text-decoration: underline;
  }

  .reject-btn {
    background: transparent;
    border: 1px solid rgba(239, 68, 68, 0.4);
    border-radius: 6px;
    color: #ef4444;
    font-size: 11px;
    padding: 3px 8px;
    cursor: pointer;
  }

  .reject-btn:hover:not(:disabled) {
    background: rgba(239, 68, 68, 0.1);
  }

  .reject-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .callback-error {
    margin-top: 6px;
    padding: 6px 8px;
    background: rgba(239, 68, 68, 0.1);
    border-radius: 6px;
  }

  .callback-error .label {
    font-size: 10px;
    color: #ef4444;
    text-transform: uppercase;
  }

  .callback-error p {
    margin: 2px 0 0;
    font-size: 12px;
    color: #ef4444;
  }

  @media (max-width: 640px) {
    .match-item {
      flex-direction: column;
      gap: 6px;
    }

    .match-time {
      width: auto;
      text-align: left;
    }

    .match-select {
      position: absolute;
      right: 12px;
      top: 12px;
    }
  }
</style>
