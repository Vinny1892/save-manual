<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { emulators, type EmulatorView } from "$lib/store";

  let debugMsg = $state("");

  function fmtIndex(i: number) {
    return String(i + 1).padStart(2, "0");
  }

  async function toggleEnabled(e: Event, emu: EmulatorView) {
    e.stopPropagation();
    try {
      await invoke("set_enabled", { id: emu.id, enabled: !emu.enabled });
    } catch (err) {
      debugMsg = `set_enabled ${emu.id}: ` + String(err);
    }
  }

  function openDetail(emu: EmulatorView) {
    goto(`/emulator/${emu.id}`);
  }
</script>

<section class="banner">
  <h1>SAVE&middot;SYNC</h1>
  <p class="banner-sub">
    ┄┄ emulator state replication terminal ┄┄ <span class="cursor">█</span>
  </p>
</section>

{#if debugMsg}
  <section class="alert">
    <span class="alert-tag">! TRACE</span>
    <span>{debugMsg}</span>
  </section>
{/if}

<div class="list-head">
  <span class="col-idx">no.</span>
  <span class="col-name">unit</span>
  <span class="col-state">state</span>
  <span class="col-sync">last_sync</span>
  <span class="col-power">power</span>
</div>

<ul class="rows">
  {#each $emulators as emu, i (emu.id)}
    <li
      class="row"
      class:disabled={!emu.enabled}
      style="--i: {i}"
      onclick={() => openDetail(emu)}
      role="button"
      tabindex="0"
      onkeydown={(e) => e.key === "Enter" && openDetail(emu)}
    >
      <span class="idx">{fmtIndex(i)}</span>
      <div class="name-block">
        <span class="led" class:led-green={emu.watching} class:led-amber={emu.enabled && !emu.watching} class:led-off={!emu.enabled}></span>
        <span class="name">{emu.name}</span>
      </div>
      <span class="state-tag">
        {#if !emu.enabled}
          // disabled
        {:else if emu.watching}
          // watching
        {:else}
          // idle
        {/if}
      </span>
      <span class="sync-val" class:dim={!emu.last_sync}>
        {emu.last_sync ?? "never"}
      </span>
      <button
        class="power-btn"
        class:on={emu.enabled}
        onclick={(e) => toggleEnabled(e, emu)}
        aria-label={emu.enabled ? "desativar" : "ativar"}
      >
        {emu.enabled ? "[ on ]" : "[ off ]"}
      </button>
    </li>
  {/each}
</ul>

<style>
  .banner {
    margin: 1.8rem 0 1.5rem;
    text-align: center;
  }

  h1 {
    font-family: "Major Mono Display", monospace;
    font-size: 2.6rem;
    margin: 0;
    letter-spacing: 0.12em;
    color: var(--text-bright);
    text-shadow: var(--title-glow);
    line-height: 1;
  }

  .banner-sub {
    font-size: 0.72rem;
    color: var(--text-muted);
    letter-spacing: 0.18em;
    text-transform: uppercase;
    margin: 0.55rem 0 0;
  }

  .cursor {
    display: inline-block;
    color: var(--text-bright);
    animation: blink 1.05s steps(1) infinite;
    margin-left: 0.15em;
  }

  .list-head {
    display: grid;
    grid-template-columns: 36px 1fr 110px 170px 70px;
    gap: 0.7rem;
    align-items: center;
    padding: 0.45rem 0.85rem;
    color: var(--text-muted);
    font-size: 0.66rem;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    border-bottom: 1px dashed var(--border);
  }

  .col-state, .col-sync, .col-power {
    text-align: left;
  }

  .col-power {
    text-align: center;
  }

  .rows {
    list-style: none;
    padding: 0;
    margin: 0.4rem 0 0;
    display: flex;
    flex-direction: column;
  }

  .row {
    display: grid;
    grid-template-columns: 36px 1fr 110px 170px 70px;
    gap: 0.7rem;
    align-items: center;
    padding: 0.7rem 0.85rem;
    border: 1px solid transparent;
    border-bottom: 1px dashed var(--border);
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
    opacity: 0;
    transform: translateY(4px);
    animation: reveal 0.35s ease-out forwards;
    animation-delay: calc(var(--i) * 70ms + 100ms);
  }

  @keyframes reveal {
    to { opacity: 1; transform: translateY(0); }
  }

  .row:hover {
    background: var(--hover-tint);
    border-color: var(--border-strong);
    border-bottom-style: solid;
  }

  .row.disabled .name,
  .row.disabled .idx {
    color: var(--text-faint);
  }

  .row.disabled .state-tag,
  .row.disabled .sync-val {
    opacity: 0.5;
  }

  .idx {
    font-family: "Major Mono Display", monospace;
    color: var(--text-muted);
    font-size: 0.95rem;
  }

  .name-block {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    min-width: 0;
  }

  .name {
    font-family: "Major Mono Display", monospace;
    font-size: 0.92rem;
    color: var(--text-bright);
    letter-spacing: 0.04em;
    text-transform: lowercase;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .state-tag {
    color: var(--text-muted);
    font-size: 0.72rem;
    font-style: italic;
    letter-spacing: 0.05em;
  }

  .sync-val {
    color: var(--text);
    font-size: 0.74rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .sync-val.dim {
    color: var(--text-faint);
    font-style: italic;
  }

  .power-btn {
    background: transparent;
    border: 1px solid var(--border-strong);
    color: var(--text-muted);
    font-family: inherit;
    font-size: 0.72rem;
    letter-spacing: 0.05em;
    padding: 0.3rem 0.5rem;
    cursor: pointer;
    transition: all 0.14s;
    text-align: center;
  }

  .power-btn:hover {
    border-color: var(--text-soft);
    color: var(--text-bright);
  }

  .power-btn.on {
    color: var(--success);
    border-color: var(--success-border);
  }

  .power-btn.on:hover {
    color: var(--success-bright);
    border-color: var(--success);
    background: var(--success-glow-bg);
  }
</style>
