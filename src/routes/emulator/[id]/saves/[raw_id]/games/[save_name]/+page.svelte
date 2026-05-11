<script lang="ts">
  import { page } from "$app/stores";
  import { invoke } from "@tauri-apps/api/core";
  import { emulators } from "$lib/store";
  import { derived } from "svelte/store";

  interface McSave {
    name: string;
    serial: string | null;
    title: string | null;
    modified: string | null;
    size_bytes: number;
  }

  const emuId = $derived($page.params.id ?? "");
  const rawId = $derived($page.params.raw_id ?? "");
  const saveName = $derived(decodeURIComponent($page.params.save_name ?? ""));
  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let entry = $state<McSave | null>(null);
  let coverUrl = $state<string | null>(null);
  let imgOk = $state(false);
  let loadErr = $state("");
  let tint = $state<string | null>(null);

  $effect(() => {
    void saveName;
    void rawId;
    void emuId;
    load();
  });

  // Propagate tint to <html> so app-wide chrome (title bar, scrollbar)
  // pick it up via global CSS in app.css. Cleared on navigation away.
  $effect(() => {
    const root = document.documentElement;
    if (tint) {
      root.style.setProperty("--game-tint", tint);
      root.setAttribute("data-tinted", "save");
    } else {
      root.style.removeProperty("--game-tint");
      root.removeAttribute("data-tinted");
    }
    return () => {
      root.style.removeProperty("--game-tint");
      root.removeAttribute("data-tinted");
    };
  });

  async function load() {
    loadErr = "";
    entry = null;
    coverUrl = null;
    imgOk = false;
    tint = null;
    try {
      const all = await invoke<McSave[]>("list_memcard_saves", { id: emuId, rawId });
      entry = all.find((s) => s.name === saveName) ?? null;
      if (!entry) {
        loadErr = `save "${saveName}" não encontrado neste memcard`;
        return;
      }
      if (entry.title) fetchCover(entry.title);
    } catch (e) {
      loadErr = String(e);
    }
  }

  async function fetchCover(title: string) {
    try {
      coverUrl = await invoke<string | null>("fetch_cover_url", { title });
      if (coverUrl) {
        invoke<string | null>("fetch_cover_tint", { url: coverUrl })
          .then((t) => { tint = t; })
          .catch(() => { tint = null; });
      }
    } catch {
      coverUrl = null;
    }
  }

  function fmtBytes(b: number): string {
    if (b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
    return `${(b / 1024 / 1024).toFixed(1)} MB`;
  }

  function initials(s: string): string {
    return s.split(/\s+/).slice(0, 2).map((w) => w[0] ?? "").join("").toUpperCase();
  }
</script>

<div
  class="page"
  class:tinted={tint !== null}
  style={tint ? `--game-tint: ${tint};` : ""}
>
{#if tint !== null}
  <div class="page-tint-bg" aria-hidden="true"></div>
{/if}

<section class="topnav">
  <a class="back" href="/emulator/{emuId}/saves/{rawId}/games">
    <span class="back-arrow">◀</span> back
  </a>
  {#if $current && entry}
    <span class="nav-title">{$current.name} / {rawId} / {entry.title ?? entry.serial ?? entry.name}</span>
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
  {@const display = entry.title ?? entry.serial ?? entry.name}
  <div class="detail">
    <div class="cover-panel">
      <div class="cover">
        {#if coverUrl}
          <img
            src={coverUrl}
            alt={display}
            class="cover-img"
            class:hidden={!imgOk}
            onload={() => (imgOk = true)}
            onerror={() => (imgOk = false)}
          />
        {/if}
        {#if !imgOk}
          <div class="cover-fallback">
            <span class="cover-initials">{initials(display)}</span>
          </div>
        {/if}
      </div>
    </div>

    <div class="info-panel">
      <div class="info-top">
        <h1 class="game-title">{display}</h1>
        {#if entry.serial}
          <span class="game-id">{entry.serial}</span>
        {/if}
      </div>

      <div class="stats">
        <div class="stat">
          <span class="stat-label">// folder</span>
          <span class="stat-value">{entry.name}</span>
        </div>
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
        <div class="stat">
          <span class="stat-label">// memcard</span>
          <span class="stat-value">{rawId}</span>
        </div>
      </div>

      <p class="readonly-note">// list-only mode — backup via [ sync now ] on the unit page</p>
    </div>
  </div>
{/if}
</div>

<style>
  .topnav {
    margin: 1rem 0 0;
    display: flex;
    align-items: center;
    gap: 0.8rem;
  }

  .back {
    display: inline-flex;
    align-items: center;
    background: transparent;
    border: 1px dashed var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.74rem;
    padding: 0.35rem 0.7rem;
    cursor: pointer;
    letter-spacing: 0.06em;
    text-decoration: none;
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

  .detail {
    display: grid;
    grid-template-columns: 260px 1fr;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    overflow: hidden;
    margin-top: 1.5rem;
  }

  .cover-panel {
    border-right: 1px solid var(--border);
  }

  .cover {
    position: relative;
    width: 100%;
    aspect-ratio: 2 / 3;
    overflow: hidden;
    background: var(--bg-hint);
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
    align-items: center;
    justify-content: center;
  }

  .cover-initials {
    font-family: "Major Mono Display", monospace;
    font-size: 2.4rem;
    color: var(--text-muted);
    line-height: 1;
    letter-spacing: 0.05em;
  }

  .info-panel {
    display: flex;
    flex-direction: column;
    padding: 1.4rem 1.6rem;
    gap: 0;
    justify-content: space-between;
  }

  .info-top {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    padding-bottom: 1rem;
    border-bottom: 1px solid var(--border);
  }

  .game-title {
    font-size: 1rem;
    color: var(--text-bright);
    letter-spacing: 0.03em;
    font-weight: 400;
    margin: 0;
    line-height: 1.4;
  }

  .game-id {
    font-size: 0.62rem;
    color: var(--text-faint);
    letter-spacing: 0.06em;
    font-variant-numeric: tabular-nums;
  }

  .stats {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.9rem 0;
    border-bottom: 1px solid var(--border);
  }

  .stat {
    display: flex;
    gap: 1rem;
    align-items: baseline;
  }

  .stat-label {
    font-size: 0.63rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.06em;
    width: 6rem;
    flex-shrink: 0;
  }

  .stat-value {
    font-size: 0.73rem;
    color: var(--text-soft);
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.04em;
    word-break: break-all;
  }

  .readonly-note {
    margin: 0.9rem 0 0;
    font-size: 0.7rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.05em;
  }

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

  /* ── per-game tint (full takeover, scoped to this page) ── */
  .page {
    position: relative;
  }

  .page-tint-bg {
    position: fixed;
    inset: 0;
    pointer-events: none;
    z-index: -1;
    background:
      radial-gradient(ellipse 80% 60% at 50% 25%,
        rgba(var(--game-tint), 0.35) 0%,
        rgba(var(--game-tint), 0.18) 35%,
        rgba(var(--game-tint), 0.06) 70%,
        transparent 100%);
    animation: tint-fade-in 0.7s ease-out;
  }

  @keyframes tint-fade-in {
    from { opacity: 0; }
    to   { opacity: 1; }
  }

  .page.tinted .back {
    border-color: rgba(var(--game-tint), 0.7);
    color: rgb(var(--game-tint));
  }
  .page.tinted .back:hover {
    background: rgba(var(--game-tint), 0.15);
    border-color: rgb(var(--game-tint));
  }
  .page.tinted .back-arrow {
    color: rgb(var(--game-tint));
  }
  .page.tinted .nav-title {
    color: rgba(var(--game-tint), 0.85);
  }

  .page.tinted .detail {
    border-color: rgb(var(--game-tint));
    background:
      linear-gradient(180deg,
        rgba(var(--game-tint), 0.45) 0%,
        rgba(var(--game-tint), 0.30) 60%,
        rgba(var(--game-tint), 0.20) 100%),
      var(--bg-unit-1);
    box-shadow:
      0 0 0 1px rgba(var(--game-tint), 0.6),
      0 12px 70px -8px rgba(var(--game-tint), 0.7);
  }

  .page.tinted .cover-panel {
    border-right-color: rgb(var(--game-tint));
    background: rgba(var(--game-tint), 0.15);
  }

  .page.tinted .game-title {
    color: rgb(var(--game-tint));
    text-shadow:
      0 0 14px rgba(var(--game-tint), 0.85),
      0 0 28px rgba(var(--game-tint), 0.5);
  }

  .page.tinted .game-id {
    color: rgba(var(--game-tint), 0.7);
  }

  .page.tinted .info-top,
  .page.tinted .stats {
    border-bottom-color: rgb(var(--game-tint));
  }

  .page.tinted .stat-label {
    color: rgba(var(--game-tint), 0.65);
  }

  .page.tinted .stat-value {
    color: rgb(var(--game-tint));
  }

  .page.tinted .readonly-note {
    color: rgba(var(--game-tint), 0.65);
  }
</style>
