<script lang="ts">
  import type { AliasScope, ReviewItem } from '#lib/types.js';

  let {
    item,
    suggested,
    busy = false,
    onSave,
    onCancel
  }: {
    item: ReviewItem;
    suggested: string;
    busy?: boolean;
    onSave: (scope: AliasScope, alias: string) => void;
    onCancel: () => void;
  } = $props();

  const scopeNames: Record<AliasScope, string> = {
    tv_show: 'show',
    title: 'title',
    sport_want: 'this event only',
    sport_league: 'league',
    sport_location: 'location'
  };

  let alias = $derived(suggested);
  let scope = $derived<AliasScope | ''>(item.scopes[0]?.scope ?? '');

  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (scope && alias.trim()) onSave(scope, alias.trim());
  }
</script>

<form class="remember" onsubmit={submit}>
  <input type="text" bind:value={alias} placeholder="Name to remember" aria-label="Name" />
  <select bind:value={scope} aria-label="Remember for">
    {#each item.scopes as option (option.scope)}
      <option value={option.scope}>{option.label} ({scopeNames[option.scope]})</option>
    {/each}
  </select>
  <button class="btn compact" type="submit" disabled={busy || !alias.trim() || !scope}>
    Remember
  </button>
  <button class="btn compact ghost" type="button" onclick={onCancel}>Cancel</button>
</form>

<style>
  .remember {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    margin-top: 6px;
  }

  .remember input,
  .remember select {
    padding: 4px 8px;
    font-size: 12px;
    border-radius: 6px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
  }

  .remember input {
    flex: 1;
    min-width: 140px;
  }
</style>
