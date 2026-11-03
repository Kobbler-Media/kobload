<script lang="ts">
  import {
    listHandlerTypeDefaults,
    upsertHandlerTypeDefault,
    listIndexerHandlers,
    type HandlerTypeDefault,
    type IndexerHandlerInfo
  } from '#lib/api/client.js';

  interface Props {
    mediaTypeId: string;
  }

  let { mediaTypeId }: Props = $props();

  let defaults = $state<HandlerTypeDefault[]>([]);
  let handlers = $state<IndexerHandlerInfo[]>([]);
  let loading = $state(false);
  let expanded = $state(false);
  let editingHandler = $state('');
  let editingCats = $state('');
  let saving = $state(false);
  let error = $state('');

  const load = async () => {
    loading = true;
    try {
      [defaults, handlers] = await Promise.all([
        listHandlerTypeDefaults(mediaTypeId),
        listIndexerHandlers()
      ]);
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load';
    } finally {
      loading = false;
    }
  };

  const toggle = () => {
    expanded = !expanded;
    if (expanded && defaults.length === 0 && !loading) {
      load();
    }
  };

  const startEdit = (handlerType: string) => {
    editingHandler = handlerType;
    const existing = defaults.find((d) => d.handler_type === handlerType);
    editingCats = existing ? existing.category_ids.join(', ') : '';
  };

  const saveDefault = async () => {
    saving = true;
    error = '';
    try {
      const cats = editingCats
        .split(',')
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
      await upsertHandlerTypeDefault(mediaTypeId, {
        handler_type: editingHandler,
        category_ids: cats
      });
      editingHandler = '';
      await load();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to save';
    } finally {
      saving = false;
    }
  };

  const getCatsForHandler = (handlerType: string): string => {
    const d = defaults.find((d) => d.handler_type === handlerType);
    return d && d.category_ids.length > 0 ? d.category_ids.join(', ') : '';
  };
</script>

<div class="handler-defaults">
  <button class="toggle-btn" type="button" onclick={toggle}>
    <span class="toggle-icon">{expanded ? '−' : '+'}</span>
    Category Defaults by Handler
    {#if defaults.length > 0}
      <span class="count">{defaults.length}</span>
    {/if}
  </button>

  {#if expanded}
    {#if error}
      <p class="error-inline">{error}</p>
    {/if}

    {#if loading}
      <p class="muted">Loading...</p>
    {:else}
      <div class="handler-list">
        {#each handlers as handler (handler.id)}
          {@const cats = getCatsForHandler(handler.id)}
          <div class="handler-row">
            <div class="handler-info">
              <span class="handler-name">{handler.name}</span>
              {#if editingHandler === handler.id}
                <div class="inline-edit">
                  <input
                    type="text"
                    bind:value={editingCats}
                    placeholder="e.g. 7020, 7030"
                    class="cats-input"
                  />
                  <button class="btn tiny" type="button" disabled={saving} onclick={saveDefault}>
                    {saving ? '...' : 'Save'}
                  </button>
                  <button class="btn tiny alt" type="button" onclick={() => (editingHandler = '')}>
                    Cancel
                  </button>
                </div>
              {:else}
                <span class="cats-display" class:empty={!cats}>
                  {cats || 'No default categories'}
                </span>
              {/if}
            </div>
            {#if editingHandler !== handler.id}
              <button class="btn tiny alt" type="button" onclick={() => startEdit(handler.id)}>
                Edit
              </button>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .handler-defaults {
    margin-top: 12px;
    border-top: 1px solid var(--ring);
    padding-top: 10px;
  }

  .toggle-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    color: var(--muted);
    cursor: pointer;
    font-size: 11px;
    padding: 0;
  }

  .toggle-btn:hover {
    color: var(--ink);
  }

  .toggle-icon {
    font-weight: bold;
    font-size: 14px;
    width: 16px;
  }

  .count {
    background: var(--ring);
    padding: 0 5px;
    border-radius: 8px;
    font-size: 10px;
  }

  .handler-list {
    margin-top: 8px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .handler-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }

  .handler-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }

  .handler-name {
    font-weight: 500;
    font-size: 11px;
  }

  .cats-display {
    font-size: 11px;
    color: var(--ink);
  }

  .cats-display.empty {
    color: var(--muted);
    font-style: italic;
  }

  .inline-edit {
    display: flex;
    gap: 4px;
    align-items: center;
    margin-top: 2px;
  }

  .cats-input {
    padding: 3px 6px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 6px;
    color: var(--ink);
    font-size: 11px;
    flex: 1;
    min-width: 0;
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

  .btn.tiny {
    padding: 2px 6px;
    font-size: 10px;
    border-radius: 4px;
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
  }

  .btn.tiny.alt {
    color: var(--muted);
  }

  .error-inline {
    color: #dc3545;
    font-size: 11px;
    margin: 4px 0;
  }

  .muted {
    color: var(--muted);
    font-size: 11px;
    margin: 4px 0;
  }
</style>
