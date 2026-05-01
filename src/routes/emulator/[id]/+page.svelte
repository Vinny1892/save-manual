<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { invoke } from "@tauri-apps/api/core";
  import { open } from "@tauri-apps/plugin-dialog";
  import { emulators, type EmulatorView } from "$lib/store";
  import { derived } from "svelte/store";

  let debugMsg = $state("");
  let savingPaths = $state(false);

  const current = derived(
    [emulators, page],
    ([$emulators, $page]) => $emulators.find((e) => e.id === $page.params.id),
  );

  let sourceDraft = $state("");
  let destDraft = $state("");
  let lastSeenId = "";

  $effect(() => {
    const emu = $current;
    if (!emu) return;
    if (emu.id !== lastSeenId) {
      sourceDraft = emu.source_path;
      destDraft = emu.dest_path;
      lastSeenId = emu.id;
    }
  });

  async function pickFolder(target: "source" | "dest") {
    debugMsg = "";
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") {
        if (target === "source") sourceDraft = selected;
        else destDraft = selected;
      }
    } catch (err) {
      debugMsg = "pickFolder: " + String(err);
    }
  }

  async function savePaths(emu: EmulatorView) {
    debugMsg = "";
    savingPaths = true;
    try {
      await invoke("set_emulator_paths", {
        id: emu.id,
        sourcePath: sourceDraft,
        destPath: destDraft,
      });
    } catch (err) {
      debugMsg = "set_emulator_paths: " + String(err);
    } finally {
      savingPaths = false;
    }
  }

  async function syncNow(emu: EmulatorView) {
    debugMsg = "";
    try {
      await invoke("sync_now", { id: emu.id });
    } catch (err) {
      debugMsg = "sync_now: " + String(err);
    }
  }

  async function toggleWatch(emu: EmulatorView) {
    debugMsg = "";
    try {
      if (emu.watching) {
        await invoke("stop_watch", { id: emu.id });
      } else {
        await invoke("start_watch", { id: emu.id });
      }
    } catch (err) {
      debugMsg = "watch: " + String(err);
    }
  }

  async function toggleEnabled(emu: EmulatorView) {
    debugMsg = "";
    try {
      await invoke("set_enabled", { id: emu.id, enabled: !emu.enabled });
    } catch (err) {
      debugMsg = "set_enabled: " + String(err);
    }
  }

  function back() {
    goto("/");
  }

  function pathDirty(emu: EmulatorView) {
    return sourceDraft !== emu.source_path || destDraft !== emu.dest_path;
  }
</script>

<section class="topnav">
  <button class="back" onclick={back} aria-label="voltar">
    <span class="back-arrow">◀</span> back to index
  </button>
</section>

