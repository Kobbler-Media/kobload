<script lang="ts">
  import { onMount } from 'svelte';
  import {
    listMediaTypes,
    updateMediaType,
    getProtocolDefaults,
    type MediaType,
    type ProtocolDefaults
  } from '$lib/api/client';
  import HandlerDefaults from '$lib/components/HandlerDefaults.svelte';
  import { MultiSearchSelect, type MultiSearchSelectItem } from '@koblibs/ui';

  let loading = $state(false);
  let error = $state('');
  let mediaTypes = $state<MediaType[]>([]);
  let editingId = $state<string | null>(null);
  let saving = $state(false);

  // Edit form state
  let editName = $state('');
  let editProtocolPriority = $state<string[]>([]);
  let editProtocolDelay = $state<number | null>(null);

  // Global defaults (fetched for display in media type cards)
  let defaults = $state<ProtocolDefaults>({ protocol_priority: [], protocol_delay_minutes: null });

  const PROTOCOL_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'usenet', label: 'Usenet', description: 'NZB downloads via SABnzbd' },
    { value: 'torrent', label: 'Torrent', description: 'Torrent downloads via Transmission' }
  ];

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      [mediaTypes, defaults] = await Promise.all([listMediaTypes(), getProtocolDefaults()]);
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load';
    } finally {
      loading = false;
    }
  };

  const startEdit = (mt: MediaType) => {
    editingId = mt.id;
    editName = mt.name;
    editProtocolPriority = [...mt.protocol_priority];
    editProtocolDelay = mt.protocol_delay_minutes;
  };

  const cancelEdit = () => {
    editingId = null;
  };

  const saveEdit = async (mt: MediaType) => {
    saving = true;
    error = '';
    try {
      await updateMediaType(mt.id, {
        name: editName,
        protocol_priority: editProtocolPriority,
        protocol_delay_minutes: editProtocolDelay
      });
      editingId = null;
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to save';
    } finally {
      saving = false;
    }
  };

  const formatDate = (date: string): string => {
    return new Date(date).toLocaleString();
  };

  onMount(refresh);
</script>

<div class="page-header">
  <div>
    <h1>Media Types</h1>
    <p class="muted">Configure protocol priority and delay per media type.</p>
  </div>
  <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<div class="card-grid">
  {#if loading && mediaTypes.length === 0}
    <div class="loading">Loading...</div>
  {:else if mediaTypes.length === 0}
    <div class="empty-state">No media types configured.</div>
  {:else}
    {#each mediaTypes as mt (mt.id)}
      <article class="card">
        {#if editingId === mt.id}
          <form
            onsubmit={(e) => {
              e.preventDefault();
              saveEdit(mt);
            }}
          >
            <div class="form-group">
              <label for="edit-name">Name</label>
              <input id="edit-name" type="text" bind:value={editName} required />
            </div>

            <div class="form-group">
              <label for="edit-priority">Protocol Priority</label>
              <MultiSearchSelect
                id="edit-priority"
                items={PROTOCOL_OPTIONS}
                bind:values={editProtocolPriority}
                placeholder="Select protocols in order of preference..."
              />
              <span class="hint"
                >Selection order determines preference. First selected is tried first.</span
              >
            </div>

            <div class="form-group">
              <label for="edit-delay">Fallback Delay (minutes)</label>
              <input
                id="edit-delay"
                type="number"
                min="0"
                bind:value={editProtocolDelay}
                placeholder="e.g. 60"
              />
              <span class="hint"
                >How long to wait for preferred protocol before accepting alternatives.</span
              >
            </div>

            <div class="form-actions">
              <button class="btn" type="submit" disabled={saving}>
                {saving ? 'Saving...' : 'Save'}
              </button>
              <button class="btn alt" type="button" onclick={cancelEdit} disabled={saving}>
                Cancel
              </button>
            </div>
          </form>
        {:else}
          <div class="card-header">
            <div>
              <h3>{mt.name}</h3>
              <p class="muted">Key: <code>{mt.key}</code></p>
            </div>
            <button class="btn alt small" type="button" onclick={() => startEdit(mt)}>Edit</button>
          </div>

          <div class="meta-grid">
            <div>
              <span class="label">Protocol Priority</span>
              <span>
                {#if mt.protocol_priority.length > 0}
                  {mt.protocol_priority.join(' > ')}
                {:else if defaults.protocol_priority.length > 0}
                  <span class="inherited">{defaults.protocol_priority.join(' > ')} (global)</span>
                {:else}
                  <span class="muted">Not set</span>
                {/if}
              </span>
            </div>
            <div>
              <span class="label">Protocol Delay</span>
              <span>
                {#if mt.protocol_delay_minutes !== null}
                  {mt.protocol_delay_minutes}m
                {:else if mt.protocol_priority.length === 0 && defaults.protocol_delay_minutes !== null}
                  <span class="inherited">{defaults.protocol_delay_minutes}m (global)</span>
                {:else}
                  None
                {/if}
              </span>
            </div>
            <div>
              <span class="label">Created</span>
              <span>{formatDate(mt.created_at)}</span>
            </div>
            <HandlerDefaults mediaTypeId={mt.id} />
          </div>
        {/if}
      </article>
    {/each}
  {/if}
</div>

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
  }

  .card-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(340px, 1fr));
    gap: 16px;
  }

  @media (max-width: 768px) {
    .card-grid {
      grid-template-columns: 1fr;
    }

    .meta-grid {
      grid-template-columns: 1fr;
    }
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
    margin-bottom: 12px;
  }

  .card h3 {
    margin: 0 0 4px;
    font-size: 16px;
  }

  .card code {
    font-size: 12px;
    padding: 2px 6px;
    background: var(--bg);
    border-radius: 4px;
    border: 1px solid var(--ring);
  }

  .meta-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
    font-size: 12px;
  }

  .meta-grid .label {
    display: block;
    color: var(--muted);
    margin-bottom: 2px;
    font-size: 11px;
  }

  .form-group {
    margin-bottom: 14px;
  }

  .form-group label {
    display: block;
    font-size: 12px;
    font-weight: 500;
    margin-bottom: 4px;
  }

  .form-group input {
    width: 100%;
    padding: 7px 10px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 8px;
    color: var(--ink);
    font-size: 13px;
    box-sizing: border-box;
  }

  .hint {
    display: block;
    font-size: 11px;
    color: var(--muted);
    margin-top: 3px;
  }

  .form-actions {
    display: flex;
    gap: 8px;
    margin-top: 16px;
  }

  .btn {
    padding: 8px 14px;
    border-radius: 8px;
    border: 1px solid var(--accent);
    background: var(--accent);
    color: #1a1b14;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
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

  .btn.small {
    padding: 5px 10px;
    font-size: 11px;
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

  .inherited {
    color: var(--muted);
    font-style: italic;
  }

  .empty-state,
  .loading {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
    grid-column: 1 / -1;
  }
</style>
