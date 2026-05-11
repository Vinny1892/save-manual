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

  const READ_ONLY_EMUS = new Set(["pcsx2"]);

  const emuId  = $derived($page.params.id);
  const rawId  = $derived($page.params.raw_id);
  const readOnly = $derived(READ_ONLY_EMUS.has(emuId));
  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let entry    = $state<SaveEntry | null>(null);
  let coverUrl = $state<string | null>(null);
  let imgOk    = $state(false);
  let loadErr  = $state("");
  let tint     = $state<string | null>(null); // "r, g, b" — null = no theming

  // actions
  let syncing  = $state(false);
  let deleting = $state(false);
  let confirmDelete = $state(false);
  let actionErr = $state("");
  let syncOk   = $state(false);

  // history
  interface HistoryVersion {
    timestamp: string;
    has_full: boolean;
    has_delta: boolean;
    size_bytes: number;
  }
  let history = $state<HistoryVersion[]>([]);
  let historyLoading = $state(false);
  let historyErr = $state("");
  let revertingTs = $state<string | null>(null);
  let confirmRevert = $state<string | null>(null);
  let revertOk = $state<string | null>(null);

  $effect(() => {
    void rawId;
    loadEntry();
    loadHistory();
  });

  // Propagate tint to <html> so app-wide chrome (title bar, scrollbar) can
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

  async function loadEntry() {
    loadErr = "";
    entry = null;
    coverUrl = null;
    imgOk = false;
    tint = null;
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
      if (coverUrl) {
        // Tint extraction runs in Rust to avoid the Tauri webview CORS
        // restriction on the SGDB CDN (which taints the canvas client-side).
        invoke<string | null>("fetch_cover_tint", { url: coverUrl })
          .then((t) => { tint = t; })
          .catch(() => { tint = null; });
      }
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

  async function loadHistory() {
    historyLoading = true;
    historyErr = "";
    history = [];
    try {
      history = await invoke<HistoryVersion[]>("list_save_history", { id: emuId, rawId });
    } catch (e) {
      historyErr = String(e);
    } finally {
      historyLoading = false;
    }
  }

  async function revert(ts: string) {
    revertingTs = ts;
    historyErr = "";
    try {
      await invoke("revert_save", { id: emuId, rawId, timestamp: ts });
      revertOk = ts;
      setTimeout(() => (revertOk = null), 4000);
      // Reload the entry (size/modified will reflect the reverted version)
      // and history (the previous live state is now in a new pending snapshot
      // once user re-syncs, but listing doesn't change instantly).
      await loadEntry();
    } catch (e) {
      historyErr = String(e);
    } finally {
      revertingTs = null;
      confirmRevert = null;
    }
  }

  /// Converts `2026-05-09T14-30-00Z` (rclone-safe ISO with hyphens) into a
  /// `Date` so we can use toLocaleString for display. The hyphens in time
  /// position would break `new Date()` directly, so we patch them back to
  /// colons before parsing.
  function parseTs(ts: string): Date | null {
    // "2026-05-09T14-30-00Z" → "2026-05-09T14:30:00Z"
    const iso = ts.replace(
      /^(\d{4}-\d{2}-\d{2})T(\d{2})-(\d{2})-(\d{2})Z$/,
      "$1T$2:$3:$4Z"
    );
    const d = new Date(iso);
    return isNaN(d.getTime()) ? null : d;
  }

  function fmtTs(ts: string): string {
    const d = parseTs(ts);
    if (!d) return ts;
    return d.toLocaleString("pt-BR", {
      day: "2-digit",
      month: "2-digit",
      year: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  function ageLabel(ts: string): string {
    const d = parseTs(ts);
    if (!d) return "";
    const diffSec = (Date.now() - d.getTime()) / 1000;
    const hours = diffSec / 3600;
    const days = hours / 24;
    if (days >= 1) return `${Math.round(days)}d atrás`;
    if (hours >= 1) return `${Math.round(hours)}h atrás`;
    return `${Math.max(1, Math.round(diffSec / 60))}min atrás`;
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
    <div class="cover-panel">
      <div class="cover">
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
          </div>
        {/if}
      </div>
    </div>

    <div class="info-panel">
      <div class="info-top">
        <h1 class="game-title">{entry.title}</h1>
        <span class="game-id">{entry.raw_id}</span>
      </div>

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
        {#if readOnly}
          <p class="readonly-note">// list-only mode — backup via [ sync now ] on the unit page</p>
        {:else}
          <button class="action-btn" onclick={syncSave} disabled={syncing}>
            {#if syncing}// syncing…{:else if syncOk}// sync done ✓{:else}[ sync this save ]{/if}
          </button>
        {/if}
        <button class="action-btn" onclick={openFolder}>[ open folder ]</button>

        {#if !readOnly}
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
              <button class="action-btn" onclick={() => (confirmDelete = false)}>[ cancel ]</button>
            </div>
          {/if}
        {/if}

        {#if actionErr}
          <p class="action-err">! {actionErr}</p>
        {/if}
      </div>
    </div>
  </div>

  <section class="history-card">
    <header class="hist-head">
      <span class="hist-tag">[ history ]</span>
      <span class="hist-meta">
        {#if historyLoading}
          // loading…
        {:else}
          {history.length} {history.length === 1 ? "version" : "versions"}
        {/if}
      </span>
      <button class="hist-refresh" onclick={loadHistory} disabled={historyLoading} aria-label="refresh">↻</button>
    </header>

    {#if historyErr}
      <p class="hist-err">! {historyErr}</p>
    {:else if !historyLoading && history.length === 0}
      <p class="hist-empty">// nenhuma versão anterior — history só começa a popular depois do segundo sync</p>
    {:else}
      <ul class="hist-list">
        {#each history as h (h.timestamp)}
          <li class="hist-row" class:reverting={revertingTs === h.timestamp}>
            <div class="hist-when">
              <span class="hist-date">{fmtTs(h.timestamp)}</span>
              <span class="hist-age">{ageLabel(h.timestamp)}</span>
            </div>
            <div class="hist-modes">
              {#if h.has_full}<span class="badge full">[ full ]</span>{/if}
              {#if h.has_delta}<span class="badge delta">[ delta ]</span>{/if}
            </div>
            <span class="hist-size">{fmtBytes(h.size_bytes)}</span>
            <div class="hist-action">
              {#if confirmRevert === h.timestamp}
                <span class="confirm-label">! sobrescreve atual?</span>
                <button class="action-btn danger" onclick={() => revert(h.timestamp)} disabled={revertingTs !== null}>
                  {revertingTs === h.timestamp ? "…" : "[ confirm ]"}
                </button>
                <button class="action-btn" onclick={() => (confirmRevert = null)}>[ cancel ]</button>
              {:else if revertOk === h.timestamp}
                <span class="revert-ok">// reverted ✓</span>
              {:else}
                <button class="action-btn" onclick={() => (confirmRevert = h.timestamp)} disabled={revertingTs !== null || readOnly && !h.has_full}>
                  [ revert ]
                </button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    {#if revertOk}
      <p class="hist-hint">// na próxima sync o rclone vai re-baselinar (resync) — esperado, e não dura muito</p>
    {/if}
  </section>
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


  /* ── page wrapper / fullscreen tint backdrop ── */
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

  /* ── topnav (tinted) ── */
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

  /* ── layout ── */
  .detail {
    display: grid;
    grid-template-columns: 260px 1fr;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    overflow: hidden;
    margin-top: 6rem;
    transition: border-color 0.6s, box-shadow 0.6s, background 0.6s;
  }

  /* full takeover: panel bg solid-ish in tint, all neutrals replaced. */
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

  /* ── cover panel ── */
  .cover-panel {
    border-right: 1px solid var(--border);
  }

  .page.tinted .cover-panel {
    border-right-color: rgb(var(--game-tint));
    background: rgba(var(--game-tint), 0.15);
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

  /* ── info panel ── */
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
    transition: text-shadow 0.6s;
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

  .page.tinted .info-top {
    border-bottom-color: rgb(var(--game-tint));
  }

  .page.tinted .stats {
    border-bottom-color: rgb(var(--game-tint));
  }

  .page.tinted .stat-label {
    color: rgba(var(--game-tint), 0.65);
  }

  .page.tinted .stat-value {
    color: rgb(var(--game-tint));
  }

  .page.tinted .action-btn {
    border-color: rgba(var(--game-tint), 0.7);
    color: rgba(var(--game-tint), 0.92);
  }

  .page.tinted .action-btn:hover:not(:disabled) {
    color: rgb(var(--game-tint));
    border-color: rgb(var(--game-tint));
    background: rgba(var(--game-tint), 0.18);
    box-shadow: 0 0 14px -2px rgba(var(--game-tint), 0.6);
  }

  .page.tinted .readonly-note {
    color: rgba(var(--game-tint), 0.65);
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
  }

  /* ── actions ── */
  .actions {
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
    padding-top: 0.9rem;
    flex: 1;
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

  .readonly-note {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.05em;
    margin: 0 0 0.3rem;
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

  .history-card {
    margin-top: 1.4rem;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    padding: 0.85rem 1rem;
  }

  .hist-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.6rem;
    border-bottom: 1px dashed var(--border);
    padding-bottom: 0.4rem;
    margin-bottom: 0.7rem;
  }

  .hist-tag {
    color: var(--accent);
    font-size: 0.74rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .hist-meta {
    color: var(--text-muted);
    font-size: 0.68rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-left: auto;
  }

  .hist-refresh {
    background: transparent;
    border: 1px dashed var(--border-strong);
    color: var(--text-muted);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 0.1rem 0.45rem;
    cursor: pointer;
    transition: all 0.14s;
  }

  .hist-refresh:hover:not(:disabled) {
    color: var(--accent);
    border-color: var(--accent);
  }

  .hist-refresh:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .hist-empty,
  .hist-err {
    margin: 0.3rem 0;
    font-size: 0.76rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.04em;
  }

  .hist-err {
    color: var(--error, #e05c5c);
    font-style: normal;
  }

  .hist-hint {
    margin: 0.7rem 0 0;
    font-size: 0.69rem;
    color: var(--text-faint);
    font-style: italic;
    letter-spacing: 0.04em;
  }

  .hist-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .hist-row {
    display: grid;
    grid-template-columns: 1fr auto auto auto;
    gap: 0.8rem;
    align-items: center;
    padding: 0.55rem 0;
    border-bottom: 1px dotted var(--border);
    font-size: 0.78rem;
  }

  .hist-row:last-child {
    border-bottom: none;
  }

  .hist-row.reverting {
    opacity: 0.55;
  }

  .hist-when {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .hist-date {
    color: var(--text-bright);
    font-variant-numeric: tabular-nums;
  }

  .hist-age {
    color: var(--text-faint);
    font-size: 0.66rem;
    font-style: italic;
    letter-spacing: 0.04em;
  }

  .hist-modes {
    display: flex;
    gap: 0.35rem;
  }

  .badge {
    font-size: 0.66rem;
    letter-spacing: 0.05em;
    padding: 0.15rem 0.35rem;
    border: 1px solid var(--border-strong);
    color: var(--text-muted);
  }

  .badge.full {
    color: var(--accent);
    border-color: var(--accent);
  }

  .badge.delta {
    color: var(--text-soft);
    border-color: var(--text-soft);
  }

  .hist-size {
    color: var(--text-soft);
    font-variant-numeric: tabular-nums;
    font-size: 0.73rem;
  }

  .hist-action {
    display: flex;
    gap: 0.4rem;
    align-items: center;
  }

  .revert-ok {
    color: var(--success, #5ec07a);
    font-size: 0.72rem;
    letter-spacing: 0.06em;
  }

  @media (max-width: 600px) {
    .hist-row {
      grid-template-columns: 1fr auto;
      grid-template-rows: auto auto;
    }
    .hist-modes,
    .hist-size {
      grid-column: 1;
    }
    .hist-action {
      grid-row: 1 / 3;
      grid-column: 2;
    }
  }
</style>
