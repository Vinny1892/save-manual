<script lang="ts">
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

  const MOCK_ENTRIES: SaveEntry[] = [
    { raw_id: "0100152000022000", title: "Mario Kart 8 Deluxe",                      modified: "01/05/2026 22:14", size_bytes: 31_457_280 },
    { raw_id: "01007EF00011E000", title: "The Legend of Zelda Breath of the Wild",   modified: "28/04/2026 18:03", size_bytes: 57_671_680 },
    { raw_id: "0100000000010000", title: "Super Mario Odyssey",                      modified: "20/04/2026 11:45", size_bytes: 12_582_912 },
    { raw_id: "01004D300C5AE000", title: "Metroid Dread",                            modified: "10/04/2026 09:30", size_bytes: 8_388_608  },
    { raw_id: "0100F2C0115B6000", title: "Kirby and the Forgotten Land",             modified: "01/04/2026 14:20", size_bytes: 5_242_880  },
    { raw_id: "0100ABF008968000", title: "Pokemon Scarlet",                          modified: "22/03/2026 20:55", size_bytes: 20_971_520 },
  ];

  const emuId = $derived($page.params.id);
  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let entries = $state<SaveEntry[]>(MOCK_ENTRIES);
  let loading = $state(false);
  let loadErr = $state("");
  let useMock = $state(true);
  let viewMode = $state<"grid" | "list">("grid");

  let coverUrls = $state<Map<string, string>>(new Map());
  let artStatus = $state<Map<string, "loading" | "ok" | "err">>(new Map());

  $effect(() => {
    void emuId;
    if (!useMock) loadSaves();
  });

  $effect(() => {
    const pending = entries.filter(
      (e) => !coverUrls.has(e.raw_id) && !artStatus.has(e.raw_id),
    );
    if (pending.length === 0) return;
    artStatus = new Map([...artStatus, ...pending.map((e) => [e.raw_id, "loading" as const])]);
    for (const entry of pending) {
      fetchCover(entry.raw_id, entry.title);
    }
  });

  async function fetchCover(rawId: string, title: string) {
    try {
      const url = await invoke<string | null>("fetch_cover_url", { title });
      if (url) {
        coverUrls = new Map(coverUrls).set(rawId, url);
      } else {
        artStatus = new Map(artStatus).set(rawId, "err");
      }
    } catch {
      artStatus = new Map(artStatus).set(rawId, "err");
    }
  }

  async function loadSaves() {
    loading = true;
    loadErr = "";
    entries = [];
    coverUrls = new Map();
    artStatus = new Map();
    try {
      entries = await invoke<SaveEntry[]>("list_saves", { id: emuId });
      useMock = false;
    } catch (err) {
      loadErr = String(err);
    } finally {
      loading = false;
    }
  }

  function onImgLoad(id: string) {
    artStatus = new Map(artStatus).set(id, "ok");
  }

  function onImgErr(id: string) {
    artStatus = new Map(artStatus).set(id, "err");
    coverUrls = new Map([...coverUrls].filter(([k]) => k !== id));
  }

  function fmtBytes(b: number): string {
    if (b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
    return `${(b / 1024 / 1024).toFixed(1)} MB`;
  }

  function back() {
    goto(`/emulator/${emuId}`);
  }
</script>

<section class="topnav">
  <button class="back" onclick={back}>
    <span class="back-arrow">◀</span> back
  </button>
  {#if $current}
    <span class="nav-title">{$current.name} / saves</span>
  {/if}
  {#if useMock}
    <span class="mock-badge">// mock data</span>
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
{/if}

{#if loading}
  <p class="status-line">// scanning saves…</p>
{:else}
  <p class="status-line">
    // {entries.length} save{entries.length !== 1 ? "s" : ""} found
    {#if useMock}— <button class="inline-btn" onclick={loadSaves}>load real data</button>{/if}
  </p>

  {#if viewMode === "grid"}
    <div class="grid">
      {#each entries as entry, i (entry.raw_id)}
        {@const coverUrl = coverUrls.get(entry.raw_id)}
        {@const status = artStatus.get(entry.raw_id)}
        <a class="card" style="--i: {i}" href="/emulator/{emuId}/saves/{entry.raw_id}">
          <div class="cover">
            {#if coverUrl}
              <img
                src={coverUrl}
                alt={entry.title}
                class="cover-img"
                class:hidden={status !== "ok"}
                onload={() => onImgLoad(entry.raw_id)}
                onerror={() => onImgErr(entry.raw_id)}
              />
            {/if}
            {#if status !== "ok"}
              <div class="cover-fallback">
                <span class="cover-initials">
                  {entry.title.split(" ").slice(0, 2).map(w => w[0]).join("").toUpperCase()}
                </span>
                <span class="cover-short">{entry.title}</span>
              </div>
            {/if}
          </div>
          <div class="card-info">
            <span class="card-title" title={entry.title}>{entry.title}</span>
            <span class="card-id">{entry.raw_id}</span>
            <div class="card-meta">
              <span class="meta-item">{fmtBytes(entry.size_bytes)}</span>
              {#if entry.modified}
                <span class="meta-sep">·</span>
                <span class="meta-item">{entry.modified}</span>
              {/if}
            </div>
          </div>
        </a>
      {/each}
    </div>
  {:else}
    <div class="list">
      {#each entries as entry, i (entry.raw_id)}
        {@const coverUrl = coverUrls.get(entry.raw_id)}
        {@const status = artStatus.get(entry.raw_id)}
        <a class="row" style="--i: {i}" href="/emulator/{emuId}/saves/{entry.raw_id}">
          <div class="row-thumb">
            {#if coverUrl}
              <img
                src={coverUrl}
                alt={entry.title}
                class="thumb-img"
                class:hidden={status !== "ok"}
                onload={() => onImgLoad(entry.raw_id)}
                onerror={() => onImgErr(entry.raw_id)}
              />
            {/if}
            {#if status !== "ok"}
              <span class="thumb-initials">
                {entry.title.split(" ").slice(0, 2).map(w => w[0]).join("").toUpperCase()}
              </span>
            {/if}
          </div>
          <div class="row-info">
            <span class="row-title">{entry.title}</span>
            <span class="row-id">{entry.raw_id}</span>
          </div>
          <div class="row-meta">
            <span class="meta-item">{fmtBytes(entry.size_bytes)}</span>
            {#if entry.modified}
              <span class="meta-sep">·</span>
              <span class="meta-item">{entry.modified}</span>
            {/if}
          </div>
        </a>
      {/each}
    </div>
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
    background: transparent;
    border: 1px dashed var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.74rem;
    padding: 0.35rem 0.7rem;
    cursor: pointer;
    letter-spacing: 0.06em;
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

  .mock-badge {
    font-size: 0.67rem;
    color: var(--accent);
    font-style: italic;
    letter-spacing: 0.06em;
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

  .inline-btn {
    background: none;
    border: none;
    color: var(--accent);
    font-family: inherit;
    font-size: inherit;
    font-style: inherit;
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
    letter-spacing: inherit;
  }

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

  @keyframes reveal {
    to { opacity: 1; transform: translateY(0); }
  }

  /* ── cover ── */
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
    text-align: center;
    padding: 0 0.3rem;
  }

  /* ── card info ── */
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

  .meta-item {
    font-size: 0.62rem;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .meta-sep {
    color: var(--text-faint);
    font-size: 0.62rem;
  }

  /* ── list view ── */
  .list {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.45rem 0.6rem;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    opacity: 0;
    transform: translateY(4px);
    animation: reveal 0.22s ease-out forwards;
    animation-delay: calc(var(--i) * 30ms);
    transition: border-color 0.12s, background 0.12s;
    cursor: pointer;
    text-decoration: none;
    color: inherit;
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
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    letter-spacing: 0.02em;
  }

  .row-id {
    font-size: 0.6rem;
    color: var(--text-faint);
    letter-spacing: 0.04em;
    font-variant-numeric: tabular-nums;
  }

  .row-meta {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    flex-shrink: 0;
    white-space: nowrap;
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
