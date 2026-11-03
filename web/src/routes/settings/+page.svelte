<script lang="ts">
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';

  // Redirect to prowlarr by default
  onMount(() => {
    goto('/settings/prowlarr', { replace: true });
  });
</script>

<div class="loading">Loading settings...</div>

<style>
  .loading {
    color: var(--muted);
    font-size: 14px;
    padding: 24px;
    text-align: center;
  }
</style>
