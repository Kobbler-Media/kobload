<script lang="ts">
  import { browseFilesystem } from '#lib/api/client.js';
  import { registerOverlay, unregisterOverlay } from '#lib/stores/ui.svelte.js';
  import { uid } from '@koblibs/ui';
  import type { DirectoryEntry } from '#lib/types.js';

  interface Props {
    open: boolean;
    initialPath?: string;
    onSelect: (path: string) => void;
    onClose: () => void;
  }

  let { open, initialPath, onSelect, onClose }: Props = $props();

  let currentPath = $state('/');
  let parentPath = $state<string | null>(null);
  let directories = $state<DirectoryEntry[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);

  const breadcrumbs = $derived.by(() => {
    const parts = currentPath.split('/').filter(Boolean);
    const crumbs = [{ name: '/', path: '/' }];
    let accumulated = '';
    for (const part of parts) {
      accumulated += '/' + part;
      crumbs.push({ name: part, path: accumulated });
    }
    return crumbs;
  });

  async function loadDirectory(path: string) {
    loading = true;
    error = null;
    try {
      const resp = await browseFilesystem(path);
      currentPath = resp.current_path;
      parentPath = resp.parent_path;
      directories = resp.directories;
    } catch (e) {
      error = e instanceof Error ? e.message : 'Failed to browse directory';
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (open) {
      loadDirectory(initialPath || '/');
    }
  });

  // The browser opens on top of the watch-directory editor, so it has to own the
  // top of the overlay stack — otherwise Escape/Back closes the editor beneath it.
  const overlayId = `dir-browser-${uid()}`;
  $effect(() => {
    if (!open) return;
    registerOverlay(overlayId, onClose);
    return () => unregisterOverlay(overlayId);
  });
</script>

{#if open}
  <button class="modal-backdrop" type="button" aria-label="Close" onclick={onClose}></button>
  <div class="modal">
    <div class="modal-header">
      <h3>Browse Directory</h3>
      <button class="icon-btn" type="button" aria-label="Close" onclick={onClose}>
        <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor">
          <path d="M6.4 5.6 5 7l5 5-5 5 1.4 1.4 5-5 5 5L17.8 17l-5-5 5-5-1.4-1.4-5 5-5-5z"></path>
        </svg>
      </button>
    </div>

    <div class="browser">
      <nav class="breadcrumbs">
        {#each breadcrumbs as crumb, i (crumb.path)}
          {#if i > 0}<span class="separator">/</span>{/if}
          <button
            class="crumb"
            type="button"
            onclick={() => loadDirectory(crumb.path)}
            disabled={loading}
          >
            {crumb.name}
          </button>
        {/each}
      </nav>

      <div class="current-path">
        <code>{currentPath}</code>
      </div>

      {#if error}
        <div class="error-msg">{error}</div>
      {/if}

      <div class="dir-list">
        {#if parentPath}
          <button
            class="dir-item"
            type="button"
            onclick={() => loadDirectory(parentPath!)}
            disabled={loading}
          >
            <svg class="folder-icon" viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
              <path d="M20 10H4V6h5.17l2 2H20v2zm0 2H4v6h16v-6z"></path>
            </svg>
            <span class="dir-name">..</span>
          </button>
        {/if}
        {#if loading}
          <div class="loading-msg">Loading...</div>
        {:else if directories.length === 0 && !error}
          <div class="empty-msg">No subdirectories</div>
        {:else}
          {#each directories as dir (dir.path)}
            <button
              class="dir-item"
              type="button"
              onclick={() => loadDirectory(dir.path)}
              disabled={loading}
            >
              <svg
                class="folder-icon"
                viewBox="0 0 24 24"
                width="16"
                height="16"
                fill="currentColor"
              >
                <path
                  d="M20 6h-8l-2-2H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm0 12H4V8h16v10z"
                ></path>
              </svg>
              <span class="dir-name">{dir.name}</span>
            </button>
          {/each}
        {/if}
      </div>
    </div>

    <div class="modal-actions">
      <button class="btn alt" type="button" onclick={onClose}>Cancel</button>
      <button class="btn" type="button" onclick={() => onSelect(currentPath)} disabled={loading}>
        Select This Directory
      </button>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    /* Above a koblibs Modal (101) — the browser opens from inside the edit modal. */
    z-index: 120;
    border: none;
    cursor: pointer;
  }

  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 20px;
    z-index: 121;
    width: 520px;
    max-width: 90vw;
    max-height: 90vh;
    overflow: auto;
  }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }

  .modal-header h3 {
    margin: 0;
    font-size: 16px;
  }

  .icon-btn {
    border-radius: 12px;
    padding: 6px 8px;
    border: 1px solid var(--ring);
    background: var(--card);
    color: var(--ink);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 16px;
  }

  .browser {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .breadcrumbs {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    font-size: 12px;
  }

  .crumb {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 4px;
    font-size: 12px;
  }

  .crumb:hover:not(:disabled) {
    background: var(--ring);
  }

  .separator {
    color: var(--muted);
  }

  .current-path {
    padding: 6px 10px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 8px;
  }

  .current-path code {
    font-size: 12px;
    word-break: break-all;
  }

  .dir-list {
    max-height: 340px;
    overflow-y: auto;
    border: 1px solid var(--ring);
    border-radius: 8px;
  }

  .dir-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 12px;
    border: none;
    background: transparent;
    color: var(--ink);
    font-size: 13px;
    cursor: pointer;
    text-align: left;
  }

  .dir-item:hover:not(:disabled) {
    background: var(--ring);
  }

  .dir-item + .dir-item {
    border-top: 1px solid var(--ring);
  }

  .folder-icon {
    flex-shrink: 0;
    color: var(--muted);
  }

  .dir-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .loading-msg,
  .empty-msg,
  .error-msg {
    padding: 16px;
    text-align: center;
    font-size: 12px;
    color: var(--muted);
  }

  .error-msg {
    color: var(--danger, #dc3545);
    background: color-mix(in srgb, var(--danger, #dc3545) 10%, transparent);
    border-radius: 8px;
  }

  .btn {
    padding: 8px 16px;
    border-radius: 12px;
    border: none;
    background: var(--accent);
    color: #1a1b14;
    font-weight: 600;
    font-size: 13px;
    cursor: pointer;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn.alt {
    background: var(--card);
    border: 1px solid var(--ring);
    color: var(--ink);
  }
</style>
