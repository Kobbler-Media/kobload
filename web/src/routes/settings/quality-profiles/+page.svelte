<script lang="ts">
  import { onMount } from 'svelte';
  import {
    listQualityProfiles,
    createQualityProfile,
    updateQualityProfile,
    deleteQualityProfile,
    resetQualityProfiles
  } from '$lib/api/client';
  import type { QualityProfile } from '$lib/types';
  import {
    MultiSearchSelect,
    type MultiSearchSelectItem,
    SearchSelect,
    type SearchSelectItem
  } from '@koblibs/ui';

  let profiles = $state<QualityProfile[]>([]);
  let loading = $state(false);
  let error = $state('');

  // Form state
  let showForm = $state(false);
  let formName = $state('');
  let formMediaKind = $state('tv');
  let formIsDefault = $state(false);
  let saving = $state(false);

  // Edit state
  let editingId = $state<string | null>(null);
  let editForm = $state<Partial<QualityProfile>>({});
  let editSaving = $state(false);
  let resetting = $state(false);

  // Known option sets
  const SOURCE_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'bluray', label: 'bluray', description: 'Blu-ray disc source' },
    { value: 'web-dl', label: 'web-dl', description: 'Web download (lossless)' },
    { value: 'webrip', label: 'webrip', description: 'Web rip (re-encoded)' },
    { value: 'hdtv', label: 'hdtv', description: 'HDTV broadcast capture' },
    { value: 'dvd', label: 'dvd', description: 'DVD source' }
  ];

  const CODEC_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'x265', label: 'x265', description: 'HEVC / H.265' },
    { value: 'x264', label: 'x264', description: 'AVC / H.264' },
    { value: 'av1', label: 'av1', description: 'AV1 codec' },
    { value: 'xvid', label: 'xvid', description: 'XviD / MPEG-4' }
  ];

  const AUDIO_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'truehd', label: 'truehd', description: 'Dolby TrueHD' },
    { value: 'atmos', label: 'atmos', description: 'Dolby Atmos' },
    { value: 'dts', label: 'dts', description: 'DTS / DTS-HD' },
    { value: 'ddp', label: 'ddp', description: 'Dolby Digital Plus' },
    { value: 'dd', label: 'dd', description: 'Dolby Digital' },
    { value: 'aac', label: 'aac', description: 'Advanced Audio Coding' },
    { value: 'flac', label: 'flac', description: 'Free Lossless Audio' },
    { value: 'mp3', label: 'mp3', description: 'MP3' }
  ];

  const REJECTED_SOURCE_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'cam', label: 'cam', description: 'Camera recording' },
    { value: 'ts', label: 'ts', description: 'Telesync' },
    { value: 'telecine', label: 'telecine', description: 'Telecine transfer' },
    { value: 'hdtv', label: 'hdtv', description: 'HDTV broadcast capture' },
    { value: 'dvd', label: 'dvd', description: 'DVD source' }
  ];

  const REJECTED_TERM_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'ASL', label: 'ASL', description: 'American Sign Language overlay' },
    { value: 'BSL', label: 'BSL', description: 'British Sign Language overlay' },
    { value: '.Signed.', label: '.Signed.', description: 'Signed broadcast version' },
    { value: 'Sign.Zone', label: 'Sign.Zone', description: 'BBC Sign Zone' },
    { value: 'SignZone', label: 'SignZone', description: 'BBC Sign Zone (no dot)' },
    { value: '3D', label: '3D', description: '3D stereoscopic release' },
    { value: 'HSBS', label: 'HSBS', description: 'Half Side-by-Side 3D' },
    { value: 'Half-SBS', label: 'Half-SBS', description: 'Half Side-by-Side 3D' },
    { value: 'HOU', label: 'HOU', description: 'Half Over-Under 3D' },
    { value: 'Half-OU', label: 'Half-OU', description: 'Half Over-Under 3D' }
  ];

  const LANGUAGE_OPTIONS: MultiSearchSelectItem[] = [
    { value: 'english', label: 'english' },
    { value: 'multi', label: 'multi', description: 'Multiple languages' },
    { value: 'french', label: 'french' },
    { value: 'german', label: 'german' },
    { value: 'spanish', label: 'spanish' },
    { value: 'italian', label: 'italian' },
    { value: 'russian', label: 'russian' },
    { value: 'korean', label: 'korean' },
    { value: 'japanese', label: 'japanese' },
    { value: 'chinese', label: 'chinese' },
    { value: 'hindi', label: 'hindi' },
    { value: 'arabic', label: 'arabic' },
    { value: 'portuguese', label: 'portuguese' },
    { value: 'dutch', label: 'dutch' },
    { value: 'swedish', label: 'swedish' },
    { value: 'danish', label: 'danish' },
    { value: 'norwegian', label: 'norwegian' },
    { value: 'finnish', label: 'finnish' },
    { value: 'polish', label: 'polish' },
    { value: 'czech', label: 'czech' },
    { value: 'hungarian', label: 'hungarian' },
    { value: 'turkish', label: 'turkish' }
  ];

  const RESOLUTION_OPTIONS = ['', '480p', '720p', '1080p', '2160p'];

  const mediaKindItems: SearchSelectItem[] = [
    { value: 'tv', label: 'TV' },
    { value: 'movie', label: 'Movie' }
  ];

  const resolutionItems: SearchSelectItem[] = RESOLUTION_OPTIONS.map((res) => ({
    value: res,
    label: res || '(none)'
  }));

  // Group profiles by media_kind
  const MEDIA_KIND_LABELS: Record<string, string> = {
    tv: 'TV',
    movie: 'Movies'
  };
  const MEDIA_KIND_ORDER = ['tv', 'movie'];

  let groupedProfiles = $derived(() => {
    const groups: { kind: string; label: string; profiles: QualityProfile[] }[] = [];
    for (const kind of MEDIA_KIND_ORDER) {
      const matching = profiles.filter((p) => p.media_kind === kind);
      if (matching.length > 0) {
        groups.push({ kind, label: MEDIA_KIND_LABELS[kind] || kind, profiles: matching });
      }
    }
    // Any remaining kinds (including book if user manually created one)
    const knownKinds = new Set(MEDIA_KIND_ORDER);
    const other = profiles.filter((p) => !knownKinds.has(p.media_kind));
    if (other.length > 0) {
      groups.push({ kind: 'other', label: 'Other', profiles: other });
    }
    return groups;
  });

  async function loadProfiles() {
    loading = true;
    error = '';
    try {
      profiles = await listQualityProfiles();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load quality profiles';
    } finally {
      loading = false;
    }
  }

  async function handleCreate() {
    if (!formName.trim()) return;
    saving = true;
    try {
      await createQualityProfile({
        name: formName.trim(),
        media_kind: formMediaKind,
        is_default: formIsDefault
      });
      formName = '';
      formIsDefault = false;
      showForm = false;
      await loadProfiles();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to create profile';
    } finally {
      saving = false;
    }
  }

  function startEdit(profile: QualityProfile) {
    editingId = profile.id;
    editForm = {
      name: profile.name,
      min_resolution: profile.min_resolution,
      max_resolution: profile.max_resolution,
      preferred_sources: [...profile.preferred_sources],
      preferred_codecs: [...profile.preferred_codecs],
      preferred_audio: [...profile.preferred_audio],
      preferred_languages: [...profile.preferred_languages],
      rejected_sources: [...profile.rejected_sources],
      rejected_languages: [...profile.rejected_languages],
      rejected_terms: [...profile.rejected_terms],
      reject_hardcoded_subs: profile.reject_hardcoded_subs,
      use_trash_filters: profile.use_trash_filters,
      reject_remux: profile.reject_remux,
      upgrade_cutoff_score: profile.upgrade_cutoff_score,
      is_default: profile.is_default
    };
  }

  async function handleSaveEdit() {
    if (!editingId) return;
    editSaving = true;
    try {
      await updateQualityProfile(editingId, editForm);
      editingId = null;
      await loadProfiles();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to update profile';
    } finally {
      editSaving = false;
    }
  }

  async function handleReset() {
    if (!confirm('This will delete all quality profiles and recreate the defaults. Continue?'))
      return;
    resetting = true;
    error = '';
    try {
      profiles = await resetQualityProfiles();
      editingId = null;
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to reset profiles';
    } finally {
      resetting = false;
    }
  }

  async function handleDelete(id: string) {
    if (!confirm('Delete this quality profile?')) return;
    try {
      await deleteQualityProfile(id);
      await loadProfiles();
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to delete profile';
    }
  }

  onMount(loadProfiles);
