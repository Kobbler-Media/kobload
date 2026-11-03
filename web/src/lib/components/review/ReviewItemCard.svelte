<script lang="ts">
  import type { AliasScope, MatchExplanation, ReviewItem } from '#lib/types.js';
  import MatchExplanationView from '#lib/components/decision-log/MatchExplanationView.svelte';
  import RememberNameForm from './RememberNameForm.svelte';

  let {
    item,
    learned,
    busy,
    onGrab,
    onNotIt,
    onRemember,
    onDismiss
  }: {
    item: ReviewItem;
    learned: string[];
    busy: boolean;
    onGrab: (releaseKey: string) => void;
    onNotIt: (releaseKey: string) => void;
    onRemember: (scope: AliasScope, alias: string) => void;
    onDismiss: () => void;
  } = $props();

  type Release = {
    release_key: string;
    title: string;
    note: string;
    explanation: MatchExplanation | null;
    suggested_alias: string | null;
    grabbable: boolean;
  };

  const releases = $derived<Release[]>(
    item.kind === 'missed'
      ? item.detail.candidates.map((c) => ({
          release_key: c.release_key,
          title: c.title,
          note: [
            c.indexer,
            c.size ? formatBytes(c.size) : null,
            `${Math.round(c.similarity * 100)}% alike`
          ]
            .filter(Boolean)
            .join(' · '),
          explanation: c.explanation,
          suggested_alias: c.suggested_alias,
          grabbable: true
        }))
      : item.detail.grab
        ? [
            {
              release_key: item.detail.grab.release_key,
              title: item.detail.grab.title,
              note: grabNote(item),
              explanation: item.detail.grab.explanation,
              suggested_alias: item.detail.grab.suggested_alias,
              grabbable: false
            }
          ]
        : []
  );

  let remembering = $state<string | null>(null);
  let open = $state<string | null>(null);

  function grabNote(review: ReviewItem): string {
    const grab = review.detail.grab;
    if (!grab) return '';
    const parts = [`grabbed at ${Math.round(grab.match_score * 100)}%`];
    if (grab.runner_up) {
      parts.push(
        `${grab.runner_up.want} would take it at ${Math.round(grab.runner_up.match_score * 100)}%`
      );
    }
    if (grab.unexplained.length > 0) parts.push(`unplaced: ${grab.unexplained.join(' ')}`);
    return parts.join(' · ');
  }

  function formatBytes(bytes: number): string {
    const gib = bytes / 1024 ** 3;
    return gib >= 1 ? `${gib.toFixed(2)} GB` : `${(bytes / 1024 ** 2).toFixed(0)} MB`;
  }

  function formatDate(iso: string | null): string {
    return iso ? new Date(iso).toLocaleString() : '';
  }
</script>

<article class="card review-item">
  <header>
    <span class="kind" class:doubtful={item.kind === 'doubtful'}>
      {item.kind === 'missed' ? 'Missed' : 'Doubtful'}
    </span>
    <h2>{item.detail.want}</h2>
    <span class="muted">
      {item.detail.media_kind}{#if item.detail.due_at}
        · due {formatDate(item.detail.due_at)}{/if}
    </span>
    <button class="btn compact ghost dismiss" type="button" disabled={busy} onclick={onDismiss}>
      Dismiss
    </button>
  </header>

  <ul class="releases">
    {#each releases as release (release.release_key)}
      <li>
        <button
          class="title"
          type="button"
          onclick={() => (open = open === release.release_key ? null : release.release_key)}
        >
          {release.title}
        </button>
        <div class="muted note">{release.note}</div>
        {#if open === release.release_key && release.explanation}
          <MatchExplanationView explanation={release.explanation} {learned} />
        {/if}
        <div class="actions">
          {#if release.grabbable}
            <button
              class="btn compact"
              type="button"
              disabled={busy}
              onclick={() => onGrab(release.release_key)}>Grab</button
            >
          {/if}
          <button
            class="btn compact danger"
            type="button"
            disabled={busy}
            onclick={() => onNotIt(release.release_key)}>Not it</button
          >
          <button
            class="btn compact alt"
            type="button"
            disabled={busy || item.scopes.length === 0}
            onclick={() => (remembering = release.release_key)}>Remember this name</button
          >
        </div>
        {#if remembering === release.release_key}
          <RememberNameForm
            {item}
            {busy}
            suggested={release.suggested_alias ?? ''}
            onSave={(scope, alias) => {
              remembering = null;
              onRemember(scope, alias);
            }}
            onCancel={() => (remembering = null)}
          />
        {/if}
      </li>
    {/each}
  </ul>
</article>

<style>
  .review-item {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
  }

  header h2 {
    margin: 0;
    font-size: 15px;
  }

  .dismiss {
    margin-left: auto;
  }

  .kind {
    padding: 1px 8px;
    border-radius: 8px;
    font-size: 11px;
    background: rgba(234, 179, 8, 0.15);
    color: #eab308;
  }

  .kind.doubtful {
    background: rgba(239, 68, 68, 0.15);
    color: #ef4444;
  }

  .muted {
    color: var(--muted);
    font-size: 12px;
  }

  .releases {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .title {
    padding: 0;
    border: none;
    background: none;
    color: var(--ink);
    font-family: monospace;
    font-size: 12px;
    text-align: left;
    word-break: break-all;
    cursor: pointer;
  }

  .note {
    margin: 2px 0 4px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 4px;
  }
</style>
