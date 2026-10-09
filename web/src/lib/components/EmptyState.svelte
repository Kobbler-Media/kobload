<script lang="ts">
  let {
    title = 'Nothing here yet',
    message = '',
    icon = 'empty'
  }: {
    title?: string;
    message?: string;
    icon?: 'empty' | 'search' | 'download' | 'watch' | 'error';
  } = $props();

  const icons: Record<string, string> = {
    empty: `<svg viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>`,
    search: `<svg viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>`,
    download: `<svg viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`,
    watch: `<svg viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg>`,
    error: `<svg viewBox="0 0 24 24" width="32" height="32" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>`
  };
</script>

<div class="empty-state">
  <div class="empty-icon">
    <!-- eslint-disable-next-line svelte/no-at-html-tags -- trusted SVG from static map -->
    {@html icons[icon] ?? icons.empty}
  </div>
  <h3>{title}</h3>
  {#if message}
    <p>{message}</p>
  {/if}
</div>

<style>
  .empty-state {
    text-align: center;
    padding: 40px 24px;
    border-radius: 12px;
    border: 1px dashed var(--ring);
    background: var(--card);
  }

  .empty-icon {
    color: var(--muted);
    opacity: 0.4;
    margin-bottom: 12px;
  }

  h3 {
    margin: 0 0 6px;
    font-size: 15px;
    font-weight: 600;
    color: var(--ink);
  }

  p {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
  }
</style>
