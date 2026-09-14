<script lang="ts">
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { invoke, isTauri, setServerBase, setDeviceToken } from "$lib/rpc";
  import { tErr } from "$lib/i18n";
  import { _ } from "svelte-i18n";

  /**
   * Pareamento com o server do NAS.
   *
   * Só existe no client de PC: no browser a página já *é* o server, e
   * parear a si mesmo não faz sentido.
   */
  const native = isTauri();

  interface ServerStatus {
    url: string;
    paired: boolean;
  }

  let status = $state<ServerStatus | null>(null);
  let url = $state("");
  let code = $state("");
  let busy = $state(false);
  let err = $state("");
  let ok = $state("");

  async function load() {
    if (!native) return;
    try {
      status = await invoke<ServerStatus>("server_status");
      if (status.url) url = status.url;
    } catch (e) {
      err = tErr(e);
    }
  }

  onMount(load);

  function normalize(raw: string): string {
    const trimmed = raw.trim().replace(/\/+$/, "");
    // Sem esquema, assume http: o caso comum é um IP de LAN, e digitar
    // "192.168.0.10:8787" é o que a pessoa faz naturalmente.
    return /^https?:\/\//.test(trimmed) ? trimmed : `http://${trimmed}`;
  }

  async function pair(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    busy = true;
    err = "";
    ok = "";
    try {
      const base = normalize(url);
      const paired = await invoke<{ device_token: string }>("pair_with_server", {
        url: base,
        code: code.trim(),
      });
      // O transporte da UI também passa a falar com esse server. O token
      // fica no SQLite do client; aqui é o espelho que o fetch usa.
      setServerBase(base);
      setDeviceToken(paired.device_token);
      ok = $_("server.paired_ok");
      code = "";
      await load();
    } catch (e) {
      err = tErr(e);
    } finally {
      busy = false;
    }
  }

  async function unpair() {
    busy = true;
    err = "";
    ok = "";
    try {
      await invoke("unpair_server");
      setDeviceToken("");
      await load();
    } catch (e) {
      err = tErr(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="topnav">
  <button class="back" onclick={() => goto("/")} aria-label={$_("common.back")}>
    <span class="back-arrow">◀</span> {$_("common.back")}
  </button>
</section>

{#if !native}
  <section class="card">
    <span class="tag">[ server ]</span>
    <p class="note">{$_("server.browser_note")}</p>
  </section>
{:else}
  <section class="card">
    <span class="tag">[ server ]</span>

    {#if status?.paired}
      <dl class="kv">
        <dt>{$_("server.url_label")}</dt>
        <dd>{status.url}</dd>
        <dt>{$_("server.state_label")}</dt>
        <dd class="ok">{$_("server.state_paired")}</dd>
      </dl>
      <button class="btn danger" onclick={unpair} disabled={busy}>
        {$_("server.unpair_btn")}
      </button>
      <p class="note">{$_("server.unpair_note")}</p>
    {:else}
      <p class="note">{$_("server.pair_intro")}</p>

      <form onsubmit={pair}>
        <label class="field">
          <span class="tag-sm">{$_("server.url_label")}</span>
          <input bind:value={url} placeholder="192.168.0.10:8787" required />
        </label>

        <label class="field">
          <span class="tag-sm">{$_("server.code_label")}</span>
          <input
            bind:value={code}
            placeholder="ABCD2345"
            autocapitalize="characters"
            autocorrect="off"
            spellcheck="false"
            required
          />
        </label>

        <button class="btn" type="submit" disabled={busy}>
          {busy ? $_("server.pairing") : $_("server.pair_btn")}
        </button>
      </form>

      <p class="note dim">{$_("server.code_hint")}</p>
    {/if}

    {#if err}<p class="err" role="alert">{err}</p>{/if}
    {#if ok}<p class="ok" role="status">{ok}</p>{/if}
  </section>
{/if}

<style>
  .topnav {
    padding: 12px 0;
  }

  .back {
    background: none;
    border: none;
    color: var(--text-dim);
    font: inherit;
    cursor: pointer;
  }

  .back:hover {
    color: var(--accent);
  }

  .card {
    border: 1px solid var(--border);
    background: var(--bg-elev);
    padding: 18px;
    margin-bottom: 16px;
    max-width: 520px;
  }

  .tag {
    color: var(--accent);
    letter-spacing: 0.1em;
    font-size: 0.8rem;
    display: block;
    margin-bottom: 14px;
  }

  .tag-sm {
    font-size: 0.7rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-bottom: 12px;
  }

  input {
    background: var(--bg);
    border: 1px solid var(--border);
    color: var(--text);
    padding: 8px 10px;
    font: inherit;
    width: 100%;
    box-sizing: border-box;
  }

  input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn {
    background: transparent;
    border: 1px solid var(--accent);
    color: var(--accent);
    padding: 8px 16px;
    font: inherit;
    cursor: pointer;
    letter-spacing: 0.06em;
  }

  .btn:hover:not(:disabled) {
    background: var(--accent);
    color: var(--bg);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn.danger {
    border-color: var(--err, #f87171);
    color: var(--err, #f87171);
  }

  .btn.danger:hover:not(:disabled) {
    background: var(--err, #f87171);
    color: var(--bg);
  }

  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 14px;
    margin: 0 0 16px;
    font-size: 0.82rem;
  }

  .kv dt {
    color: var(--text-dim);
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.08em;
  }

  .kv dd {
    margin: 0;
    word-break: break-all;
  }

  .note {
    color: var(--text-dim);
    font-size: 0.78rem;
    line-height: 1.55;
    margin: 0 0 14px;
  }

  .note.dim {
    margin: 14px 0 0;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }

  .err {
    color: var(--err, #f87171);
    font-size: 0.8rem;
    margin: 12px 0 0;
  }

  .ok {
    color: var(--ok, #4ade80);
    font-size: 0.8rem;
    margin: 12px 0 0;
  }
</style>
