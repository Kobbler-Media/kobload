<script lang="ts">
  import type { StreamAttempt, StreamAttemptEvent } from '#lib/api/client.js';
  import { getStreamAttemptStatusStyle } from '#lib/styles/statusColors.js';
  import { streamEventTones } from '#lib/styles/eventTones.js';
  import EventTimeline from '../EventTimeline.svelte';

  interface Props {
    attempt: StreamAttempt;
    isExpanded: boolean;
    events: StreamAttemptEvent[] | undefined;
    loadingEvents: boolean;
    onToggleExpand: (id: string) => void;
  }

  let { attempt, isExpanded, events, loadingEvents, onToggleExpand }: Props = $props();

  const status = $derived(getStreamAttemptStatusStyle(attempt.status));
  const title = $derived(attempt.release_title ?? attempt.file_name ?? attempt.info_hash);
  const showProgress = $derived(attempt.status === 'caching' && attempt.progress !== null);

  const cachedLabel = (cached: boolean | null) =>
    cached === null ? null : cached ? 'Already cached' : 'Not cached';

  const elapsed = (from: string, to: string) => {
    const ms = new Date(to).getTime() - new Date(from).getTime();
    return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(1)}s`;
  };

  const toggle = (e: Event) => {
    if ((e.target as HTMLElement).closest('a, button, details, summary')) return;
    onToggleExpand(attempt.id);
  };
</script>

<div
  class="attempt-row"
  class:expanded={isExpanded}
  role="button"
  tabindex="0"
  onclick={toggle}
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggle(e);
    }
  }}
>
  <div class="row-main">
    <div class="row-info">
      <div class="row-title">{title}</div>
      <div class="row-meta">
        <span>{attempt.origin === 'play' ? 'Playback' : 'Warm probe'}</span>
        <span>{attempt.provider_name}</span>
        {#if cachedLabel(attempt.cached)}
          <span class:cached={attempt.cached}>{cachedLabel(attempt.cached)}</span>
        {/if}
        {#if attempt.ready_at}
          <span>ready in {elapsed(attempt.created_at, attempt.ready_at)}</span>
        {/if}
        <span title={new Date(attempt.created_at).toLocaleString()}>
          {new Date(attempt.created_at).toLocaleTimeString()}
        </span>
      </div>
      {#if attempt.error}
        <div class="row-error">{attempt.error}</div>
      {/if}
    </div>
    <div class="row-status">
      {#if showProgress}
        <div class="row-progress">
          <div
            class="row-progress-bar"
            style={`width: ${Math.round((attempt.progress ?? 0) * 100)}%`}
          ></div>
        </div>
        <span class="row-progress-text">{Math.round((attempt.progress ?? 0) * 100)}%</span>
      {/if}
      <span class="status-chip" style={`color: ${status.bg}; border-color: ${status.bg}`}>
        {status.label}
      </span>
    </div>
  </div>
  {#if isExpanded}
    <div class="details-panel">
      <div class="details-grid">
        <div>
          <span class="label">Info hash</span>
          <span class="mono break-word">{attempt.info_hash}</span>
        </div>
        {#if attempt.file_name}
          <div>
            <span class="label">File</span>
            <span class="mono break-word">{attempt.file_name}</span>
          </div>
        {/if}
        {#if attempt.torrent_id}
          <div>
            <span class="label">Provider torrent</span>
            <span class="mono">{attempt.torrent_id}</span>
          </div>
        {/if}
        {#if attempt.ready_at}
          <div>
            <span class="label">Ready at</span>
            <span>{new Date(attempt.ready_at).toLocaleString()}</span>
          </div>
        {/if}
        <div>
          <span class="label">Last update</span>
          <span>{new Date(attempt.updated_at).toLocaleString()}</span>
        </div>
      </div>
      <EventTimeline {events} {loadingEvents} tones={streamEventTones} />
    </div>
  {/if}
</div>

<style>
  .attempt-row {
    background: var(--card);
    padding: 10px 14px;
    cursor: pointer;
    transition: background 0.1s;
  }

  .attempt-row:hover,
  .attempt-row.expanded {
    background: var(--bg-2);
  }

  .row-main {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .row-info {
    flex: 1;
    min-width: 0;
  }

  .row-title {
    font-size: 14px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .row-meta {
    font-size: 12px;
    color: var(--muted);
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .row-meta > :global(span + span)::before {
    content: '·';
    margin-right: 4px;
    opacity: 0.5;
  }

  .row-meta .cached {
    color: #3fb950;
  }

  .row-error {
    margin-top: 4px;
    font-size: 12px;
    color: #dc3545;
    overflow-wrap: anywhere;
  }

  .row-status {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .row-progress {
    width: 60px;
    height: 4px;
    background: var(--ring);
    border-radius: 2px;
    overflow: hidden;
  }

  .row-progress-bar {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s ease;
  }

  .row-progress-text {
    font-size: 12px;
    color: var(--muted);
    min-width: 32px;
  }

  .details-panel {
    margin-top: 8px;
    padding-top: 12px;
    border-top: 1px solid var(--ring);
    display: grid;
    gap: 16px;
  }

  .details-grid {
    display: grid;
    gap: 8px;
    font-size: 13px;
  }

  .details-grid .label {
    display: block;
    color: var(--muted);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .mono {
    font-family: monospace;
    font-size: 12px;
  }

  .break-word {
    word-break: break-all;
  }

  @media (max-width: 640px) {
    .row-main {
      gap: 8px;
    }

    .row-progress {
      width: 40px;
    }

    .row-progress-text {
      display: none;
    }
  }
</style>
