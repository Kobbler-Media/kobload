<script lang="ts">
  import type { DecisionLogProviderCall } from '#lib/types.js';

  let {
    call
  }: {
    call: DecisionLogProviderCall;
  } = $props();

  let expanded = $state(false);

  const statusStyle = $derived(
    call.status === 'success'
      ? { bg: 'rgba(34, 197, 94, 0.15)', color: '#22c55e' }
      : call.status === 'error'
        ? { bg: 'rgba(239, 68, 68, 0.15)', color: '#ef4444' }
        : { bg: 'rgba(156, 163, 175, 0.15)', color: '#9ca3af' }
  );

  const hasDetails = $derived(call.categories || call.external_ids);
</script>

<button
  class="provider-card"
  type="button"
  onclick={() => {
    if (hasDetails) expanded = !expanded;
  }}
  class:clickable={hasDetails}
>
  <div class="provider-header">
    <div class="provider-left">
      <span class="source-badge">{call.source_name}</span>
      {#if call.handler_type}
        <span class="handler-badge">{call.handler_type}</span>
      {/if}
      {#if call.search_type}
        <span class="meta-text">{call.search_type}</span>
      {/if}
      {#if call.query}
        <code class="query-text">{call.query}</code>
      {/if}
    </div>
    <div class="provider-right">
      <span class="results-count">
        {call.results_count} result{call.results_count !== 1 ? 's' : ''}
        {#if call.results_after_dedup < call.results_count}
          <span class="dedup">({call.results_after_dedup} after dedup)</span>
        {/if}
      </span>
      {#if call.duration_ms !== null}
        <span class="duration">{call.duration_ms}ms</span>
      {/if}
      <span class="status-badge" style="background: {statusStyle.bg}; color: {statusStyle.color}">
        {call.status}
      </span>
      {#if hasDetails}
        <span class="chevron" class:expanded>&#9656;</span>
      {/if}
    </div>
  </div>

  {#if call.error_message}
    <div class="error-row">{call.error_message}</div>
  {/if}

  {#if expanded}
    <div class="provider-details">
      {#if call.categories}
        <div class="detail-section">
          <span class="detail-label">Categories</span>
          <pre class="detail-json">{JSON.stringify(call.categories, null, 2)}</pre>
        </div>
      {/if}
      {#if call.external_ids}
        <div class="detail-section">
          <span class="detail-label">External IDs</span>
          <pre class="detail-json">{JSON.stringify(call.external_ids, null, 2)}</pre>
        </div>
      {/if}
    </div>
  {/if}
</button>

<style>
  .provider-card {
    display: block;
    width: 100%;
    text-align: left;
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 10px;
    padding: 10px 14px;
    color: inherit;
    font: inherit;
    cursor: default;
  }

  .provider-card.clickable {
    cursor: pointer;
  }

  .provider-card.clickable:hover {
    border-color: var(--accent);
  }

  .provider-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .provider-left {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    min-width: 0;
  }

  .provider-right {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  .source-badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    background: rgba(59, 130, 246, 0.15);
    color: #3b82f6;
    font-weight: 600;
  }

  .handler-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--bg);
    border: 1px solid var(--ring);
    color: var(--muted);
  }

  .meta-text {
    font-size: 11px;
    color: var(--muted);
  }

  .query-text {
    font-size: 11px;
    padding: 1px 5px;
    background: var(--bg);
    border-radius: 3px;
    border: 1px solid var(--ring);
    color: var(--ink);
    max-width: 300px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .results-count {
    font-size: 11px;
    color: var(--muted);
  }

  .dedup {
    opacity: 0.7;
  }

  .duration {
    font-size: 11px;
    color: var(--muted);
  }

  .status-badge {
    font-size: 10px;
    padding: 2px 6px;
    border-radius: 4px;
    font-weight: 500;
    text-transform: capitalize;
  }

  .chevron {
    font-size: 12px;
    color: var(--muted);
    transition: transform 0.15s;
    display: inline-block;
  }

  .chevron.expanded {
    transform: rotate(90deg);
  }

  .error-row {
    margin-top: 6px;
    padding: 6px 8px;
    background: rgba(239, 68, 68, 0.08);
    border-radius: 6px;
    font-size: 12px;
    color: #ef4444;
  }

  .provider-details {
    margin-top: 8px;
    padding-top: 8px;
    border-top: 1px solid var(--ring);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .detail-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--muted);
    font-weight: 600;
  }

  .detail-json {
    font-size: 11px;
    padding: 6px 8px;
    background: var(--bg);
    border-radius: 6px;
    border: 1px solid var(--ring);
    overflow-x: auto;
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
