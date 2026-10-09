<script lang="ts">
  import type { DebridTestResult } from '$lib/api/client';

  let { result }: { result: DebridTestResult } = $props();

  const formatExpiry = (expiration: string | null): string => {
    if (!expiration) return 'Unknown';
    try {
      return new Date(expiration).toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric'
      });
    } catch {
      return expiration;
    }
  };
</script>

<div class="test-result" class:success={result.success}>
  <div class="test-message">{result.message}</div>
  {#if result.account}
    <div class="account-info">
      {#if result.account.username}
        <span class="account-field"><strong>User:</strong> {result.account.username}</span>
      {/if}
      <span class="account-field">
        <strong>Premium:</strong>
        {result.account.premium ? 'Yes' : 'No'}
      </span>
      {#if result.account.expiration}
        <span class="account-field">
          <strong>Expires:</strong>
          {formatExpiry(result.account.expiration)}
        </span>
      {/if}
      {#if result.account.remaining_traffic}
        <span class="account-field">
          <strong>Traffic:</strong>
          {result.account.remaining_traffic}
        </span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .test-result {
    margin-top: 12px;
    padding: 10px 14px;
    border-radius: 10px;
    font-size: 13px;
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.3);
    color: #dc3545;
  }

  .test-result.success {
    background: rgba(40, 167, 69, 0.15);
    border: 1px solid rgba(40, 167, 69, 0.3);
    color: #28a745;
  }

  .test-message {
    font-weight: 500;
  }

  .account-info {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin-top: 8px;
    font-size: 12px;
  }

  .account-field strong {
    opacity: 0.8;
  }
</style>
