<script lang="ts">
  import type { MatchExplanation } from '$lib/types';
  import { learnedIn } from '$lib/learnedAliases';

  let { explanation, learned = [] }: { explanation: MatchExplanation; learned?: string[] } =
    $props();
</script>

<div class="detail-section">
  <h4 class="section-title">Match Explanation</h4>
  <div class="kv-grid">
    {#each explanation.matched as hit, i (i)}
      <span class="kv-key">{hit.slot}</span>
      <span class="kv-val">
        {hit.value}{#if hit.alias && hit.alias !== hit.value}
          <span class="alias">on "{hit.alias}"</span>{/if}{#if hit.weak}
          <span class="alias">(weak)</span>{/if}{#if learnedIn(hit.alias ?? hit.value, learned)}
          <span class="learned">learned</span>{/if}
      </span>
    {/each}
    {#if explanation.unexplained.length > 0}
      <span class="kv-key">unexplained</span>
      <span class="kv-val">{explanation.unexplained.join(' ')}</span>
    {/if}
  </div>
  {#each explanation.contradictions as c, i (i)}
    <div class="rejection-reason">other {c.slot}: {c.value}</div>
  {/each}
</div>

<style>
  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .section-title {
    margin: 0;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--muted);
    font-weight: 600;
  }

  .kv-grid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 12px;
    font-size: 12px;
  }

  .kv-key {
    color: var(--muted);
    text-transform: capitalize;
    white-space: nowrap;
  }

  .kv-val {
    color: var(--ink);
    word-break: break-word;
  }

  .alias {
    color: var(--muted);
  }

  .learned {
    margin-left: 4px;
    padding: 0 6px;
    border-radius: 8px;
    font-size: 10px;
    background: rgba(59, 130, 246, 0.15);
    color: #3b82f6;
  }

  .rejection-reason {
    padding: 4px 8px;
    background: rgba(239, 68, 68, 0.08);
    border-radius: 4px;
    font-size: 11px;
    color: #ef4444;
  }
</style>
