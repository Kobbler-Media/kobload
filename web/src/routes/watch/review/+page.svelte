<script lang="ts">
  import { onMount } from 'svelte';
  import {
    listReviewItems,
    grabReviewRelease,
    rejectReviewRelease,
    rememberReviewName,
    dismissReviewItem,
    listLearnedAliases,
    deleteLearnedAlias
  } from '#lib/api/client.js';
  import type { AliasScope, LearnedAlias, ReviewItem } from '#lib/types.js';
  import { learnedNames } from '#lib/learnedAliases.js';
  import { showToast } from '#lib/stores/toast.svelte.js';
  import { refreshReviewCount } from '#lib/stores/review.svelte.js';
  import ReviewItemCard from '#lib/components/review/ReviewItemCard.svelte';
  import LearnedAliasList from '#lib/components/review/LearnedAliasList.svelte';

  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let items = $state<ReviewItem[]>([]);
  let aliases = $state<LearnedAlias[]>([]);

  const learned = $derived(learnedNames(aliases));

  async function load() {
    try {
      const [review, learnedAliases] = await Promise.all([listReviewItems(), listLearnedAliases()]);
      items = review.items;
      aliases = learnedAliases;
      error = '';
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load the review queue';
    } finally {
      loading = false;
    }
    await refreshReviewCount();
  }

  async function act(action: () => Promise<unknown>, done: string) {
    busy = true;
    try {
      await action();
      showToast(done);
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Action failed');
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<div class="page-header">
  <div>
    <h1>Review</h1>
    <p class="muted">
      Wants that got nothing past their date, and grabs that may be the wrong release.
    </p>
  </div>
</div>

{#if error}
  <div class="error-banner">{error}</div>
{/if}

{#if loading}
  <div class="loading-state">Loading…</div>
{:else}
  <section class="items">
    {#if items.length === 0}
      <p class="muted">Nothing to review.</p>
    {/if}
    {#each items as item (item.id)}
      <ReviewItemCard
        {item}
        {learned}
        {busy}
        onGrab={(key) => act(() => grabReviewRelease(item.id, key), 'Grabbed')}
        onNotIt={(key) => act(() => rejectReviewRelease(item.id, key), 'Noted: not this want')}
        onRemember={(scope: AliasScope, alias: string) =>
          act(() => rememberReviewName(item.id, scope, alias), `Remembered "${alias}"`)}
        onDismiss={() => act(() => dismissReviewItem(item.id), 'Dismissed')}
      />
    {/each}
  </section>

  <section class="learned">
    <h2>Remembered names</h2>
    <LearnedAliasList
      {aliases}
      {busy}
      onDelete={(alias) => act(() => deleteLearnedAlias(alias.id), `Forgot "${alias.alias}"`)}
    />
  </section>
{/if}

<style>
  .muted {
    color: var(--muted);
    margin: 0;
  }

  .items {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-bottom: 28px;
  }

  .learned h2 {
    font-size: 16px;
    margin: 0 0 8px;
  }
</style>
