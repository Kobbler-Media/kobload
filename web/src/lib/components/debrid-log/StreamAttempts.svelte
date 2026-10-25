<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    DeleteConfirmModal,
    InfiniteScroll,
    SearchSelect,
    type SearchSelectItem
  } from '@koblibs/ui';
  import {
    clearStreamAttempts,
    getStreamAttemptEvents,
    listStreamAttempts,
    type StreamAttempt,
    type StreamAttemptEvent
  } from '$lib/api/client';
  import { createSseConnection, type SseConnection } from '$lib/sse';
  import { showToast } from '$lib/stores/toast.svelte';
  import { getStreamAttemptStatusStyle, streamAttemptStatuses } from '$lib/styles/statusColors';
  import StreamAttemptCard from './StreamAttemptCard.svelte';
  import LoadingState from '../LoadingState.svelte';
  import EmptyState from '../EmptyState.svelte';

  const PAGE_SIZE = 50;

  let attempts = $state.raw<StreamAttempt[]>([]);
  let loading = $state(false);
  let error = $state('');
  let hasMore = $state(true);
  let searchQuery = $state('');
  let statusFilter = $state('');
  let searchTimeout: ReturnType<typeof setTimeout> | null = null;
  let sse: SseConnection | null = null;

  let expandedIds = $state<Set<string>>(new Set());
  let eventCache = $state<Record<string, StreamAttemptEvent[]>>({});
  let loadingEvents = $state<Set<string>>(new Set());

  let showClearConfirm = $state(false);
  let clearing = $state(false);

  const statusItems: SearchSelectItem[] = [
    { value: '', label: 'All statuses' },
    ...streamAttemptStatuses.map((s) => ({ value: s, label: getStreamAttemptStatusStyle(s).label }))
  ];

  const matchesFilters = (a: StreamAttempt) => {
    if (statusFilter && a.status !== statusFilter) return false;
    const q = searchQuery.trim().toLowerCase();
    if (!q) return true;
    return [a.release_title, a.file_name, a.info_hash].some((v) => v?.toLowerCase().includes(q));
  };

  const load = async (reset = false) => {
    if (loading) return;
    loading = true;
    error = '';
    try {
      const page = await listStreamAttempts({
        status: statusFilter || undefined,
        search: searchQuery.trim() || undefined,
        before: reset ? undefined : attempts.at(-1)?.created_at,
        limit: PAGE_SIZE
      });
      attempts = reset ? page : [...attempts, ...page];
      hasMore = page.length >= PAGE_SIZE;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load stream attempts';
    } finally {
      loading = false;
    }
  };

  const loadEvents = async (id: string) => {
    loadingEvents = new Set(loadingEvents).add(id);
    try {
      eventCache = { ...eventCache, [id]: await getStreamAttemptEvents(id) };
    } catch {
      // The row still shows its status; the timeline just stays empty.
    } finally {
      const next = new Set(loadingEvents);
      next.delete(id);
      loadingEvents = next;
    }
  };

  const toggleExpand = (id: string) => {
    const next = new Set(expandedIds);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
      if (!eventCache[id]) loadEvents(id);
    }
    expandedIds = next;
  };

  const handleStreamEvent = (payload: Record<string, unknown>) => {
    const type = payload.type as string;
    if (type === 'snapshot') {
      if (attempts.length === 0 && Array.isArray(payload.attempts)) {
        attempts = (payload.attempts as StreamAttempt[]).filter(matchesFilters);
        hasMore = attempts.length >= PAGE_SIZE;
      }
      return;
    }
    const attempt = payload.attempt as StreamAttempt | undefined;
    if (!attempt) return;
    if (type === 'created' && matchesFilters(attempt)) {
      attempts = [attempt, ...attempts.filter((a) => a.id !== attempt.id)];
    } else if (type === 'updated') {
      attempts = attempts.map((a) => (a.id === attempt.id ? attempt : a));
    }
    if (expandedIds.has(attempt.id)) loadEvents(attempt.id);
  };

  const handleSearchInput = (e: Event) => {
    searchQuery = (e.target as HTMLInputElement).value;
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => load(true), 300);
  };

  const handleClear = async () => {
    clearing = true;
    try {
      await clearStreamAttempts();
      attempts = [];
      hasMore = false;
      expandedIds = new Set();
      eventCache = {};
      showClearConfirm = false;
      showToast('Stream attempts cleared');
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to clear stream attempts';
    } finally {
      clearing = false;
    }
  };

  onMount(() => {
    load(true);
    sse = createSseConnection('/api/v1/stream-attempts/stream', handleStreamEvent, (err) => {
      error = err ?? '';
    });
  });

  onDestroy(() => {
    sse?.stop();
    if (searchTimeout) clearTimeout(searchTimeout);
  });
</script>

<div class="filters">
  <input
    type="text"
    class="search-input"
    placeholder="Search releases or hashes..."
    value={searchQuery}
    oninput={handleSearchInput}
  />
  <div class="status-filter">
    <SearchSelect
      id="attempt-status-filter"
      items={statusItems}
      value={statusFilter}
      onchange={(v) => {
        statusFilter = v;
        load(true);
      }}
      placeholder="All statuses"
    />
  </div>
  <button
    class="btn compact danger"
    type="button"
    onclick={() => (showClearConfirm = true)}
    disabled={attempts.length === 0}
  >
    Clear All
  </button>
</div>

{#if error}
  <div class="error-banner">{error}</div>
{/if}

{#if loading && attempts.length === 0}
  <LoadingState message="Loading stream attempts..." />
{:else if attempts.length === 0}
  <EmptyState
    icon="download"
    title="No stream attempts"
    message={searchQuery || statusFilter
      ? 'Try adjusting your filters.'
      : 'Streams appear here while Debrid Logging is on. They are kept for a day.'}
  />
{:else}
  <div class="attempts-list">
    {#each attempts as attempt (attempt.id)}
      <StreamAttemptCard
        {attempt}
        isExpanded={expandedIds.has(attempt.id)}
        events={eventCache[attempt.id]}
        loadingEvents={loadingEvents.has(attempt.id)}
        onToggleExpand={toggleExpand}
      />
    {/each}
  </div>
{/if}

<InfiniteScroll {hasMore} {loading} onLoadMore={() => load()} />

{#if showClearConfirm}
  <DeleteConfirmModal
    title="Clear Stream Attempts?"
    message="This permanently deletes every recorded stream attempt."
    loading={clearing}
    onConfirm={handleClear}
    onCancel={() => (showClearConfirm = false)}
  />
{/if}

<style>
  .status-filter {
    min-width: 160px;
  }

  .attempts-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    background: var(--ring);
    border: 1px solid var(--ring);
    border-radius: 12px;
    overflow: hidden;
  }
</style>
