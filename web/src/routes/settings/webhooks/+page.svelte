<script lang="ts">
  import { onMount } from 'svelte';
  import { listWebhooks, updateWebhook, deleteWebhook, type StatusWebhook } from '$lib/api/client';
  import WebhookEditModal from '$lib/components/settings/WebhookEditModal.svelte';

  const VALID_EVENTS = [
    'download.created',
    'download.started',
    'download.completed',
    'download.failed',
    'download.cancelled'
  ];

  let loading = $state(false);
  let error = $state('');
  let webhooks = $state<StatusWebhook[]>([]);

  let editorOpen = $state(false);
  let editing = $state<StatusWebhook | null>(null);

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      webhooks = await listWebhooks();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load webhooks';
    } finally {
      loading = false;
    }
  };

  const openEditor = (wh: StatusWebhook | null) => {
    editing = wh;
    editorOpen = true;
  };

  const closeEditor = () => {
    editorOpen = false;
    editing = null;
  };

  const handleSaved = async () => {
    closeEditor();
    await refresh();
  };

  const handleDelete = async (id: string) => {
    if (!confirm('Delete this webhook?')) return;
    error = '';
    try {
      await deleteWebhook(id);
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete webhook';
    }
  };

  const handleToggle = async (wh: StatusWebhook) => {
    error = '';
    try {
      await updateWebhook(wh.id, { enabled: !wh.enabled });
      await refresh();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to update webhook';
    }
  };

  onMount(refresh);
</script>

<div class="page-header">
  <div>
    <h1>Webhooks</h1>
    <p class="muted">Receive HTTP notifications when download events occur.</p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add Webhook</button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<div class="webhook-list">
  {#if loading && webhooks.length === 0}
    <div class="loading">Loading...</div>
  {:else if webhooks.length === 0}
    <div class="empty-state">No webhooks configured.</div>
  {:else}
    {#each webhooks as wh (wh.id)}
      <article class="card" class:disabled={!wh.enabled}>
        <div class="card-header">
          <div class="webhook-info">
            <code class="webhook-url">{wh.url}</code>
            <div class="webhook-meta">
              <span
                class="badge"
                class:badge-active={wh.enabled}
                class:badge-inactive={!wh.enabled}
              >
                {wh.enabled ? 'Active' : 'Disabled'}
              </span>
              {#if wh.events.length > 0}
                {#each wh.events as event (event)}
                  <span class="event-tag">{event}</span>
                {/each}
              {:else}
                <span class="event-tag all">All events</span>
              {/if}
            </div>
          </div>
          <div class="card-actions">
            <button
              class="btn tiny"
              type="button"
              onclick={() => handleToggle(wh)}
              title={wh.enabled ? 'Disable' : 'Enable'}
            >
              {wh.enabled ? 'Disable' : 'Enable'}
            </button>
            <button class="btn tiny" type="button" onclick={() => openEditor(wh)}>Edit</button>
            <button class="btn tiny danger" type="button" onclick={() => handleDelete(wh.id)}>
              Delete
            </button>
          </div>
        </div>
      </article>
    {/each}
  {/if}
</div>

{#if editorOpen}
  <WebhookEditModal
    webhook={editing}
    events={VALID_EVENTS}
    onClose={closeEditor}
    onSaved={handleSaved}
  />
{/if}

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

  .header-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 18px;
  }

  .card.disabled {
    opacity: 0.6;
  }

  .webhook-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
  }

  .webhook-info {
    min-width: 0;
    flex: 1;
  }

  .webhook-url {
    font-size: 13px;
    padding: 2px 6px;
    background: var(--bg);
    border-radius: 4px;
    border: 1px solid var(--ring);
    word-break: break-all;
  }

  .webhook-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 8px;
  }

  .badge {
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 10px;
    font-weight: 500;
  }

  .badge-active {
    background: rgba(40, 167, 69, 0.2);
    color: #28a745;
  }

  .badge-inactive {
    background: rgba(108, 117, 125, 0.2);
    color: #6c757d;
  }

  .event-tag {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(211, 140, 90, 0.15);
    color: var(--accent);
  }

  .event-tag.all {
    background: rgba(108, 117, 125, 0.15);
    color: var(--muted);
  }

  .card-actions {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
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

  .btn.tiny {
    padding: 3px 8px;
    font-size: 11px;
    border-radius: 6px;
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
  }

  .btn.tiny.danger {
    color: #dc3545;
    border-color: rgba(220, 53, 69, 0.3);
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

  .empty-state,
  .loading {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      align-items: stretch;
    }
  }

  @media (max-width: 768px) {
    .card-header {
      flex-direction: column;
      gap: 8px;
    }

    .card-actions {
      align-self: flex-start;
    }
  }
</style>
