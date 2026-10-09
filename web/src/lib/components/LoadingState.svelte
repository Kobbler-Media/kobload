<script lang="ts">
  let { message = 'Loading...' }: { message?: string } = $props();
</script>

<div class="loading-state">
  <svg class="spinner" viewBox="0 0 24 24" width="24" height="24">
    <circle
      cx="12"
      cy="12"
      r="10"
      fill="none"
      stroke="currentColor"
      stroke-width="2.5"
      stroke-dasharray="31.4 31.4"
      stroke-linecap="round"
    ></circle>
  </svg>
  <span>{message}</span>
</div>

<style>
  .loading-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 32px 24px;
    color: var(--muted);
    font-size: 13px;
  }

  .spinner {
    animation: spin 0.8s linear infinite;
    opacity: 0.6;
  }

  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }
</style>
