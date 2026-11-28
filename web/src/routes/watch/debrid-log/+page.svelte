<script lang="ts">
  import { ScrollToTop } from '@koblibs/ui';
  import LogToggle from '#lib/components/decision-log/LogToggle.svelte';
  import StreamGroups from '#lib/components/debrid-log/StreamGroups.svelte';
</script>

<div class="page-header">
  <div>
    <h1>Debrid Log</h1>
    <p class="muted">
      Each stream search with its ranked options, why each ranked where it did, and what was tried:
      warm probes, the copy picked for playback and its progress. Kept for a day.
    </p>
  </div>
  <div class="header-actions">
    <LogToggle type="debrid" />
  </div>
</div>

<StreamGroups />

<ScrollToTop />

<style>
  .muted {
    color: var(--muted);
    font-size: 13px;
    margin: 0;
  }
</style>
