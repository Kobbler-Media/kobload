<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal, SearchSelect, type SearchSelectItem } from '@koblibs/ui';
  import DirectoryBrowser from '$lib/components/DirectoryBrowser.svelte';
  import { createWatchDirectory, updateWatchDirectory } from '$lib/api/client';
  import { discardGuard } from '$lib/discardGuard';
  import type { WatchDirectory } from '$lib/types';

  let {
    directory,
    onClose,
    onSaved
  }: {
    directory: WatchDirectory | null;
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const formId = 'watch-directory-edit-form';

  const mediaTypeItems: SearchSelectItem[] = [
    { value: 'book', label: 'Book' },
    { value: 'tv', label: 'TV' },
    { value: 'movie', label: 'Movie' }
  ];

  let form = $state(
    untrack(() => ({
      path: directory?.path ?? '',
      name: directory?.name ?? '',
      media_type: (directory?.media_type ?? 'book') as 'book' | 'tv' | 'movie',
      callback_url: directory?.callback_url ?? '',
      auth_token: directory?.auth_token ?? '',
      recursive: directory?.recursive ?? true,
      enabled: directory?.enabled ?? true
    }))
  );

  let saving = $state(false);
  let formError = $state('');
  let showBrowser = $state(false);

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  const save = async () => {
    if (!form.path.trim() || !form.name.trim() || !form.callback_url.trim()) {
      formError = 'Path, name, and callback URL are required.';
      return;
    }
    saving = true;
    formError = '';
    try {
      const payload = {
        path: form.path.trim(),
        name: form.name.trim(),
        media_type: form.media_type,
        callback_url: form.callback_url.trim(),
        auth_token: form.auth_token.trim() || null,
        recursive: form.recursive,
        enabled: form.enabled
      };
      if (directory) {
        await updateWatchDirectory(directory.id, payload);
      } else {
        await createWatchDirectory(payload);
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save watch directory';
    } finally {
      saving = false;
    }
  };
</script>

<Modal
  title={directory ? `Edit ${directory.name}` : 'Add Watch Directory'}
  sheet
  {onClose}
  {canClose}
>
  <form
    id={formId}
    class="modal-form"
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    {#if formError}
      <div class="error">{formError}</div>
    {/if}

    <p class="intro">
      Watch directories scan for new files and send them to the configured callback URL (e.g.
      Kobbler's ingest endpoint).
    </p>

    <div class="form-grid cols-1">
      <div class="field">
        <span>Path</span>
        <div class="path-row">
          <input type="text" bind:value={form.path} placeholder="/path/to/watch" />
          <button class="btn compact alt" type="button" onclick={() => (showBrowser = true)}>
            Browse
          </button>
        </div>
      </div>
      <label>
        Name
        <input type="text" bind:value={form.name} placeholder="My Books" />
      </label>
      <label>
        Media type
        <SearchSelect
          id="media-type"
          items={mediaTypeItems}
          value={form.media_type}
          onchange={(v) => {
            form.media_type = v as 'book' | 'tv' | 'movie';
          }}
          placeholder="Select..."
        />
      </label>
      <label>
        Callback URL
        <input
          type="text"
          bind:value={form.callback_url}
          placeholder="http://kobbler:6766/api/v1/admin/ingest/your-token"
        />
      </label>
      <label>
        Auth token
        <input
          type="text"
          bind:value={form.auth_token}
          placeholder="Bearer token (optional)"
          autocomplete="off"
        />
      </label>
    </div>

    <div class="checkbox-row">
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.recursive} />
        Recursive
      </label>
      <label class="checkbox">
        <input type="checkbox" bind:checked={form.enabled} />
        Enabled
      </label>
    </div>
  </form>

  {#snippet footer()}
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {directory ? 'Update' : 'Add directory'}
    </button>
  {/snippet}
</Modal>

<DirectoryBrowser
  open={showBrowser}
  initialPath={form.path || '/'}
  onSelect={(path) => {
    form.path = path;
    if (!form.name) {
      const parts = path.split('/').filter(Boolean);
      form.name = parts[parts.length - 1] ?? '';
    }
    showBrowser = false;
  }}
  onClose={() => (showBrowser = false)}
/>

<style>
  .intro {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    line-height: 1.5;
  }

  .path-row {
    display: flex;
    gap: 8px;
  }

  .path-row input {
    flex: 1;
    min-width: 0;
  }
</style>
