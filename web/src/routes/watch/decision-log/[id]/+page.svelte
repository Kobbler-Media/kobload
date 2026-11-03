<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { DeleteConfirmModal } from '@koblibs/ui';
  import {
    getDecisionLogSession,
    exportDecisionLogSession,
    deleteDecisionLogSession,
    testWatchPattern,
    listLearnedAliases
  } from '#lib/api/client.js';
  import { learnedNames } from '#lib/learnedAliases.js';
  import type { DecisionLogSessionDetail } from '#lib/types.js';
  import { showToast } from '#lib/stores/toast.svelte.js';
  import ProviderCallCard from '#lib/components/decision-log/ProviderCallCard.svelte';
  import ReleaseRow from '#lib/components/decision-log/ReleaseRow.svelte';

  let loading = $state(true);
  let error = $state('');
  let learned = $state<string[]>([]);
  let session = $state<DecisionLogSessionDetail | null>(null);
  let showDeleteConfirm = $state(false);
  let deleting = $state(false);

  // Filter state
  let releaseFilter = $state('');
  let decisionFilter = $state<'all' | 'best' | 'candidate' | 'rejected'>('all');

  // Score release state
  let scoreInput = $state('');
  let scoring = $state(false);

  // Compare mode
  let compareMode = $state(false);
  let compareIds = $state(new Set<string>());

  const compareReleases = $derived(() => {
    if (!session || compareIds.size !== 2) return [];
    const ids = [...compareIds];
    return session.releases.filter((r) => ids.includes(r.id));
  });

  function toggleCompare(release: import('#lib/types.js').DecisionLogRelease) {
    if (compareIds.has(release.id)) {
      compareIds.delete(release.id);
    } else {
      if (compareIds.size >= 2) {
        // Replace oldest
        const first = [...compareIds][0];
        compareIds.delete(first);
      }
      compareIds.add(release.id);
    }
    compareIds = new Set(compareIds);
  }

  const sessionId = $derived(page.params.id!);

  const filteredReleases = $derived(() => {
    if (!session) return [];
    let releases = [...session.releases];

    // Sort: best first, then by total_score desc
    releases.sort((a, b) => {
      if (a.is_best && !b.is_best) return -1;
      if (!a.is_best && b.is_best) return 1;
      return (b.total_score ?? 0) - (a.total_score ?? 0);
    });

    // Filter by decision category
    if (decisionFilter === 'best') {
      releases = releases.filter((r) => r.is_best);
    } else if (decisionFilter === 'candidate') {
      releases = releases.filter((r) => r.decision === 'candidate');
    } else if (decisionFilter === 'rejected') {
      releases = releases.filter((r) => r.decision.startsWith('rejected'));
    }

    // Filter by release title text
    if (releaseFilter.trim()) {
      const q = releaseFilter.toLowerCase();
      releases = releases.filter((r) => r.release_title.toLowerCase().includes(q));
    }

    return releases;
  });

  const releaseCounts = $derived(() => {
    if (!session) return { all: 0, best: 0, candidate: 0, rejected: 0 };
    const releases = session.releases;
    return {
      all: releases.length,
      best: releases.filter((r) => r.is_best).length,
      candidate: releases.filter((r) => r.decision === 'candidate').length,
      rejected: releases.filter((r) => r.decision.startsWith('rejected')).length
    };
  });

  function getTriggerLabel(trigger: string): string {
    switch (trigger) {
      case 'backlog_search':
        return 'Backlog Search';
      case 'immediate_search':
        return 'Immediate Search';
      case 'want':
        return 'Want';
      case 'feed_sync':
        return 'Feed Sync';
      case 'debrid_search':
        return 'Debrid Search';
      case 'test':
        return 'Test';
      default:
        return trigger;
    }
  }

  function getTriggerStyle(trigger: string): { bg: string; color: string } {
    switch (trigger) {
      case 'backlog_search':
        return { bg: 'rgba(168, 85, 247, 0.15)', color: '#a855f7' };
      case 'immediate_search':
        return { bg: 'rgba(59, 130, 246, 0.15)', color: '#3b82f6' };
      case 'want':
        return { bg: 'rgba(234, 179, 8, 0.15)', color: '#eab308' };
      case 'feed_sync':
        return { bg: 'rgba(6, 182, 212, 0.15)', color: '#06b6d4' };
      case 'debrid_search':
        return { bg: 'rgba(99, 102, 241, 0.15)', color: '#6366f1' };
      default:
        return { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af' };
    }
  }

  function getOutcomeStyle(outcome: string | null): { bg: string; color: string; label: string } {
    switch (outcome) {
      case 'downloaded':
        return { bg: 'rgba(34, 197, 94, 0.15)', color: '#22c55e', label: 'Downloaded' };
      case 'no_match':
        return { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af', label: 'No Match' };
      case 'error':
        return { bg: 'rgba(239, 68, 68, 0.15)', color: '#ef4444', label: 'Error' };
      case 'held':
        return { bg: 'rgba(168, 85, 247, 0.15)', color: '#a855f7', label: 'Held' };
      case 'callback_sent':
        return { bg: 'rgba(59, 130, 246, 0.15)', color: '#3b82f6', label: 'Notified' };
      case 'skipped':
        return { bg: 'rgba(234, 179, 8, 0.15)', color: '#eab308', label: 'Skipped' };
      default:
        return {
          bg: 'rgba(156, 163, 175, 0.15)',
          color: '#9ca3af',
          label: outcome ?? 'Pending'
        };
    }
  }

  async function loadSession() {
    loading = true;
    error = '';
    try {
      session = await getDecisionLogSession(sessionId);
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load session';
    } finally {
      loading = false;
    }
  }

  async function handleExport() {
    try {
      const data = await exportDecisionLogSession(sessionId);
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `decision-log-${sessionId}.json`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Export failed');
    }
  }

  async function handleDelete() {
    deleting = true;
    try {
      await deleteDecisionLogSession(sessionId);
      showToast('Session deleted');
      // Navigate back to appropriate log page
      const trigger = session?.trigger_type;
      if (trigger === 'feed_sync') {
        goto('/watch/feed-log');
      } else {
        goto('/watch/search-log');
      }
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete session';
    } finally {
      deleting = false;
    }
  }

  async function handleScoreRelease() {
    if (!scoreInput.trim() || !session?.watch_pattern_id) return;
    scoring = true;
    try {
      const result = await testWatchPattern(session.watch_pattern_id, scoreInput.trim());
      scoreInput = '';
      goto(`/watch/decision-log/${result.session_id}`);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Score test failed');
    } finally {
      scoring = false;
    }
  }

  function getBackLink(): string {
    if (session?.trigger_type === 'feed_sync') return '/watch/feed-log';
    if (session?.trigger_type === 'debrid_search') return '/watch/debrid-log?tab=searches';
    return '/watch/search-log';
  }

  function getBackLabel(): string {
    if (session?.trigger_type === 'feed_sync') return 'Feed Log';
    if (session?.trigger_type === 'debrid_search') return 'Debrid Log';
    return 'Search Log';
  }

  onMount(() => {
    loadSession();
    listLearnedAliases()
      .then((aliases) => (learned = learnedNames(aliases)))
      .catch(() => (learned = []));
  });
</script>

{#if loading}
  <div class="loading-state">Loading session...</div>
{:else if error}
  <div class="error-banner">{error}</div>
  <a href="/watch/search-log" class="back-link">Back to Search Log</a>
{:else if session}
  {@const trigger = getTriggerStyle(session.trigger_type)}
  {@const outcome = getOutcomeStyle(session.outcome)}
  {@const meta = session.pattern_metadata as Record<string, string> | null}
  {@const displayName = meta?.title
    ? `${meta.title}${meta.author ? ` — ${meta.author}` : ''}`
    : session.search_queries?.length
      ? session.search_queries[0]
      : (session.pattern_name ?? session.source_name ?? 'Decision Log')}

  <!-- Breadcrumbs -->
  <nav class="breadcrumbs">
    <a href={getBackLink()}>{getBackLabel()}</a>
    <span class="sep">›</span>
    <span class="current">Session Detail</span>
  </nav>

  <!-- Header -->
  <div class="page-header">
    <div class="header-left">
      <div class="header-badges">
        <span class="trigger-badge" style="background: {trigger.bg}; color: {trigger.color}">
          {getTriggerLabel(session.trigger_type)}
        </span>
        <span class="outcome-badge" style="background: {outcome.bg}; color: {outcome.color}">
          {outcome.label}
        </span>
      </div>
      <h1>{displayName}</h1>
      {#if meta?.title && session.pattern_name}
        <div class="pattern-terms">{session.pattern_name}</div>
      {/if}
      <div class="header-meta">
        <span>{session.releases_evaluated} releases evaluated</span>
        {#if session.duration_ms !== null}
          <span>{session.duration_ms}ms</span>
        {/if}
        <span>{new Date(session.created_at).toLocaleString()}</span>
      </div>
    </div>
    <div class="header-actions">
      <button class="btn compact alt" type="button" onclick={handleExport}>Export JSON</button>

      <button class="btn compact danger" type="button" onclick={() => (showDeleteConfirm = true)}
        >Delete</button
      >
    </div>
  </div>

  <!-- Test banner -->
  {#if session.is_test}
    <div class="test-banner">
      Dry run — no download was created
      {#if session.watch_pattern_id}
        <a class="btn compact" href="/watch/search-log?watch_pattern_id={session.watch_pattern_id}">
          Watch next search
        </a>
      {/if}
    </div>
  {/if}

  <!-- Error message -->
  {#if session.error_message}
    <div class="error-banner">{session.error_message}</div>
  {/if}

  <!-- Score Release -->
  {#if session.watch_pattern_id}
    <div class="score-section">
      <span class="score-label">Score a release</span>
      <form
        class="score-form"
        onsubmit={(e) => {
          e.preventDefault();
          handleScoreRelease();
        }}
      >
        <input
          type="text"
          class="score-input"
          placeholder="Paste a release string to test scoring..."
          bind:value={scoreInput}
          disabled={scoring}
        />
        <button class="btn compact" type="submit" disabled={scoring || !scoreInput.trim()}>
          {scoring ? 'Testing...' : 'Score'}
        </button>
      </form>
    </div>
  {/if}

  <!-- Provider Calls -->
  {#if session.provider_calls.length > 0}
    <section class="section">
      <h2 class="section-heading">
        Provider Calls
        <span class="count-badge">{session.provider_calls.length}</span>
      </h2>
      <div class="provider-list">
        {#each session.provider_calls as call (call.id)}
          <ProviderCallCard {call} />
        {/each}
      </div>
    </section>
  {/if}

  <!-- Releases -->
  <section class="section">
    <h2 class="section-heading">
      Releases
      <span class="count-badge">{session.releases.length}</span>
    </h2>

    <div class="release-controls">
      <div class="filter-tabs">
        <button
          class="tab"
          class:active={decisionFilter === 'all'}
          type="button"
          onclick={() => (decisionFilter = 'all')}>All ({releaseCounts().all})</button
        >

        <button
          class="tab"
          class:active={decisionFilter === 'best'}
          type="button"
          onclick={() => (decisionFilter = 'best')}>Best ({releaseCounts().best})</button
        >

        <button
          class="tab"
          class:active={decisionFilter === 'candidate'}
          type="button"
          onclick={() => (decisionFilter = 'candidate')}
          >Candidates ({releaseCounts().candidate})</button
        >

        <button
          class="tab"
          class:active={decisionFilter === 'rejected'}
          type="button"
          onclick={() => (decisionFilter = 'rejected')}
          >Rejected ({releaseCounts().rejected})</button
        >
      </div>
      <input
        type="text"
        class="release-search"
        placeholder="Filter by title..."
        bind:value={releaseFilter}
      />
      <button
        class="btn compact"
        class:active={compareMode}
        type="button"
        onclick={() => {
          compareMode = !compareMode;
          if (!compareMode) compareIds = new Set();
        }}
      >
        {compareMode ? 'Exit Compare' : 'Compare'}
      </button>
    </div>

    {#if compareMode && compareReleases().length === 2}
      {@const [a, b] = compareReleases()}
      <div class="compare-panel">
        <h3 class="compare-title">Comparison</h3>
        <div class="compare-grid">
          <div class="compare-header"></div>
          <div class="compare-header compare-name" title={a.release_title}>{a.release_title}</div>
          <div class="compare-header compare-name" title={b.release_title}>{b.release_title}</div>

          <div class="compare-label">Decision</div>
          <div class="compare-val">{a.decision}</div>
          <div class="compare-val">{b.decision}</div>

          <div class="compare-label">Match Score</div>
          <div class="compare-val">
            {a.match_score !== null ? (a.match_score * 100).toFixed(0) + '%' : '-'}
          </div>
          <div class="compare-val">
            {b.match_score !== null ? (b.match_score * 100).toFixed(0) + '%' : '-'}
          </div>

          <div class="compare-label">Quality Score</div>
          <div class="compare-val">{a.quality_score?.toFixed(2) ?? '-'}</div>
          <div class="compare-val">{b.quality_score?.toFixed(2) ?? '-'}</div>

          <div class="compare-label">Total Score</div>
          <div class="compare-val">{a.total_score?.toFixed(2) ?? '-'}</div>
          <div class="compare-val">{b.total_score?.toFixed(2) ?? '-'}</div>

          {#if a.quality_score_breakdown && b.quality_score_breakdown}
            {@const aB = a.quality_score_breakdown}
            {@const bB = b.quality_score_breakdown}
            {@const allKeys = [...new Set([...Object.keys(aB), ...Object.keys(bB)])].filter(
              (k) =>
                k !== 'total' &&
                k !== 'rejected' &&
                k !== 'rejected_reason' &&
                ((aB[k as keyof typeof aB] ?? 0) !== 0 || (bB[k as keyof typeof bB] ?? 0) !== 0)
            )}
            {#each allKeys as key (key)}
              {@const aVal = (aB[key as keyof typeof aB] ?? 0) as number}
              {@const bVal = (bB[key as keyof typeof bB] ?? 0) as number}
              {@const diff = aVal !== bVal}
              <div class="compare-label sub">{key.replace(/_/g, ' ')}</div>
              <div class="compare-val" class:diff-highlight={diff}>
                {aVal !== 0 ? (aVal > 0 ? '+' : '') + aVal.toFixed(2) : '-'}
              </div>
              <div class="compare-val" class:diff-highlight={diff}>
                {bVal !== 0 ? (bVal > 0 ? '+' : '') + bVal.toFixed(2) : '-'}
              </div>
            {/each}
          {/if}

          {#if a.parsed_quality && b.parsed_quality}
            {@const aPQ = a.parsed_quality as Record<string, unknown>}
            {@const bPQ = b.parsed_quality as Record<string, unknown>}
            {#each ['resolution', 'source', 'codec', 'audio', 'release_group'] as field (field)}
              {@const aV = aPQ[field] ?? '-'}
              {@const bV = bPQ[field] ?? '-'}
              <div class="compare-label">{field.replace(/_/g, ' ')}</div>
              <div class="compare-val" class:diff-highlight={String(aV) !== String(bV)}>{aV}</div>
              <div class="compare-val" class:diff-highlight={String(aV) !== String(bV)}>{bV}</div>
            {/each}
          {/if}
        </div>
      </div>
    {:else if compareMode && compareReleases().length < 2}
      <div class="compare-hint">Select 2 releases to compare</div>
    {/if}

    <div class="release-list">
      {#if filteredReleases().length === 0}
        <div class="empty-state">No releases match the current filter.</div>
      {:else}
        {#each filteredReleases() as release (release.id)}
          <ReleaseRow
            {release}
            {compareMode}
            compareSelected={compareIds.has(release.id)}
            onToggleCompare={toggleCompare}
            {learned}
          />
        {/each}
      {/if}
    </div>
  </section>

  <!-- Search queries -->
  {#if session.search_queries && session.search_queries.length > 0}
    <section class="section">
      <h2 class="section-heading">Search Queries</h2>
      <div class="queries-list">
        {#each session.search_queries as query (query)}
          <code class="query-item">{query}</code>
        {/each}
      </div>
    </section>
  {/if}

  {#if showDeleteConfirm}
    <DeleteConfirmModal
      title="Delete Session?"
      message="This will permanently delete this decision log session."
      loading={deleting}
      onConfirm={handleDelete}
      onCancel={() => {
        showDeleteConfirm = false;
      }}
    />
  {/if}
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

  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 20px;
  }

  .header-left {
    min-width: 0;
  }

  .header-badges {
    display: flex;
    gap: 6px;
    margin-bottom: 6px;
  }

  .page-header h1 {
    margin: 0 0 4px;
    font-size: 22px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
    word-break: break-word;
  }

  .pattern-terms {
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 2px;
  }

  .header-meta {
    display: flex;
    gap: 12px;
    font-size: 12px;
    color: var(--muted);
  }

  .header-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
  }

  .trigger-badge,
  .outcome-badge {
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 600;
    text-transform: capitalize;
    white-space: nowrap;
  }

  .test-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 14px;
    border-radius: 10px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(234, 179, 8, 0.12);
    border: 1px solid rgba(234, 179, 8, 0.25);
    color: #eab308;
    font-weight: 500;
  }

  .error-banner {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .back-link {
    color: var(--accent);
    text-decoration: none;
    font-size: 13px;
  }

  .back-link:hover {
    text-decoration: underline;
  }

  .score-section {
    margin-bottom: 20px;
    padding: 12px 16px;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
  }

  .score-label {
    display: block;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin-bottom: 6px;
    font-weight: 600;
  }

  .score-form {
    display: flex;
    gap: 8px;
  }

  .score-input {
    flex: 1;
    padding: 7px 10px;
    border-radius: 8px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
  }

  .score-input::placeholder {
    color: var(--muted);
  }

  .section {
    margin-bottom: 24px;
  }

  .section-heading {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 0 12px;
    font-size: 16px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .count-badge {
    font-size: 12px;
    padding: 1px 8px;
    border-radius: 8px;
    background: var(--bg);
    border: 1px solid var(--ring);
    color: var(--muted);
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    font-weight: 400;
  }

  .provider-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .release-controls {
    display: flex;
    gap: 12px;
    margin-bottom: 12px;
    align-items: center;
    flex-wrap: wrap;
  }

  .filter-tabs {
    display: flex;
    gap: 2px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 8px;
    padding: 2px;
  }

  .tab {
    padding: 4px 10px;
    border-radius: 6px;
    border: none;
    background: none;
    color: var(--muted);
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
    font: inherit;
    font-size: 12px;
  }

  .tab:hover {
    color: var(--ink);
  }

  .tab.active {
    background: var(--card);
    color: var(--ink);
    font-weight: 500;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.1);
  }

  .release-search {
    flex: 1;
    min-width: 180px;
    padding: 6px 10px;
    border-radius: 8px;
    border: 1px solid var(--ring);
    background: var(--card);
    color: var(--ink);
    font-size: 12px;
  }

  .release-search::placeholder {
    color: var(--muted);
  }

  .release-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .compare-panel {
    margin-bottom: 12px;
    padding: 14px;
    background: var(--card);
    border: 1px solid var(--accent);
    border-radius: 12px;
  }

  .compare-title {
    margin: 0 0 10px;
    font-size: 14px;
    font-weight: 600;
  }

  .compare-grid {
    display: grid;
    grid-template-columns: auto 1fr 1fr;
    gap: 4px 12px;
    font-size: 12px;
  }

  .compare-header {
    font-weight: 600;
    font-size: 11px;
    color: var(--muted);
    padding-bottom: 4px;
    border-bottom: 1px solid var(--ring);
  }

  .compare-name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 300px;
  }

  .compare-label {
    color: var(--muted);
    text-transform: capitalize;
    white-space: nowrap;
  }

  .compare-label.sub {
    padding-left: 12px;
    font-size: 11px;
  }

  .compare-val {
    font-variant-numeric: tabular-nums;
  }

  .diff-highlight {
    font-weight: 600;
    color: #eab308;
  }

  .compare-hint {
    padding: 10px 14px;
    border-radius: 8px;
    background: rgba(59, 130, 246, 0.08);
    color: #3b82f6;
    font-size: 13px;
    margin-bottom: 12px;
  }

  .loading-state,
  .empty-state {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
  }

  .queries-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .query-item {
    font-size: 12px;
    padding: 4px 10px;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 6px;
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      gap: 10px;
    }

    .header-meta {
      flex-wrap: wrap;
      gap: 8px;
    }

    .release-controls {
      flex-direction: column;
      align-items: stretch;
    }

    .filter-tabs {
      overflow-x: auto;
      -webkit-overflow-scrolling: touch;
    }

    .release-search {
      min-width: 0;
    }

    .score-form {
      flex-direction: column;
    }
  }
</style>
