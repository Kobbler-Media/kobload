<script lang="ts">
  import { untrack } from 'svelte';
  import { Modal } from '@koblibs/ui';
  import { createUser, updateUser, type UserSummary } from '$lib/api/client';
  import { discardGuard } from '$lib/discardGuard';

  let {
    user,
    onClose,
    onSaved
  }: {
    user: UserSummary | null;
    onClose: () => void;
    onSaved: () => void;
  } = $props();

  const formId = 'user-edit-form';

  let form = $state(untrack(() => ({ email: user?.email ?? '', password: '' })));
  let saving = $state(false);
  let formError = $state('');

  const canClose = discardGuard(form);
  const cancel = () => {
    if (canClose()) onClose();
  };

  const save = async () => {
    if (!form.email.trim()) {
      formError = 'Email is required.';
      return;
    }
    if (!user && !form.password.trim()) {
      formError = 'Password is required for new users.';
      return;
    }
    saving = true;
    formError = '';
    try {
      if (user) {
        await updateUser(user.id, {
          email: form.email.trim(),
          password: form.password.trim() || undefined
        });
      } else {
        await createUser({ email: form.email.trim(), password: form.password.trim() });
      }
      onSaved();
    } catch (err) {
      formError = err instanceof Error ? err.message : 'Failed to save user';
    } finally {
      saving = false;
    }
  };
</script>

<Modal title={user ? `Edit ${user.email}` : 'Add User'} sheet {onClose} {canClose}>
  <form
    id={formId}
    class="modal-form"
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    {#if formError}
      <div class="error">{formError}</div>
    {/if}

    <div class="form-grid cols-1">
      <label>
        Email
        <input type="email" bind:value={form.email} placeholder="user@example.com" />
      </label>
      <label>
        Password
        <input
          type="password"
          bind:value={form.password}
          placeholder={user ? 'Leave blank to keep current password' : 'Set a password'}
          autocomplete="new-password"
        />
      </label>
    </div>
  </form>

  {#snippet footer()}
    <button class="btn compact alt" type="button" onclick={cancel} disabled={saving}>Cancel</button>
    <button class="btn compact" type="submit" form={formId} disabled={saving}>
      {user ? 'Update user' : 'Add user'}
    </button>
  {/snippet}
</Modal>
