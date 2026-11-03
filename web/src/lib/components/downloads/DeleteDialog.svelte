<script lang="ts">
  import type { DownloadItem } from '#lib/api/client.js';

  type DeleteMode =
    { kind: 'single'; item: DownloadItem } | { kind: 'clear'; downloads: DownloadItem[] };

  interface Props {
    mode: DeleteMode | null;
    loading: boolean;
    onConfirm: (cancelProtocols: string[]) => void;
    onClose: () => void;
  }

  let { mode, loading, onConfirm, onClose }: Props = $props();

  const ACTIVE_STATUSES = ['queued', 'downloading'];

  // Determine which protocols are present
  const protocols = $derived.by(() => {
    if (!mode) return [];
    if (mode.kind === 'single') {
      const item = mode.item;
      if (!item.download_client_id) return [];
      return [item.protocol];
    }
    // Clear mode: unique protocols among inactive downloads that have a client
    const seen = new Set<string>();
    for (const d of mode.downloads) {
      if (!ACTIVE_STATUSES.includes(d.status) && d.download_client_id) {
        seen.add(d.protocol);
      }
    }
    return [...seen].sort();
  });

  const protocolCounts = $derived.by(() => {
    if (!mode || mode.kind !== 'clear') return {};
    const counts: Record<string, number> = {};
    for (const d of mode.downloads) {
      if (!ACTIVE_STATUSES.includes(d.status) && d.download_client_id) {
        counts[d.protocol] = (counts[d.protocol] || 0) + 1;
      }
    }
    return counts;
  });

  const inactiveCount = $derived(
    mode?.kind === 'clear'
      ? mode.downloads.filter((d) => !ACTIVE_STATUSES.includes(d.status)).length
      : 0
  );

  const isActive = $derived(
    mode?.kind === 'single' ? ACTIVE_STATUSES.includes(mode.item.status) : false
  );

  // Checkbox state: default usenet checked, torrent unchecked
  let cancelFlags = $state<Record<string, boolean>>({});

  // Reset checkbox defaults when mode changes
  $effect(() => {
    if (mode) {
      const flags: Record<string, boolean> = {};
      for (const p of protocols) {
        flags[p] = p === 'usenet';
      }
      cancelFlags = flags;
    }
  });

  function handleConfirm() {
    const selected = Object.entries(cancelFlags)
      .filter(([, v]) => v)
      .map(([k]) => k);
    onConfirm(selected);
  }

  const protocolLabel = (p: string) => p.charAt(0).toUpperCase() + p.slice(1);
</script>

{#if mode}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={onClose} onkeydown={() => {}}>
    <div
      class="modal"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
      role="dialog"
      aria-modal="true"
      tabindex="-1"
    >
      {#if mode.kind === 'single'}
        <h3>{isActive ? 'Cancel Download' : 'Remove Download'}</h3>
        <p class="dl-name">{mode.item.name}</p>
        {#if isActive}
          <p class="muted">This download is still active.</p>
        {/if}
      {:else}
        <h3>Clear Downloads</h3>
        <p class="muted">
          Remove {inactiveCount} completed/failed download{inactiveCount !== 1 ? 's' : ''}.
        </p>
      {/if}

      {#if protocols.length > 0}
        <div class="options">
          <span class="options-label">Download client cleanup</span>
          {#each protocols as protocol (protocol)}
            <label class="checkbox-row">
              <input type="checkbox" bind:checked={cancelFlags[protocol]} disabled={loading} />
              <span>
                {#if mode.kind === 'single'}
                  {isActive ? 'Cancel and remove' : 'Remove'} from {protocolLabel(protocol)} client
                {:else}
                  Remove {protocolLabel(protocol)} downloads from client
                  {#if protocolCounts[protocol]}
                    <span class="count">({protocolCounts[protocol]})</span>
                  {/if}
                {/if}
              </span>
            </label>
          {/each}
        </div>
      {/if}

      <div class="modal-actions">
        <button class="btn alt" type="button" onclick={onClose} disabled={loading}>Cancel</button>
        <button class="btn danger" type="button" onclick={handleConfirm} disabled={loading}>
          {#if loading}
            Working...
          {:else if mode.kind === 'single'}
            {isActive ? 'Cancel & Remove' : 'Remove'}
          {:else}
            Clear {inactiveCount}
          {/if}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 16px;
  }

  .modal {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 24px;
    max-width: 440px;
    width: 100%;
  }

  .modal h3 {
    margin: 0 0 8px;
    font-size: 18px;
  }

  .dl-name {
    font-size: 13px;
    color: var(--ink);
    margin: 0 0 4px;
    word-break: break-word;
  }

  .options {
    margin: 16px 0;
    display: grid;
    gap: 8px;
  }

  .options-label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
  }

  .checkbox-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
    cursor: pointer;
  }

  .checkbox-row input[type='checkbox'] {
    width: 16px;
    height: 16px;
    accent-color: var(--accent);
    cursor: pointer;
  }

  .count {
    color: var(--muted);
    font-size: 12px;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 20px;
  }
</style>
