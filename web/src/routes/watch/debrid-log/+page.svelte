<script lang="ts">
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { ScrollToTop } from '@koblibs/ui';
  import LogToggle from '#lib/components/decision-log/LogToggle.svelte';
  import StreamAttempts from '#lib/components/debrid-log/StreamAttempts.svelte';
  import SearchSessions from '#lib/components/debrid-log/SearchSessions.svelte';

  const tabs = [
    { id: 'streams', label: 'Streams' },
    { id: 'searches', label: 'Searches' }
  ] as const;

  const active = $derived(page.url.searchParams.get('tab') === 'searches' ? 'searches' : 'streams');

  const select = (id: string) => {
    const url = new URL(page.url.href);
    url.searchParams.set('tab', id);
    goto(url, { replace: true, reset: false });
  };
</script>

<div class="page-header">
  <div>
    <h1>Debrid Log</h1>
    <p class="muted">
      Which streams were tried, what was cached, and why anything failed. Kept for a day.
    </p>
  </div>
  <div class="header-actions">
    <LogToggle type="debrid" />
  </div>
</div>

<div class="tabs" role="tablist">
  {#each tabs as tab (tab.id)}
    <button
      class="tab"
      class:active={active === tab.id}
      type="button"
      role="tab"
      aria-selected={active === tab.id}
      onclick={() => select(tab.id)}
    >
      {tab.label}
    </button>
  {/each}
</div>

{#if active === 'streams'}
  <StreamAttempts />
{:else}
  <SearchSessions />
{/if}

<ScrollToTop />

<style>
  .muted {
    color: var(--muted);
    font-size: 13px;
    margin: 0;
  }

  .tabs {
    display: inline-flex;
    gap: 2px;
    background: var(--bg);
    border: 1px solid var(--ring);
    border-radius: 8px;
    padding: 2px;
    margin-bottom: 16px;
  }

  .tab {
    padding: 6px 14px;
    border-radius: 6px;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
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
</style>