{#if !$current}
  <section class="empty">
    <p>// unit not found · <button class="link" onclick={back}>return</button></p>
  </section>
{:else}
  {@const emu = $current}

  <section class="head">
    <div class="head-row">
      <span class="led" class:led-green={emu.watching} class:led-amber={emu.enabled && !emu.watching} class:led-off={!emu.enabled}></span>
      <h1>{emu.name}</h1>
      <span class="state-tag">
        {#if !emu.enabled}
          // disabled
        {:else if emu.watching}
          // watching
        {:else}
          // idle
        {/if}
      </span>
    </div>
    <p class="head-id">unit_id :: {emu.id}</p>
  </section>

  {#if debugMsg}
    <section class="alert">
      <span class="alert-tag">! TRACE</span>
      <span>{debugMsg}</span>
    </section>
  {/if}

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ hint ]</span>
      <span class="card-meta">where to look</span>
    </header>
    <p class="hint">{emu.hint}</p>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ paths ]</span>
      <span class="card-meta">source &rarr; destination</span>
    </header>

    <div class="field">
      <label class="field-label" for="source-path">source / live save dir</label>
      <div class="field-row">
        <input
          id="source-path"
          type="text"
          class="field-input"
          bind:value={sourceDraft}
          placeholder="C:\path\to\emulator\saves"
          disabled={!emu.enabled}
        />
        <button class="btn btn-thin" onclick={() => pickFolder("source")} disabled={!emu.enabled}>
          [ browse ]
        </button>
      </div>
    </div>

    <div class="field">
      <label class="field-label" for="dest-path">destination / mirror dir</label>
      <div class="field-row">
        <input
          id="dest-path"
          type="text"
          class="field-input"
          bind:value={destDraft}
          placeholder="C:\path\to\backup"
          disabled={!emu.enabled}
        />
        <button class="btn btn-thin" onclick={() => pickFolder("dest")} disabled={!emu.enabled}>
          [ browse ]
        </button>
      </div>
    </div>

    <div class="field-actions">
      <button
        class="btn"
        onclick={() => savePaths(emu)}
        disabled={!emu.enabled || savingPaths || !pathDirty(emu)}
      >
        {savingPaths ? "saving..." : pathDirty(emu) ? "[ commit paths ]" : "[ saved ]"}
      </button>
    </div>
  </section>

  <section class="card">
    <header class="card-head">
      <span class="card-tag">[ status ]</span>
      <span class="card-meta">last operation</span>
    </header>

    <div class="meta-row">
      <span class="meta-key">last_sync</span>
      <span class="meta-val" class:dim={!emu.last_sync}>
        {emu.last_sync ?? "never"}
      </span>
    </div>

    {#if emu.last_error}
      <div class="meta-row error">
        <span class="meta-key">last_error</span>
        <span class="meta-val err">{emu.last_error}</span>
      </div>
    {/if}
  </section>

  <section class="card ops">
    <header class="card-head">
      <span class="card-tag">[ ops ]</span>
      <span class="card-meta">control surface</span>
    </header>

    <div class="ops-row">
      <button
        class="btn btn-power"
        class:on={emu.enabled}
        onclick={() => toggleEnabled(emu)}
      >
        {emu.enabled ? "[ disable unit ]" : "[ enable unit ]"}
      </button>

      <button
        class="btn"
        onclick={() => syncNow(emu)}
        disabled={!emu.enabled}
      >
        &#9654; sync now
      </button>

      <button
        class="btn btn-watch"
        class:active={emu.watching}
        onclick={() => toggleWatch(emu)}
        disabled={!emu.enabled}
      >
        {emu.watching ? "&#9632; halt watcher" : "&#9678; engage watcher"}
      </button>
    </div>
  </section>
{/if}

<style>
  .topnav {
    margin: 1rem 0 0.6rem;
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
    text-transform: lowercase;
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

  .empty {
    margin-top: 2rem;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.85rem;
  }

  .link {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    font-family: inherit;
    font-size: inherit;
    text-decoration: underline;
    padding: 0;
  }

  .head {
    margin: 1rem 0 1.2rem;
  }

  .head-row {
    display: flex;
    align-items: center;
    gap: 0.7rem;
  }

  h1 {
    font-family: "Major Mono Display", monospace;
    font-size: 1.7rem;
    margin: 0;
    color: var(--text-bright);
    letter-spacing: 0.06em;
    text-transform: lowercase;
    text-shadow: var(--title-glow);
  }

  .state-tag {
    color: var(--text-muted);
    font-size: 0.72rem;
    font-style: italic;
    letter-spacing: 0.05em;
    margin-left: auto;
  }

  .head-id {
    margin: 0.4rem 0 0;
    font-size: 0.7rem;
    color: var(--text-faint);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .card {
    margin-top: 1.1rem;
    border: 1px solid var(--border);
    background: var(--bg-unit-1);
    padding: 0.85rem 1rem;
  }

  .card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    border-bottom: 1px dashed var(--border);
    padding-bottom: 0.4rem;
    margin-bottom: 0.7rem;
  }

  .card-tag {
    color: var(--accent);
    font-size: 0.74rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .card-meta {
    color: var(--text-muted);
    font-size: 0.68rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .hint {
    margin: 0.2rem 0 0;
    font-size: 0.78rem;
    color: var(--text-soft);
    line-height: 1.5;
    white-space: pre-wrap;
    background: var(--bg-hint);
    padding: 0.55rem 0.7rem;
    border-left: 2px solid var(--border-strong);
  }

  .field {
    margin-bottom: 0.8rem;
  }

  .field:last-of-type {
    margin-bottom: 0;
  }

  .field-label {
    display: block;
    font-size: 0.68rem;
    color: var(--text-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-bottom: 0.3rem;
  }

  .field-row {
    display: flex;
    gap: 0.5rem;
    align-items: stretch;
  }

  .field-input {
    flex: 1;
    background: var(--bg-input);
    border: 1px solid var(--border-strong);
    color: var(--text-bright);
    font-family: inherit;
    font-size: 0.78rem;
    padding: 0.45rem 0.6rem;
    letter-spacing: 0.02em;
    min-width: 0;
  }

  .field-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .field-input:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .field-actions {
    margin-top: 0.85rem;
    display: flex;
    justify-content: flex-end;
  }

  .btn {
    background: transparent;
    border: 1px solid var(--border-strong);
    color: var(--text-soft);
    font-family: inherit;
    font-size: 0.75rem;
    padding: 0.45rem 0.8rem;
    cursor: pointer;
    letter-spacing: 0.05em;
    transition: all 0.14s;
    text-align: center;
  }

  .btn:hover:not(:disabled) {
    color: var(--text-bright);
    border-color: var(--text-soft);
    background: var(--hover-tint);
  }

  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .btn-thin {
    flex-shrink: 0;
    padding: 0.45rem 0.7rem;
    font-size: 0.72rem;
  }

  .btn-power.on {
    color: var(--success);
    border-color: var(--success-border);
  }

  .btn-power.on:hover {
    color: var(--success-bright);
    background: var(--success-glow-bg);
  }

  .btn-watch.active {
    color: var(--success);
    border-color: var(--success-border);
  }

  .btn-watch.active:hover {
    color: var(--success-bright);
    background: var(--success-glow-bg);
  }

  .ops .ops-row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.55rem;
  }

  .ops .ops-row .btn {
    flex: 1;
    min-width: 130px;
  }

  .meta-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.35rem 0;
    font-size: 0.78rem;
    border-bottom: 1px dotted var(--border);
  }

  .meta-row:last-child {
    border-bottom: none;
  }

  .meta-key {
    color: var(--text-muted);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    font-size: 0.7rem;
  }

  .meta-val {
    color: var(--text-bright);
    font-variant-numeric: tabular-nums;
  }

  .meta-val.dim {
    color: var(--text-faint);
    font-style: italic;
  }

  .meta-row.error {
    background: var(--error-bg);
    margin: 0.3rem -1rem -0.3rem;
    padding: 0.45rem 1rem;
  }

  .meta-val.err {
    color: var(--error-text);
    font-size: 0.74rem;
    text-align: right;
    word-break: break-word;
  }
</style>
