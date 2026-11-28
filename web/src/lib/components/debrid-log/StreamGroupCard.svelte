<script lang="ts">
  import { formatBytes } from '@koblibs/ui';
  import type {
    StreamAttempt,
    StreamAttemptEvent,
    StreamSearch,
    StreamSearchOption
  } from '#lib/api/client.js';
  import { getStreamAttemptStatusStyle } from '#lib/styles/statusColors.js';
  import { formatEta, formatSpeed } from '../downloads/usenetFormat.js';
  import StreamAttemptCard from './StreamAttemptCard.svelte';

  interface Props {
    search: StreamSearch;
    attempts: StreamAttempt[];
    expandedIds: Set<string>;
    eventCache: Record<string, StreamAttemptEvent[]>;
    loadingEvents: Set<string>;
    onToggleExpand: (id: string) => void;
  }

  let { search, attempts, expandedIds, eventCache, loadingEvents, onToggleExpand }: Props =
    $props();

  const COLLAPSED_OPTIONS = 8;
  let showAll = $state(false);
  let openOptions = $state<Set<number>>(new Set());

  const sameKey = (a: string | null, b: string | null) =>
    !!a && !!b && a.toLowerCase() === b.toLowerCase();

  // Usenet options are listed by indexer reference and played by release key, so a title
  // match stands in when the keys differ.
  const isFor = (a: StreamAttempt, o: StreamSearchOption) =>
    (sameKey(a.release_key, o.release_key) &&
      (o.file_id === null || a.file_id === null || a.file_id === o.file_id)) ||
    (a.release_title !== null && a.release_title === o.release_title);

  const optionOf = (a: StreamAttempt) => search.options.find((o) => isFor(a, o));
  const attemptsFor = (o: StreamSearchOption) => attempts.filter((a) => optionOf(a) === o);
  const unlisted = $derived(attempts.filter((a) => !optionOf(a)));

  const picked = $derived(attempts.find((a) => a.origin === 'play'));
  const pickedOption = $derived(picked ? optionOf(picked) : undefined);
  const visibleOptions = $derived(
    showAll ? search.options : search.options.slice(0, COLLAPSED_OPTIONS)
  );

  const pct = (p: number | null) => Math.round((p ?? 0) * 100);

  const eta = (a: StreamAttempt, o: StreamSearchOption | undefined) => {
    if (!a.speed || !o?.size || a.progress === null) return null;
    return formatEta(Math.round(((1 - a.progress) * o.size) / a.speed));
  };

  const availabilityLabel: Record<string, string> = {
    ready: 'Cached here',
    likely: 'Likely cached',
    unknown: 'Unknown',
    uncached: 'Not cached'
  };

  const toggleOption = (rank: number) => {
    const next = new Set(openOptions);
    if (next.has(rank)) next.delete(rank);
    else next.add(rank);
    openOptions = next;
  };
</script>

