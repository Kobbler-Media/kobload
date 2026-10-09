<script lang="ts">
  import { onMount } from 'svelte';
  import { listUsers, type UserSummary } from '$lib/api/client';
  import UserEditModal from '$lib/components/settings/UserEditModal.svelte';

  let loading = $state(false);
  let error = $state('');
  let users = $state<UserSummary[]>([]);

  let editorOpen = $state(false);
  let editing = $state<UserSummary | null>(null);

  const refresh = async () => {
    loading = true;
    error = '';
    try {
      users = await listUsers();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load users';
    } finally {
      loading = false;
    }
  };

  const openEditor = (user: UserSummary | null) => {
    editing = user;
    editorOpen = true;
  };

  const closeEditor = () => {
    editorOpen = false;
    editing = null;
  };

  const handleSaved = async () => {
    closeEditor();
    await refresh();
  };

  onMount(() => {
    refresh();
  });
</script>

<div class="page-header">
  <div>
    <h1>Users</h1>
    <p class="muted">Manage Kobload login accounts.</p>
  </div>
  <div class="header-actions">
    <button class="btn" type="button" onclick={() => openEditor(null)}>Add user</button>
    <button class="btn alt" type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

<div class="card-grid">
  {#if users.length === 0}
    <div class="empty-state">No users configured yet.</div>
  {:else}
    {#each users as user (user.id)}
      <article class="card">
        <div class="card-header">
          <div>
            <h3>{user.email}</h3>
            <p class="muted">Created {new Date(user.created_at).toLocaleString()}</p>
          </div>
        </div>
        <div class="actions">
          <button class="btn alt" type="button" onclick={() => openEditor(user)}> Edit </button>
        </div>
      </article>
    {/each}
  {/if}
</div>

{#if editorOpen}
  <UserEditModal user={editing} onClose={closeEditor} onSaved={handleSaved} />
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 20px;
  }

  .page-header h1 {
    margin: 0 0 6px;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
  }

  .header-actions {
    display: flex;
    gap: 8px;
    flex-shrink: 0;
    flex-wrap: wrap;
  }

  @media (max-width: 640px) {
    .page-header {
      flex-direction: column;
      align-items: stretch;
    }
  }

  .muted {
    color: var(--muted);
    font-size: 14px;
    margin: 0;
  }

  .card {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 18px;
    margin-bottom: 18px;
  }

  .card h3 {
    margin: 0 0 8px;
    font-size: 16px;
  }

  .card-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 16px;
  }

  @media (max-width: 768px) {
    .card-grid {
      grid-template-columns: 1fr;
    }
  }

  .card-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: flex-start;
  }

  .actions {
    display: flex;
    gap: 10px;
    margin-top: 16px;
    flex-wrap: wrap;
  }

  .btn {
    padding: 10px 18px;
    border-radius: 10px;
    border: 1px solid var(--accent);
    background: var(--accent);
    color: #1a1b14;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn.alt {
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
  }

  .error {
    padding: 12px;
    border-radius: 12px;
    margin-bottom: 16px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .empty-state {
    padding: 24px;
    text-align: center;
    border: 1px dashed var(--ring);
    border-radius: 12px;
    color: var(--muted);
    grid-column: 1 / -1;
  }
</style>
