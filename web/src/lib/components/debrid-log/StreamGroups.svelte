<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { DeleteConfirmModal, InfiniteScroll } from '@koblibs/ui';
  import {
    clearStreamAttempts,
    getStreamAttemptEvents,
    listStreamGroups,
    type StreamAttempt,
    type StreamAttemptEvent,
    type StreamGroup,
    type StreamSearch
  } from '#lib/api/client.js';
  import { createSseConnection, type SseConnection } from '#lib/sse.js';
  import { showToast } from '#lib/stores/toast.svelte.js';
  import StreamGroupCard from './StreamGroupCard.svelte';
  import StreamAttemptCard from './StreamAttemptCard.svelte';
  import LoadingState from '../LoadingState.svelte';
  import EmptyState from '../EmptyState.svelte';

  const PAGE_SIZE = 30;

  let groups = $state.raw<StreamGroup[]>([]);
  let loading = $state(false);
  let error = $state('');
  let hasMore = $state(true);
  let searchQuery = $state('');
  let searchTimeout: ReturnType<typeof setTimeout> | null = null;
  let reloadTimeout: ReturnType<typeof setTimeout> | null = null;
  let sse: SseConnection | null = null;

  let expandedIds = $state<Set<string>>(new Set());
  let eventCache = $state<Record<string, StreamAttemptEvent[]>>({});
  let loadingEvents = $state<Set<string>>(new Set());

  let showClearConfirm = $state(false);
  let clearing = $state(false);

  const groupTime = (g: StreamGroup) => g.search?.created_at ?? g.attempts[0]?.created_at ?? '';

  const load = async (reset = false) => {
    if (loading) return;
    loading = true;
    error = '';
    try {
      const page = await listStreamGroups({
        search: searchQuery.trim() || undefined,
        before: reset || groups.length === 0 ? undefined : groupTime(groups[groups.length - 1]),
        limit: PAGE_SIZE
      });
      groups = reset ? page : [...groups, ...page];
      hasMore = page.length >= PAGE_SIZE;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load streams';
    } finally {
      loading = false;
    }
  };

  // A play can join a search older than the page holds; reloading places it.
  const reloadSoon = () => {
    if (reloadTimeout) clearTimeout(reloadTimeout);
    reloadTimeout = setTimeout(() => load(true), 500);
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

  const upsertAttempt = (attempt: StreamAttempt) => {
    const existing = groups.findIndex((g) => g.attempts.some((a) => a.id === attempt.id));
    if (existing >= 0) {
      groups = groups.map((g, i) =>
        i === existing
          ? { ...g, attempts: g.attempts.map((a) => (a.id === attempt.id ? attempt : a)) }
          : g
      );
      return;
    }
    if (searchQuery.trim()) return;
    if (attempt.search_id) {
      const home = groups.findIndex((g) => g.id === attempt.search_id);
      if (home < 0) return reloadSoon();
      groups = groups.map((g, i) =>
        i === home ? { ...g, attempts: [attempt, ...g.attempts] } : g
      );
      return;
    }
    groups = [{ id: attempt.id, search: null, attempts: [attempt] }, ...groups];
  };

  const addSearch = (search: StreamSearch) => {
    if (searchQuery.trim() || groups.some((g) => g.id === search.id)) return;
    groups = [{ id: search.id, search, attempts: [] }, ...groups];
  };

  const handleStreamEvent = (payload: Record<string, unknown>) => {
    switch (payload.type) {
      case 'search':
        addSearch(payload.search as StreamSearch);
        return;
      case 'created':
      case 'updated': {
        const attempt = payload.attempt as StreamAttempt;
        upsertAttempt(attempt);
        if (expandedIds.has(attempt.id)) loadEvents(attempt.id);
        return;
      }
    }
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
      groups = [];
      hasMore = false;
      expandedIds = new Set();
      eventCache = {};
      showClearConfirm = false;
      showToast('Stream log cleared');
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to clear the stream log';
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
    if (reloadTimeout) clearTimeout(reloadTimeout);
  });
</script>

<div class="filters">
  <input
    type="text"
    class="search-input"
    placeholder="Search titles, releases or hashes..."
    value={searchQuery}
    oninput={handleSearchInput}
  />
  <button
    class="btn compact danger"
    type="button"
    onclick={() => (showClearConfirm = true)}
    disabled={groups.length === 0}
  >
    Clear All
  </button>
</div>

{#if error}
  <div class="error-banner">{error}</div>
{/if}

{#if loading && groups.length === 0}
  <LoadingState message="Loading streams..." />
{:else if groups.length === 0}
  <EmptyState
    icon="download"
    title="No streams"
    message={searchQuery
      ? 'Try adjusting your search.'
      : 'Stream searches and what was tried for them appear here while Debrid Logging is on. They are kept for a day.'}
  />
{:else}
  <div class="group-list">
    {#each groups as group (group.id)}
      {#if group.search}
        <StreamGroupCard
          search={group.search}
          attempts={group.attempts}
          {expandedIds}
          {eventCache}
          {loadingEvents}
          onToggleExpand={toggleExpand}
        />
      {:else}
        <div class="lone">
          {#each group.attempts as attempt (attempt.id)}
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
    {/each}
  </div>
{/if}

<InfiniteScroll {hasMore} {loading} onLoadMore={() => load()} />

{#if showClearConfirm}
  <DeleteConfirmModal
    title="Clear the stream log?"
    message="This permanently deletes every recorded stream search and attempt."
    loading={clearing}
    onConfirm={handleClear}
    onCancel={() => (showClearConfirm = false)}
  />
{/if}

<style>
  .group-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .lone {
    border: 1px solid var(--ring);
    border-radius: 12px;
    overflow: hidden;
  }
</style>
