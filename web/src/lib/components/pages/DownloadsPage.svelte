<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    listDownloads,
    getDownloadEvents,
    deleteDownload,
    clearDownloads,
    reingestDownload,
    type DownloadItem,
    type DownloadEvent
  } from '#lib/api/client.js';
  import { InfiniteScroll, ScrollToTop, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import { createSseConnection, type SseConnection } from '#lib/sse.js';
  import { showToast } from '#lib/stores/toast.svelte.js';
  import DownloadCard from '../downloads/DownloadCard.svelte';
  import ReingestDialog from '../downloads/ReingestDialog.svelte';
  import DeleteDialog from '../downloads/DeleteDialog.svelte';
  import LoadingState from '../LoadingState.svelte';
  import EmptyState from '../EmptyState.svelte';

  let downloads = $state.raw<DownloadItem[]>([]);
  let loading = $state(false);
  let error = $state('');
  let sseConnection: SseConnection | null = null;
  let nextCursor = $state<string | null>(null);
  let hasMore = $state(true);

  // Filters
  let searchQuery = $state('');
  let statusFilter = $state('');
  let searchTimeout: ReturnType<typeof setTimeout> | null = null;

  // Track expanded cards and their events
  let expandedIds = $state<Set<string>>(new Set());
  let eventCache = $state<Record<string, DownloadEvent[]>>({});
  let loadingEvents = $state<Set<string>>(new Set());

  const statusFilterItems: SearchSelectItem[] = [
    { value: '', label: 'All statuses' },
    { value: 'pending', label: 'Pending' },
    { value: 'queued', label: 'Queued' },
    { value: 'downloading', label: 'Downloading' },
    { value: 'seeding', label: 'Seeding' },
    { value: 'processing', label: 'Processing' },
    { value: 'completed', label: 'Completed' },
    { value: 'failed', label: 'Failed' },
    { value: 'cancelled', label: 'Cancelled' },
    { value: 'imported', label: 'Imported' }
  ];

  const ACTIVE_STATUSES = ['queued', 'downloading'];
  const inactiveCount = $derived(
    downloads.filter((d) => !ACTIVE_STATUSES.includes(d.status)).length
  );

  // Delete dialog state
  type DeleteMode =
    { kind: 'single'; item: DownloadItem } | { kind: 'clear'; downloads: DownloadItem[] };
  let deleteMode = $state<DeleteMode | null>(null);
  let deleteLoading = $state(false);

  const openDeleteSingle = (item: DownloadItem) => {
    deleteMode = { kind: 'single', item };
  };

  const openDeleteClear = () => {
    deleteMode = { kind: 'clear', downloads };
  };

  const closeDeleteDialog = () => {
    if (!deleteLoading) deleteMode = null;
  };

  const handleDeleteConfirm = async (cancelProtocols: string[]) => {
    deleteLoading = true;
    try {
      if (deleteMode?.kind === 'single') {
        const item = deleteMode.item;
        const shouldCancel = cancelProtocols.includes(item.protocol);
        await deleteDownload(item.id, shouldCancel);
        downloads = downloads.filter((d) => d.id !== item.id);
        showToast('Download removed');
      } else if (deleteMode?.kind === 'clear') {
        await clearDownloads(cancelProtocols.length > 0 ? cancelProtocols : undefined);
        downloads = downloads.filter((d) => ACTIVE_STATUSES.includes(d.status));
        expandedIds = new Set();
        eventCache = {};
        showToast('Downloads cleared');
      }
      deleteMode = null;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Operation failed';
    } finally {
      deleteLoading = false;
    }
  };

  // Reingest state
  let reingestId = $state<string | null>(null);
  let reingestUrl = $state('');
  let reingestLoading = $state(false);
  let reingestResult = $state<{ success: boolean; message: string } | null>(null);

  const openReingestDialog = (item: DownloadItem) => {
    reingestId = item.id;
    reingestUrl = '';
    reingestResult = null;
  };

  const closeReingestDialog = () => {
    reingestId = null;
    reingestUrl = '';
    reingestResult = null;
  };

  const handleReingest = async () => {
    if (!reingestId) return;
    reingestLoading = true;
    reingestResult = null;
    try {
      const options = reingestUrl.trim() ? { callback_url: reingestUrl.trim() } : undefined;
      const result = await reingestDownload(reingestId, options);
      reingestResult = result;
      if (result.success) {
        showToast('Reingest triggered');
        // Refresh events for this download
        delete eventCache[reingestId];
        eventCache = { ...eventCache };
        if (expandedIds.has(reingestId)) {
          loadingEvents.add(reingestId);
          loadingEvents = new Set(loadingEvents);
          try {
            eventCache[reingestId] = await getDownloadEvents(reingestId);
            eventCache = { ...eventCache };
          } catch {
            // Ignore
          } finally {
            loadingEvents.delete(reingestId);
            loadingEvents = new Set(loadingEvents);
          }
        }
        await loadDownloads(true);
      }
    } catch (err) {
      reingestResult = {
        success: false,
        message: err instanceof Error ? err.message : 'Reingest failed'
      };
    } finally {
      reingestLoading = false;
    }
  };

  const loadDownloads = async (reset = false) => {
    if (loading) return;
    loading = true;
    error = '';
    try {
      const response = await listDownloads({
        status: statusFilter || undefined,
        search: searchQuery || undefined,
        cursor: reset ? undefined : nextCursor || undefined,
        limit: 50
      });
      if (reset) {
        downloads = response.items;
      } else {
        downloads = [...downloads, ...response.items];
      }
      nextCursor = response.next_cursor;
      hasMore = response.next_cursor !== null;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load downloads';
    } finally {
      loading = false;
    }
  };

  const handleSearchInput = (e: Event) => {
    const value = (e.target as HTMLInputElement).value;
    searchQuery = value;
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {
      nextCursor = null;
      loadDownloads(true);
    }, 300);
  };

  const handleStatusChange = () => {
    nextCursor = null;
    loadDownloads(true);
  };

  const toggleExpand = async (id: string) => {
    if (expandedIds.has(id)) {
      expandedIds.delete(id);
      expandedIds = new Set(expandedIds);
    } else {
      expandedIds.add(id);
      expandedIds = new Set(expandedIds);
      // Load events if not cached
      if (!eventCache[id] && !loadingEvents.has(id)) {
        loadingEvents.add(id);
        loadingEvents = new Set(loadingEvents);
        try {
          eventCache[id] = await getDownloadEvents(id);
          eventCache = { ...eventCache };
        } catch {
          // Ignore errors loading events
        } finally {
          loadingEvents.delete(id);
          loadingEvents = new Set(loadingEvents);
        }
      }
    }
  };

  const handleStreamEvent = (payload: Record<string, unknown>) => {
    const type = payload.type as string;
    if (type === 'snapshot') {
      // On SSE snapshot, only use it if we have no data yet (initial load)
      if (downloads.length === 0 && Array.isArray(payload.downloads)) {
        downloads = payload.downloads as DownloadItem[];
        hasMore = downloads.length >= 50;
      }
    } else if (type === 'created' && payload.download) {
      const dl = payload.download as DownloadItem;
      // Prepend new downloads (they match any filter since they're new)
      downloads = [dl, ...downloads.filter((d) => d.id !== dl.id)];
    } else if (type === 'updated' && payload.download) {
      const dl = payload.download as DownloadItem;
      downloads = downloads.map((d) => (d.id === dl.id ? dl : d));
    }
  };

  onMount(() => {
    loadDownloads(true);
    sseConnection = createSseConnection('/api/v1/downloads/stream', handleStreamEvent, (err) => {
      error = err ?? '';
    });
  });

  onDestroy(() => {
    sseConnection?.stop();
    if (searchTimeout) clearTimeout(searchTimeout);
  });
