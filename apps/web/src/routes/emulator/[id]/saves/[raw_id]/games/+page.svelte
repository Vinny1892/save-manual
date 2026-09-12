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

  const emuId = $derived($page.params.id);
  const rawId = $derived($page.params.raw_id);
  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let saves = $state<McSave[]>([]);
  let loading = $state(false);
  let loadErr = $state("");
  let viewMode = $state<"grid" | "list">("grid");

  let gridUrls = $state<Map<string, string>>(new Map());
  let iconUrls = $state<Map<string, string>>(new Map());
  let gridStatus = $state<Map<string, "loading" | "ok" | "err">>(new Map());
  let iconStatus = $state<Map<string, "loading" | "ok" | "err">>(new Map());

  $effect(() => {
    void rawId;
    void emuId;
    load();
  });

  $effect(() => {
    const isList = viewMode === "list";
    const urls = isList ? iconUrls : gridUrls;
    const status = isList ? iconStatus : gridStatus;
    const pending = saves.filter(
      (s) => s.title && !urls.has(s.name) && !status.has(s.name),
    );
    if (pending.length === 0) return;
    const next = new Map<string, "loading" | "ok" | "err">([
      ...status,
      ...pending.map((s) => [s.name, "loading"] as [string, "loading"]),
    ]);
    if (isList) iconStatus = next;
    else gridStatus = next;
    for (const save of pending) {
      fetchAsset(save.name, save.title!, isList ? "icon" : "grid");
    }
  });

  async function fetchAsset(key: string, title: string, kind: "grid" | "icon") {
    try {
      const url = await invoke<string | null>("fetch_cover_url", { title, kind });
      if (kind === "icon") {
        if (url) iconUrls = new Map(iconUrls).set(key, url);
        else iconStatus = new Map(iconStatus).set(key, "err");
      } else {
        if (url) gridUrls = new Map(gridUrls).set(key, url);
        else gridStatus = new Map(gridStatus).set(key, "err");
      }
    } catch {
      if (kind === "icon") iconStatus = new Map(iconStatus).set(key, "err");
      else gridStatus = new Map(gridStatus).set(key, "err");
    }
  }

  function onImgLoad(key: string, kind: "grid" | "icon") {
    if (kind === "icon") iconStatus = new Map(iconStatus).set(key, "ok");
    else gridStatus = new Map(gridStatus).set(key, "ok");
  }

  function onImgErr(key: string, kind: "grid" | "icon") {
    if (kind === "icon") {
      iconStatus = new Map(iconStatus).set(key, "err");
      iconUrls = new Map([...iconUrls].filter(([k]) => k !== key));
    } else {
      gridStatus = new Map(gridStatus).set(key, "err");
      gridUrls = new Map([...gridUrls].filter(([k]) => k !== key));
    }
  }

  async function load() {
    loading = true;
    loadErr = "";
    saves = [];
    gridUrls = new Map();
    iconUrls = new Map();
    gridStatus = new Map();
    iconStatus = new Map();
    try {
      saves = await invoke<McSave[]>("list_memcard_saves", { id: emuId, rawId });
    } catch (e) {
      loadErr = String(e);
    } finally {
      loading = false;
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

  function saveHref(save: McSave): string {
    return `/emulator/${emuId}/saves/${rawId}/games/${encodeURIComponent(save.name)}`;
  }
</script>

<section class="topnav">
  <a class="back" href="/emulator/{emuId}/saves">
    <span class="back-arrow">◀</span> back
  </a>
  {#if $current}
    <span class="nav-title">{$current.name} / {rawId}</span>
  {/if}
  <div class="view-toggle">
    <button
      class="vbtn"
      class:active={viewMode === "grid"}
      title="grid view"
      onclick={() => (viewMode = "grid")}
    >⊞</button>
    <button
      class="vbtn"
      class:active={viewMode === "list"}
      title="list view"
      onclick={() => (viewMode = "list")}
    >≡</button>
  </div>
</section>

{#if loadErr}
  <section class="alert">
    <span class="alert-tag">! ERROR</span>
    <span>{loadErr}</span>
  </section>
{:else if loading}
  <p class="status-line">// reading memcard…</p>
{:else}
  <p class="status-line">
    // {saves.length} game{saves.length !== 1 ? "s" : ""} on this card
  </p>

  {#if viewMode === "grid"}
    <div class="grid">
      {#each saves as save, i (save.name)}
        {@const coverUrl = gridUrls.get(save.name)}
        {@const status = gridStatus.get(save.name)}
        {@const display = save.title ?? save.serial ?? save.name}
        <a class="card" style="--i: {i}" href={saveHref(save)}>
          <div class="cover">
            {#if coverUrl}
              <img
                src={coverUrl}
                alt={display}
                class="cover-img"
                class:hidden={status !== "ok"}
                onload={() => onImgLoad(save.name, "grid")}
                onerror={() => onImgErr(save.name, "grid")}
              />
            {/if}
            {#if status !== "ok"}
              <div class="cover-fallback">
                <span class="cover-initials">{initials(display)}</span>
                <span class="cover-short">{display}</span>
              </div>
            {/if}
          </div>
          <div class="card-info">
            <span class="card-title" title={display}>{display}</span>
            <span class="card-id">{save.serial ?? save.name}</span>
            <div class="card-meta">
              <span class="meta-item">{fmtBytes(save.size_bytes)}</span>
              {#if save.modified}
                <span class="meta-sep">·</span>
                <span class="meta-item">{save.modified}</span>
              {/if}
            </div>
          </div>
        </a>
      {/each}
    </div>
  {:else}
    <div class="list">
      {#each saves as save, i (save.name)}
        {@const coverUrl = iconUrls.get(save.name)}
        {@const status = iconStatus.get(save.name)}
        {@const display = save.title ?? save.serial ?? save.name}
        <a class="row" style="--i: {i}" href={saveHref(save)}>
          <div class="row-thumb">
            {#if coverUrl}
              <img
                src={coverUrl}
                alt={display}
                class="thumb-img"
                class:hidden={status !== "ok"}
                onload={() => onImgLoad(save.name, "icon")}
                onerror={() => onImgErr(save.name, "icon")}
              />
            {/if}
            {#if status !== "ok"}
              <span class="thumb-initials">{initials(display)}</span>
            {/if}
          </div>
          <div class="row-info">
            <span class="row-title">{display}</span>
            <span class="row-id">
              {save.serial ?? save.name}
              {#if save.serial && save.serial !== save.name}
                <span class="row-folder">· {save.name}</span>
              {/if}
            </span>
          </div>
          <div class="row-meta">
            <span class="meta-item">{fmtBytes(save.size_bytes)}</span>
            {#if save.modified}
              <span class="meta-sep">·</span>
              <span class="meta-item">{save.modified}</span>
            {/if}
          </div>
        </a>
      {/each}
    </div>
  {/if}

  {#if saves.length === 0}
    <p class="empty-note">// nenhum save encontrado neste memcard</p>
  {/if}
{/if}

<style>
  .topnav {
    margin: 1rem 0 1rem;
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
  }

  .view-toggle {
    margin-left: auto;
    display: flex;
    gap: 0.25rem;
  }

  .vbtn {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-faint);
    font-family: inherit;
    font-size: 0.9rem;
    width: 1.9rem;
    height: 1.6rem;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 0.12s;
    line-height: 1;
  }

  .vbtn:hover {
    color: var(--text-soft);
    border-color: var(--border-strong);
  }

  .vbtn.active {
    color: var(--accent);
    border-color: var(--accent);
    background: var(--hover-tint);
  }

  .status-line {
    font-size: 0.72rem;
    color: var(--text-muted);
    font-style: italic;
    letter-spacing: 0.06em;
    margin: 0 0 1rem;
  }

  .empty-note {
    margin-top: 1rem;
    font-size: 0.74rem;
    color: var(--text-faint);
    font-style: italic;
    letter-spacing: 0.05em;
  }

  /* ── grid ── */
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 0.85rem;
  }

  .card {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    overflow: hidden;
    opacity: 0;
    transform: translateY(6px);
    animation: reveal 0.28s ease-out forwards;
    animation-delay: calc(var(--i) * 45ms);
    transition: border-color 0.14s, background 0.14s;
    cursor: pointer;
    text-decoration: none;
    color: inherit;
  }

  .card:hover {
    border-color: var(--border-strong);
    background: var(--bg-unit-2);
  }

  .card:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: 2px;
  }

  .cover {
    position: relative;
    aspect-ratio: 2 / 3;
    overflow: hidden;
    background: var(--bg-hint);
    border-bottom: 1px solid var(--border);
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
    padding: 0.6rem;
    text-align: center;
  }

  .cover-initials {
    font-family: "Major Mono Display", monospace;
    font-size: 1.8rem;
    color: var(--text-muted);
    line-height: 1;
    letter-spacing: 0.05em;
  }

  .cover-short {
    font-size: 0.6rem;
    color: var(--text-faint);
    letter-spacing: 0.05em;
    line-height: 1.3;
    word-break: break-word;
  }

  .card-info {
    padding: 0.5rem 0.55rem 0.6rem;
    display: flex;
    flex-direction: column;
    gap: 0.18rem;
  }

  .card-title {
    font-size: 0.72rem;
    color: var(--text-bright);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    letter-spacing: 0.02em;
    line-height: 1.3;
  }

  .card-id {
    font-size: 0.58rem;
    color: var(--text-faint);
    letter-spacing: 0.04em;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    margin-top: 0.2rem;
    flex-wrap: wrap;
  }

  /* ── list ── */
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.5rem 0.7rem;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    opacity: 0;
    transform: translateY(4px);
    animation: reveal 0.22s ease-out forwards;
    animation-delay: calc(var(--i) * 30ms);
    text-decoration: none;
    color: inherit;
    cursor: pointer;
    transition: border-color 0.12s, background 0.12s;
  }

  .row:hover {
    border-color: var(--border-strong);
    background: var(--bg-unit-2);
  }

  .row:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: 2px;
  }

  .row-thumb {
    position: relative;
    width: 40px;
    height: 40px;
    flex-shrink: 0;
    background: var(--bg-hint);
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .thumb-img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    transition: opacity 0.2s;
  }

  .thumb-img.hidden {
    opacity: 0;
    pointer-events: none;
  }

  .thumb-initials {
    font-family: "Major Mono Display", monospace;
    font-size: 0.75rem;
    color: var(--text-muted);
    letter-spacing: 0.04em;
    line-height: 1;
    position: relative;
    z-index: 1;
  }

  .row-info {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    flex: 1;
    min-width: 0;
  }

  .row-title {
    font-size: 0.78rem;
    color: var(--text-bright);
    letter-spacing: 0.04em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .row-id {
    font-size: 0.62rem;
    color: var(--text-faint);
    letter-spacing: 0.04em;
    font-variant-numeric: tabular-nums;
    word-break: break-all;
  }

  .row-folder {
    color: var(--text-faint);
    font-size: 0.58rem;
    margin-left: 0.2rem;
  }

  .row-meta {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    flex-shrink: 0;
    white-space: nowrap;
  }

  .meta-item {
    font-size: 0.66rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .meta-sep {
    color: var(--text-faint);
    font-size: 0.66rem;
  }

  @keyframes reveal {
    to { opacity: 1; transform: translateY(0); }
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
</style>
