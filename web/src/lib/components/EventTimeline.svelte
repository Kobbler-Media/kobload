<script lang="ts">
  import type { EventTone, TimelineEvent } from '#lib/styles/eventTones.js';

  interface Props {
    events: TimelineEvent[] | undefined;
    loadingEvents: boolean;
    tones: Record<string, EventTone>;
  }

  let { events, loadingEvents, tones }: Props = $props();
</script>

<div class="details-section">
  <h4>Event Timeline</h4>
  {#if loadingEvents}
    <div class="loading-text">Loading events...</div>
  {:else if events && events.length > 0}
    <ul class="event-timeline">
      {#each events as event (event.id)}
        <li class="event-item">
          <div class="event-header">
            <span class={`event-type tone-${tones[event.event_type] ?? 'none'}`}>
              {event.event_type.replace(/_/g, ' ')}
            </span>
            <span class="event-time">
              {new Date(event.created_at).toLocaleString()}
            </span>
          </div>
          {#if event.message}
            <div class="event-message">{event.message}</div>
          {/if}
          {#if event.details}
            <details class="event-details">
              <summary>Details</summary>
              <pre>{JSON.stringify(event.details, null, 2)}</pre>
            </details>
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <div class="muted">No events recorded.</div>
  {/if}
</div>

<style>
  .details-section {
    display: grid;
    gap: 8px;
  }

  .details-section h4 {
    margin: 0;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
  }

  .loading-text {
    font-size: 12px;
    color: var(--muted);
  }

  .event-timeline {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }

  .event-item {
    padding: 8px;
    background: rgba(0, 0, 0, 0.08);
    border-radius: 8px;
    border-left: 3px solid var(--ring);
  }

  :global([data-theme='light']) .event-item {
    background: rgba(0, 0, 0, 0.04);
  }

  .event-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .event-type {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.1);
  }

  .tone-good {
    background: rgba(46, 160, 67, 0.2);
    color: #3fb950;
  }

  .tone-info {
    background: rgba(59, 130, 246, 0.2);
    color: #3b82f6;
  }

  .tone-accent {
    background: rgba(139, 92, 246, 0.2);
    color: #a78bfa;
  }

  .tone-bad {
    background: rgba(220, 53, 69, 0.2);
    color: #dc3545;
  }

  .tone-warn {
    background: rgba(202, 138, 4, 0.2);
    color: #ca8a04;
  }

  .event-time {
    font-size: 11px;
    color: var(--muted);
  }

  .event-message {
    margin-top: 4px;
    font-size: 12px;
  }

  .event-details {
    margin-top: 4px;
  }

  .event-details summary {
    font-size: 11px;
  }

  .event-details pre {
    margin: 4px 0 0;
    padding: 6px;
    background: rgba(0, 0, 0, 0.1);
    border-radius: 4px;
    font-size: 10px;
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
  }
</style>