</script>

<div class="page-header">
  <div>
    <h1>Downloads</h1>
    <p class="muted">Track active and completed requests handled by Kobload.</p>
  </div>
  {#if inactiveCount > 0}
    <div class="header-actions">
      <button class="btn danger-outline small" type="button" onclick={openDeleteClear}>
        Clear
      </button>
    </div>
  {/if}
</div>

<div class="filters">
  <input
    type="text"
    class="search-input"
    placeholder="Search downloads..."
    value={searchQuery}
    oninput={handleSearchInput}
  />
  <div class="status-filter">
    <SearchSelect
      id="status-filter"
      items={statusFilterItems}
      value={statusFilter}
      onchange={(v) => {
        statusFilter = v;
        handleStatusChange();
      }}
      placeholder="All statuses"
    />
  </div>
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

{#if loading && downloads.length === 0}
  <LoadingState message="Loading downloads..." />
{:else if downloads.length === 0}
  <EmptyState
    icon="download"
    title="No downloads found"
    message={searchQuery || statusFilter
      ? 'Try adjusting your filters.'
      : 'Kick off a search in Kobbler to see requests here.'}
  />
{:else}
  <div class="downloads-list">
    {#each downloads as item (item.id)}
      <DownloadCard
        {item}
        isExpanded={expandedIds.has(item.id)}
        events={eventCache[item.id]}
        loadingEvents={loadingEvents.has(item.id)}
        onToggleExpand={toggleExpand}
        onOpenReingest={openReingestDialog}
        onDelete={openDeleteSingle}
      />
    {/each}
  </div>
{/if}

<InfiniteScroll {hasMore} {loading} onLoadMore={() => loadDownloads()} />
<ScrollToTop />

<ReingestDialog
  {reingestId}
  bind:reingestUrl
  {reingestLoading}
  {reingestResult}
  onClose={closeReingestDialog}
  onSubmit={handleReingest}
/>

<DeleteDialog
  mode={deleteMode}
  loading={deleteLoading}
  onConfirm={handleDeleteConfirm}
  onClose={closeDeleteDialog}
/>

<style>
  .status-filter {
    min-width: 160px;
  }

  .downloads-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    background: var(--ring);
    border: 1px solid var(--ring);
    border-radius: 12px;
    overflow: hidden;
  }
</style>
