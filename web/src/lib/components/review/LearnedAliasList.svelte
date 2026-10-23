<script lang="ts">
  import type { AliasScope, LearnedAlias } from '$lib/types';

  let {
    aliases,
    busy,
    onDelete
  }: { aliases: LearnedAlias[]; busy: boolean; onDelete: (alias: LearnedAlias) => void } = $props();

  const scopeNames: Record<AliasScope, string> = {
    tv_show: 'Show',
    title: 'Title',
    sport_want: 'Event',
    sport_league: 'League',
    sport_location: 'Location'
  };
</script>

{#if aliases.length === 0}
  <p class="muted">No names remembered yet.</p>
{:else}
  <table>
    <thead>
      <tr><th>Name</th><th>For</th><th>Scope</th><th></th></tr>
    </thead>
    <tbody>
      {#each aliases as alias (alias.id)}
        <tr>
          <td><code>{alias.alias}</code></td>
          <td>{alias.scope_label}</td>
          <td class="muted">{scopeNames[alias.scope]}</td>
          <td>
            <button
              class="btn compact danger"
              type="button"
              disabled={busy}
              onclick={() => onDelete(alias)}>Delete</button
            >
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }

  th,
  td {
    padding: 6px 8px;
    text-align: left;
    border-bottom: 1px solid var(--ring);
  }

  th {
    color: var(--muted);
    font-weight: 500;
    font-size: 11px;
    text-transform: uppercase;
  }

  .muted {
    color: var(--muted);
  }
</style>
