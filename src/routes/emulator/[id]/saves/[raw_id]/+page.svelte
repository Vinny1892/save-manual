<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { invoke } from "@tauri-apps/api/core";
  import { emulators } from "$lib/store";
  import { derived } from "svelte/store";

  interface SaveEntry {
    raw_id: string;
    title: string;
    modified: string | null;
    size_bytes: number;
  }

  const emuId  = $derived($page.params.id);
  const rawId  = $derived($page.params.raw_id);
  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let entry    = $state<SaveEntry | null>(null);
  let coverUrl = $state<string | null>(null);
  let imgOk    = $state(false);
  let loadErr  = $state("");

  // actions
  let syncing  = $state(false);
  let deleting = $state(false);
  let confirmDelete = $state(false);
  let actionErr = $state("");
  let syncOk   = $state(false);

  $effect(() => {
    void rawId;
    loadEntry();
  });

  async function loadEntry() {
    loadErr = "";
    entry = null;
    coverUrl = null;
    imgOk = false;
    try {
      entry = await invoke<SaveEntry | null>("get_save_entry", { id: emuId, rawId });
      if (entry) fetchCover(entry.title);
    } catch (e) {
      loadErr = String(e);
    }
  }

  async function fetchCover(title: string) {
    try {
      coverUrl = await invoke<string | null>("fetch_cover_url", { title });
    } catch {
      coverUrl = null;
    }
  }

  async function syncSave() {
    syncing = true;
    actionErr = "";
    syncOk = false;
    try {
      await invoke("sync_one_save", { id: emuId, rawId });
      syncOk = true;
      setTimeout(() => (syncOk = false), 3000);
    } catch (e) {
      actionErr = String(e);
    } finally {
      syncing = false;
    }
  }

  async function openFolder() {
    actionErr = "";
    try {
      await invoke("open_save_folder", { id: emuId, rawId });
    } catch (e) {
      actionErr = String(e);
    }
  }

  async function deleteSave() {
    deleting = true;
    actionErr = "";
    try {
      await invoke("delete_save_entry", { id: emuId, rawId });
      goto(`/emulator/${emuId}/saves`);
    } catch (e) {
      actionErr = String(e);
      deleting = false;
      confirmDelete = false;
    }
  }

  function fmtBytes(b: number): string {
    if (b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
    return `${(b / 1024 / 1024).toFixed(1)} MB`;
  }

  function initials(title: string): string {
    return title.split(" ").slice(0, 2).map(w => w[0]).join("").toUpperCase();
  }
</script>

<section class="topnav">
  <button class="back" onclick={() => goto(`/emulator/${emuId}/saves`)}>
    <span class="back-arrow">◀</span> back
  </button>
  {#if $current && entry}
    <span class="nav-title">{$current.name} / saves / {entry.title}</span>
  {/if}
</section>

{#if loadErr}
  <section class="alert">
    <span class="alert-tag">! ERROR</span>
    <span>{loadErr}</span>
  </section>
{:else if !entry}
  <p class="status-line">// loading…</p>
{:else}
  <div class="detail">
    <div class="cover-col">
      <div class="cover" class:has-img={imgOk}>
        {#if coverUrl}
          <img
            src={coverUrl}
            alt={entry.title}
            class="cover-img"
            class:hidden={!imgOk}
            onload={() => (imgOk = true)}
            onerror={() => (imgOk = false)}
          />
        {/if}
        {#if !imgOk}
          <div class="cover-fallback">
            <span class="cover-initials">{initials(entry.title)}</span>
            <span class="cover-short">{entry.title}</span>
          </div>
        {/if}
      </div>
    </div>

    <div class="info-col">
      <h1 class="game-title">{entry.title}</h1>
      <span class="game-id">{entry.raw_id}</span>

      <div class="stats">
        <div class="stat">
          <span class="stat-label">// size</span>
          <span class="stat-value">{fmtBytes(entry.size_bytes)}</span>
        </div>
        {#if entry.modified}
          <div class="stat">
            <span class="stat-label">// modified</span>
            <span class="stat-value">{entry.modified}</span>
          </div>
        {/if}
        {#if $current?.last_sync}
          <div class="stat">
            <span class="stat-label">// last sync</span>
            <span class="stat-value">{$current.last_sync}</span>
          </div>
        {/if}
      </div>

      <div class="actions">
        <button class="action-btn" onclick={syncSave} disabled={syncing}>
          {#if syncing}
            // syncing…
          {:else if syncOk}
            // sync done ✓
          {:else}
            [ sync this save ]
          {/if}
        </button>

        <button class="action-btn" onclick={openFolder}>
          [ open folder ]
        </button>

        {#if !confirmDelete}
          <button class="action-btn danger" onclick={() => (confirmDelete = true)}>
            [ delete save ]
          </button>
        {:else}
          <div class="confirm-row">
            <span class="confirm-label">! confirm delete?</span>
            <button class="action-btn danger" onclick={deleteSave} disabled={deleting}>
              {deleting ? "deleting…" : "[ yes, delete ]"}
            </button>
            <button class="action-btn" onclick={() => (confirmDelete = false)}>
              [ cancel ]
            </button>
          </div>
        {/if}
      </div>

      {#if actionErr}
        <p class="action-err">! {actionErr}</p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .topnav {
    margin: 1rem 0 1.5rem;
    display: flex;
    align-items: center;
    gap: 0.8rem;
  }

  .back {
    background: transparent;
    border: 1px dashed var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.74rem;
    padding: 0.35rem 0.7rem;
    cursor: pointer;
    letter-spacing: 0.06em;
    transition: all 0.14s;
    flex-shrink: 0;
  }

  .back:hover {
    color: var(--text-bright);
    border-color: var(--text-soft);
    background: var(--hover-tint);
  }

  .back-arrow {
    color: var(--accent);
    margin-right: 0.25rem;
  }

  .nav-title {
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.1em;
    text-transform: uppercase;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .status-line {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.06em;
  }

  /* ── layout ── */
  .detail {
    display: flex;
    gap: 2rem;
    align-items: flex-start;
  }

  /* ── cover ── */
  .cover-col {
    flex-shrink: 0;
  }

  .cover {
    position: relative;
    width: 160px;
    aspect-ratio: 2 / 3;
    overflow: hidden;
    background: var(--bg-hint);
    border: 1px solid var(--border);
  }

  .cover-img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    transition: opacity 0.2s;
  }

  .cover-img.hidden {
    opacity: 0;
    pointer-events: none;
  }

  .cover-fallback {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    padding: 0.8rem;
    text-align: center;
  }

  .cover-initials {
    font-family: "Major Mono Display", monospace;
    font-size: 2.2rem;
    color: var(--text-muted);
    line-height: 1;
    letter-spacing: 0.05em;
  }

  .cover-short {
    font-size: 0.62rem;
    color: var(--text-faint);
    letter-spacing: 0.04em;
    line-height: 1.3;
    word-break: break-word;
  }

  /* ── info ── */
  .info-col {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .game-title {
    font-size: 1.1rem;
    color: var(--text-bright);
    letter-spacing: 0.03em;
    font-weight: 400;
    margin: 0;
    line-height: 1.3;
  }

  .game-id {
    font-size: 0.65rem;
    color: var(--text-faint);
    letter-spacing: 0.06em;
    font-variant-numeric: tabular-nums;
  }

  .stats {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin-top: 0.4rem;
    padding-top: 0.6rem;
    border-top: 1px solid var(--border);
  }

  .stat {
    display: flex;
    gap: 1rem;
    align-items: baseline;
  }

  .stat-label {
    font-size: 0.65rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.06em;
    width: 7rem;
    flex-shrink: 0;
  }

  .stat-value {
    font-size: 0.75rem;
    color: var(--text-soft);
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.04em;
  }

  /* ── actions ── */
  .actions {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-top: 0.8rem;
    padding-top: 0.7rem;
    border-top: 1px solid var(--border);
  }

  .action-btn {
    background: transparent;
    border: 1px solid var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.76rem;
    padding: 0.45rem 0.8rem;
    cursor: pointer;
    letter-spacing: 0.06em;
    text-align: left;
    transition: all 0.13s;
    width: fit-content;
  }

  .action-btn:hover:not(:disabled) {
    color: var(--accent);
    border-color: var(--accent);
    background: var(--hover-tint);
  }

  .action-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .action-btn.danger {
    color: var(--error, #e05c5c);
    border-color: var(--error-border, #5a2b2b);
  }

  .action-btn.danger:hover:not(:disabled) {
    background: var(--error-bg, rgba(200, 50, 50, 0.08));
    border-color: var(--error, #e05c5c);
    color: var(--error, #e05c5c);
  }

  .confirm-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
  }

  .confirm-label {
    font-size: 0.72rem;
    color: var(--error, #e05c5c);
    letter-spacing: 0.06em;
  }

  .action-err {
    font-size: 0.7rem;
    color: var(--error, #e05c5c);
    letter-spacing: 0.04em;
    margin-top: 0.3rem;
  }

  /* ── misc ── */
  .alert {
    display: flex;
    gap: 0.9rem;
    padding: 0.65rem 0.9rem;
    border: 1px solid var(--error-border);
    background: var(--error-bg);
    color: var(--error-text);
    font-size: 0.78rem;
    margin-bottom: 1rem;
  }

  .alert-tag {
    color: var(--error);
    font-weight: 700;
    letter-spacing: 0.1em;
    flex-shrink: 0;
  }
</style>
