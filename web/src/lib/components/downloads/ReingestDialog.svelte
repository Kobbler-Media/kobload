<script lang="ts">
  interface Props {
    reingestId: string | null;
    reingestUrl: string;
    reingestLoading: boolean;
    reingestResult: { success: boolean; message: string } | null;
    onClose: () => void;
    onSubmit: () => void;
  }

  let {
    reingestId,
    reingestUrl = $bindable(),
    reingestLoading,
    reingestResult,
    onClose,
    onSubmit
  }: Props = $props();
</script>

{#if reingestId}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="modal-overlay" onclick={onClose} onkeydown={() => {}}>
    <div
      class="modal"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
      role="dialog"
      aria-modal="true"
      tabindex="-1"
    >
      <form
        onsubmit={(e) => {
          e.preventDefault();
          onSubmit();
        }}
      >
        <h3>Reingest Download</h3>
        <p class="muted">
          Re-trigger the callback to Kobbler. Optionally update the callback URL if the original was
          incorrect.
        </p>

        <div class="form-group">
          <label for="reingest-url">Callback URL (optional)</label>
          <input
            id="reingest-url"
            type="url"
            bind:value={reingestUrl}
            placeholder="Leave empty to use existing URL"
            disabled={reingestLoading}
          />
        </div>

        {#if reingestResult}
          <div class={`reingest-result ${reingestResult.success ? 'success' : 'error'}`}>
            {reingestResult.message}
          </div>
        {/if}

        <div class="modal-actions">
          <button class="btn alt" type="button" onclick={onClose} disabled={reingestLoading}>
            {reingestResult?.success ? 'Close' : 'Cancel'}
          </button>
          {#if !reingestResult?.success}
            <button class="btn" type="submit" disabled={reingestLoading}>
              {reingestLoading ? 'Sending...' : 'Send Callback'}
            </button>
          {/if}
        </div>
      </form>
    </div>
  </div>
{/if}

<style>
  .modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 16px;
  }

  .modal {
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    padding: 24px;
    max-width: 480px;
    width: 100%;
    max-height: 90vh;
    overflow-y: auto;
  }

  .modal h3 {
    margin: 0 0 8px;
    font-size: 18px;
  }

  .form-group {
    margin: 16px 0;
  }

  .form-group label {
    display: block;
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
    margin-bottom: 6px;
  }

  .form-group input {
    width: 100%;
    padding: 10px 12px;
    font-size: 14px;
    border: 1px solid var(--ring);
    border-radius: 8px;
    background: transparent;
    color: var(--ink);
  }

  .form-group input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .form-group input:disabled {
    opacity: 0.6;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 20px;
  }

  .reingest-result {
    padding: 12px;
    border-radius: 8px;
    font-size: 13px;
    margin-top: 12px;
  }

  .reingest-result.success {
    background: rgba(46, 160, 67, 0.15);
    border: 1px solid rgba(46, 160, 67, 0.5);
    color: #3fb950;
  }

  .reingest-result.error {
    background: rgba(220, 53, 69, 0.15);
    border: 1px solid rgba(220, 53, 69, 0.5);
    color: #dc3545;
  }
</style>