</script>

<div class="page-header">
  <div>
    <h1>Quality Profiles</h1>
    <p class="muted">Configure quality preferences for different media types.</p>
  </div>
  <div class="header-actions">
    <button class="btn secondary" type="button" onclick={handleReset} disabled={resetting}>
      {resetting ? 'Resetting...' : 'Reset to Defaults'}
    </button>
    <button class="btn" type="button" onclick={() => (showForm = !showForm)}>
      {showForm ? 'Cancel' : 'Add Profile'}
    </button>
  </div>
</div>

{#if error}
  <div class="error">{error}</div>
{/if}

{#if showForm}
  <section class="card">
    <h2>New Quality Profile</h2>
    <form
      onsubmit={(e) => {
        e.preventDefault();
        handleCreate();
      }}
    >
      <div class="form-grid">
        <label>
          Name
          <input type="text" bind:value={formName} placeholder="e.g. TV - 1080p Custom" />
        </label>
        <label>
          Media Kind
          <SearchSelect
            id="form-media-kind"
            items={mediaKindItems}
            value={formMediaKind}
            onchange={(v) => {
              formMediaKind = v;
            }}
            placeholder="Select..."
          />
        </label>
      </div>
      <label class="checkbox-label">
        <input type="checkbox" bind:checked={formIsDefault} />
        Default profile for this media kind
      </label>
      <div class="actions">
        <button class="btn" type="submit" disabled={saving || !formName.trim()}>
          {saving ? 'Creating...' : 'Create'}
        </button>
      </div>
    </form>
  </section>
{/if}

{#if loading}
  <div class="muted">Loading...</div>
{:else if profiles.length === 0}
  <div class="empty">No quality profiles configured yet.</div>
{:else}
  {#each groupedProfiles() as group (group.kind)}
    <div class="group">
      <h2 class="group-title">{group.label}</h2>
      <div class="profiles-list">
        {#each group.profiles as profile (profile.id)}
          <section class="card profile-card">
            {#if editingId === profile.id}
              <form
                onsubmit={(e) => {
                  e.preventDefault();
                  handleSaveEdit();
                }}
              >
                <div class="form-grid">
                  <label>
                    Name
                    <input type="text" bind:value={editForm.name} />
                  </label>
                  <label>
                    Min Resolution
                    <SearchSelect
                      id="edit-min-resolution"
                      items={resolutionItems}
                      value={editForm.min_resolution || ''}
                      onchange={(v) => {
                        editForm.min_resolution = v || null;
                      }}
                      placeholder="(none)"
                    />
                  </label>
                  <label>
                    Max Resolution
                    <SearchSelect
                      id="edit-max-resolution"
                      items={resolutionItems}
                      value={editForm.max_resolution || ''}
                      onchange={(v) => {
                        editForm.max_resolution = v || null;
                      }}
                      placeholder="(none)"
                    />
                  </label>
                </div>
                <div class="form-stack">
                  <label>
                    Preferred Sources
                    <MultiSearchSelect
                      items={SOURCE_OPTIONS}
                      bind:values={editForm.preferred_sources}
                      allowCreate={true}
                      placeholder="Select or type sources..."
                    />
                  </label>
                  <label>
                    Preferred Codecs
                    <MultiSearchSelect
                      items={CODEC_OPTIONS}
                      bind:values={editForm.preferred_codecs}
                      allowCreate={true}
                      placeholder="Select or type codecs..."
                    />
                  </label>
                  <label>
                    Preferred Audio
                    <MultiSearchSelect
                      items={AUDIO_OPTIONS}
                      bind:values={editForm.preferred_audio}
                      allowCreate={true}
                      placeholder="Select or type audio formats..."
                    />
                  </label>
                  <label>
                    Preferred Languages
                    <MultiSearchSelect
                      items={LANGUAGE_OPTIONS}
                      bind:values={editForm.preferred_languages}
                      allowCreate={true}
                      placeholder="Select or type languages..."
                    />
                  </label>
                  <label>
                    Rejected Sources
                    <MultiSearchSelect
                      items={REJECTED_SOURCE_OPTIONS}
                      bind:values={editForm.rejected_sources}
                      allowCreate={true}
                      placeholder="Select or type rejected sources..."
                    />
                  </label>
                  <label>
                    Rejected Languages
                    <MultiSearchSelect
                      items={LANGUAGE_OPTIONS}
                      bind:values={editForm.rejected_languages}
                      allowCreate={true}
                      placeholder="Select or type rejected languages..."
                    />
                  </label>
                  <label>
                    Rejected Terms
                    <MultiSearchSelect
                      items={REJECTED_TERM_OPTIONS}
                      bind:values={editForm.rejected_terms}
                      allowCreate={true}
                      placeholder="Select or type terms to reject in release names..."
                    />
                  </label>
                  <label>
                    Upgrade Cutoff Score
                    <input
                      type="number"
                      value={editForm.upgrade_cutoff_score ?? ''}
                      onchange={(e) => {
                        const v = (e.target as HTMLInputElement).value;
                        editForm.upgrade_cutoff_score = v ? Number(v) : null;
                      }}
                      min="0"
                      max="2"
                      step="0.05"
                      placeholder="No cutoff"
                    />
                    <span class="helper-text"
                      >Once the held release's quality score reaches this, only PROPER/REPACK
                      releases upgrade. Raw score scale (~0&ndash;1.3): a complete 1080p WEB-DL
                      &asymp; 1.05, 4K/HDR higher. Try ~1.05 for a 1080p profile. Leave blank to
                      always allow upgrades.</span
                    >
                  </label>
                  <label class="checkbox-label">
                    <input type="checkbox" bind:checked={editForm.reject_hardcoded_subs} />
                    Reject hardcoded subs
                  </label>
                  <label class="checkbox-label">
                    <input type="checkbox" bind:checked={editForm.reject_remux} />
                    Reject remux releases
                  </label>
                  <label class="checkbox-label">
                    <input type="checkbox" bind:checked={editForm.use_trash_filters} />
                    Apply TRaSH Guides filters
                    <span class="helper-text"
                      >Auto-reject known low-quality groups, upscaled releases, and BR-DISK. Boost
                      scores for known-good release groups.</span
                    >
                  </label>
                  <label class="checkbox-label">
                    <input type="checkbox" bind:checked={editForm.is_default} />
                    Default profile for this media kind
                  </label>
                </div>
                <div class="actions">
                  <button class="btn" type="submit" disabled={editSaving}>
                    {editSaving ? 'Saving...' : 'Save'}
                  </button>
                  <button class="btn secondary" type="button" onclick={() => (editingId = null)}>
                    Cancel
                  </button>
                </div>
              </form>
            {:else}
              <div class="profile-header">
                <div>
                  <h3>{profile.name}</h3>
                  <span class="badge">{profile.media_kind}</span>
                  {#if profile.is_default}
                    <span class="badge default">default</span>
                  {/if}
                </div>
                <div class="profile-actions">
                  <button class="btn secondary" type="button" onclick={() => startEdit(profile)}>
                    Edit
                  </button>
                  <button class="btn danger" type="button" onclick={() => handleDelete(profile.id)}>
                    Delete
                  </button>
                </div>
              </div>
              <div class="profile-details">
                {#if profile.min_resolution || profile.max_resolution}
                  <div class="detail">
                    <span class="label">Resolution:</span>
                    {profile.min_resolution || 'any'} – {profile.max_resolution || 'any'}
                  </div>
                {/if}
                {#if profile.preferred_sources.length > 0}
                  <div class="detail">
                    <span class="label">Sources:</span>
                    {profile.preferred_sources.join(', ')}
                  </div>
                {/if}
                {#if profile.preferred_codecs.length > 0}
                  <div class="detail">
                    <span class="label">Codecs:</span>
                    {profile.preferred_codecs.join(', ')}
                  </div>
                {/if}
                {#if profile.preferred_audio.length > 0}
                  <div class="detail">
                    <span class="label">Audio:</span>
                    {profile.preferred_audio.join(', ')}
                  </div>
                {/if}
                {#if profile.preferred_languages.length > 0}
                  <div class="detail">
                    <span class="label">Languages:</span>
                    {profile.preferred_languages.join(', ')}
                  </div>
                {/if}
                {#if profile.rejected_sources.length > 0}
                  <div class="detail">
                    <span class="label">Rejected sources:</span>
                    {profile.rejected_sources.join(', ')}
                  </div>
                {/if}
                {#if profile.rejected_languages.length > 0}
                  <div class="detail">
                    <span class="label">Rejected languages:</span>
                    {profile.rejected_languages.join(', ')}
                  </div>
                {/if}
                {#if profile.rejected_terms.length > 0}
                  <div class="detail">
                    <span class="label">Rejected terms:</span>
                    {profile.rejected_terms.join(', ')}
                  </div>
                {/if}
                {#if profile.upgrade_cutoff_score !== null}
                  <div class="detail">
                    <span class="label">Upgrade Cutoff:</span>
                    {profile.upgrade_cutoff_score}
                  </div>
                {/if}
                {#if profile.reject_hardcoded_subs}
                  <div class="detail">
                    <span class="label">HC subs:</span> rejected
                  </div>
                {/if}
                {#if profile.reject_remux}
                  <div class="detail">
                    <span class="label">Remux:</span> rejected
                  </div>
                {/if}
                {#if profile.use_trash_filters}
                  <div class="detail">
                    <span class="label">TRaSH filters:</span> enabled
                  </div>
                {/if}
              </div>
            {/if}
          </section>
        {/each}
      </div>
    </div>
  {/each}
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 20px;
  }

  .header-actions {
    display: flex;
    gap: 8px;
  }

  .page-header h1 {
    margin: 0 0 6px;
    font-size: 24px;
    font-family: 'Palatino Linotype', 'Book Antiqua', Palatino, serif;
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
    margin-bottom: 12px;
  }

  .card h2,
  .card h3 {
    margin: 0;
    font-size: 16px;
  }

  .group {
    margin-bottom: 24px;
  }

  .group-title {
    font-size: 14px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    margin: 0 0 10px 4px;
  }

  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 16px;
    margin: 16px 0;
  }

  .form-stack {
    display: flex;
    flex-direction: column;
    gap: 16px;
    margin: 16px 0;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
  }

  .checkbox-label {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--ink);
  }

  .checkbox-label input[type='checkbox'] {
    width: auto;
  }

  input {
    padding: 10px 14px;
    border-radius: 10px;
    border: 1px solid var(--ring);
    background: var(--bg);
    color: var(--ink);
    font-size: 13px;
  }

  .actions {
    display: flex;
    gap: 10px;
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

  .btn.secondary {
    background: transparent;
    border-color: var(--ring);
    color: var(--ink);
    font-size: 12px;
    padding: 6px 12px;
  }

  .btn.danger {
    background: transparent;
    border-color: rgba(220, 53, 69, 0.5);
    color: #dc3545;
    font-size: 12px;
    padding: 6px 12px;
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

  .empty {
    text-align: center;
    padding: 32px;
    color: var(--muted);
    font-size: 14px;
  }

  .profile-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }

  .profile-actions {
    display: flex;
    gap: 8px;
  }

  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 500;
    background: rgba(100, 149, 237, 0.2);
    color: cornflowerblue;
    margin-top: 4px;
  }

  .badge.default {
    background: rgba(40, 167, 69, 0.2);
    color: #28a745;
  }

  .profile-details {
    margin-top: 8px;
  }

  .detail {
    margin-top: 4px;
    font-size: 12px;
    color: var(--muted);
  }

  .detail .label {
    font-weight: 500;
    color: var(--ink);
  }

  .helper-text {
    font-size: 11px;
    color: var(--muted);
    font-style: italic;
  }

  .profiles-list {
    display: flex;
    flex-direction: column;
  }

  @media (max-width: 768px) {
    .profile-header {
      flex-direction: column;
      align-items: flex-start;
      gap: 8px;
    }

    .form-grid {
      grid-template-columns: 1fr;
    }

    .header-actions {
      flex-wrap: wrap;
    }
  }
</style>