<section class="group-card">
  <header class="group-header">
    <div class="group-title">
      <h3>{search.title}</h3>
      <div class="group-meta">
        <span title={new Date(search.created_at).toLocaleString()}>
          {new Date(search.created_at).toLocaleTimeString()}
        </span>
        <span>{search.options.length} option{search.options.length === 1 ? '' : 's'}</span>
        <span>{attempts.length} tried</span>
        <a href="/watch/decision-log/{search.id}">Scoring details</a>
      </div>
    </div>
  </header>

  {#if picked}
    {@const status = getStreamAttemptStatusStyle(picked.status)}
    <div class="picked">
      <div class="picked-head">
        <span class="picked-label">Playing</span>
        {#if pickedOption}
          <span class="rank">#{pickedOption.rank}</span>
        {/if}
        <span class="picked-title">{picked.release_title ?? picked.release_key}</span>
        <span class="status-chip" style={`color: ${status.bg}; border-color: ${status.bg}`}>
          {status.label}
        </span>
      </div>
      {#if picked.status === 'caching'}
        <div class="picked-progress">
          <div class="bar"><div class="fill" style={`width: ${pct(picked.progress)}%`}></div></div>
          <span>{pct(picked.progress)}%</span>
          {#if picked.speed}
            <span>{formatSpeed(picked.speed)}</span>
          {/if}
          {#if eta(picked, pickedOption)}
            <span>ETA {eta(picked, pickedOption)}</span>
          {/if}
        </div>
      {/if}
      {#if picked.error}
        <div class="picked-error">{picked.error}</div>
      {/if}
      {#if pickedOption}
        <div class="why">
          {#each pickedOption.why as reason (reason)}
            <span class="why-chip">{reason}</span>
          {/each}
        </div>
      {/if}
    </div>
  {/if}

  <ol class="options">
    {#each visibleOptions as option (option.rank)}
      {@const tried = attemptsFor(option)}
      <li class="option" class:picked-option={option === pickedOption}>
        <button
          type="button"
          class="option-row"
          aria-expanded={openOptions.has(option.rank)}
          onclick={() => toggleOption(option.rank)}
        >
          <span class="rank">#{option.rank}</span>
          <span class="option-main">
            <span class="option-title">{option.release_title}</span>
            <span class="option-meta">
              <span>{formatBytes(option.size)}</span>
              <span>{option.source_type}</span>
              <span>{option.indexer}</span>
              {#if option.availability}
                <span>{availabilityLabel[option.availability] ?? option.availability}</span>
              {/if}
              <span>score {option.total_score.toFixed(2)}</span>
            </span>
            <span class="why">
              {#each option.why as reason (reason)}
                <span class="why-chip">{reason}</span>
              {/each}
            </span>
          </span>
          <span class="option-status">
            {#each tried as attempt (attempt.id)}
              {@const s = getStreamAttemptStatusStyle(attempt.status)}
              <span
                class="status-chip"
                style={`color: ${s.bg}; border-color: ${s.bg}`}
                title={attempt.origin === 'play' ? 'Playback' : 'Warm probe'}
              >
                {attempt.origin === 'play' ? '▶ ' : ''}{s.label}{attempt.status === 'caching'
                  ? ` ${pct(attempt.progress)}%`
                  : ''}
              </span>
            {:else}
              <span class="untried">not tried</span>
            {/each}
          </span>
        </button>
        {#if openOptions.has(option.rank) && tried.length > 0}
          <div class="option-attempts">
            {#each tried as attempt (attempt.id)}
              <StreamAttemptCard
                {attempt}
                isExpanded={expandedIds.has(attempt.id)}
                events={eventCache[attempt.id]}
                loadingEvents={loadingEvents.has(attempt.id)}
                {onToggleExpand}
              />
            {/each}
          </div>
        {/if}
      </li>
    {/each}
  </ol>
  {#if search.options.length > COLLAPSED_OPTIONS}
    <button type="button" class="btn compact alt show-all" onclick={() => (showAll = !showAll)}>
      {showAll ? 'Show fewer' : `Show all ${search.options.length} options`}
    </button>
  {/if}

  {#if unlisted.length > 0}
    <div class="unlisted">
      <h4>Other releases tried</h4>
      {#each unlisted as attempt (attempt.id)}
        <StreamAttemptCard
          {attempt}
          isExpanded={expandedIds.has(attempt.id)}
          events={eventCache[attempt.id]}
          loadingEvents={loadingEvents.has(attempt.id)}
          {onToggleExpand}
        />
      {/each}
    </div>
  {/if}
</section>

<style>
  .group-card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 12px;
    padding: 12px 14px;
    display: grid;
    gap: 10px;
  }

  .group-title h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .group-meta,
  .option-meta {
    font-size: 12px;
    color: var(--muted);
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .group-meta > :global(* + *)::before,
  .option-meta > :global(span + span)::before {
    content: '·';
    margin-right: 4px;
    opacity: 0.5;
  }

  .picked {
    border: 1px solid var(--accent);
    border-radius: 10px;
    padding: 10px 12px;
    display: grid;
    gap: 6px;
  }

  .picked-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .picked-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--accent);
  }

  .picked-title {
    flex: 1;
    min-width: 0;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .picked-progress {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--muted);
  }

  .bar {
    flex: 1;
    height: 6px;
    background: var(--ring);
    border-radius: 3px;
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s ease;
  }

  .picked-error {
    font-size: 12px;
    color: #dc3545;
    overflow-wrap: anywhere;
  }

  .options {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--ring);
    border-radius: 10px;
    overflow: hidden;
  }

  .option + .option {
    border-top: 1px solid var(--ring);
  }

  .option.picked-option {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .option-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: 100%;
    padding: 8px 12px;
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .option-row:hover {
    background: var(--bg-2);
  }

  .rank {
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    min-width: 24px;
  }

  .option-main {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  .option-title {
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .why {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .why-chip {
    font-size: 11px;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--bg);
    border: 1px solid var(--ring);
    color: var(--muted);
  }

  .option-status {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
    flex-shrink: 0;
  }

  .untried {
    font-size: 11px;
    color: var(--muted);
  }

  .option-attempts,
  .unlisted {
    border-top: 1px solid var(--ring);
  }

  .unlisted h4 {
    margin: 4px 0;
    font-size: 12px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .show-all {
    justify-self: start;
  }

  @media (max-width: 640px) {
    .option-row {
      flex-wrap: wrap;
    }

    .option-status {
      flex-direction: row;
      align-items: center;
    }
  }
</style>
