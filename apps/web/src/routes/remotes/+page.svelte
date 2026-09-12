<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { _ } from "svelte-i18n";
  import { tErr } from "$lib/i18n";

  /**
   * Provider presets follow rclone's S3 backend matrix
   * (https://rclone.org/s3/). For "AWS", endpoint stays empty so rclone
   * derives it from the region. Everyone else needs an explicit endpoint.
   *
   * `labelKey` / `hintKey` resolve through i18n at render time so the form
   * follows the user's locale.
   */
  type Preset = {
    id: string;
    labelKey: string;
    provider: string;
    endpoint: string;
    region: string;
    hintKey: string;
  };

  const PRESETS: Preset[] = [
    { id: "aws",    labelKey: "remotes.presets.aws_label",    hintKey: "remotes.presets.aws_hint",    provider: "AWS",          endpoint: "",                                                          region: "us-east-1" },
    { id: "r2",     labelKey: "remotes.presets.r2_label",     hintKey: "remotes.presets.r2_hint",     provider: "Cloudflare",   endpoint: "https://<account-id>.r2.cloudflarestorage.com",             region: "auto" },
    { id: "b2",     labelKey: "remotes.presets.b2_label",     hintKey: "remotes.presets.b2_hint",     provider: "Other",        endpoint: "https://s3.us-west-002.backblazeb2.com",                    region: "us-west-002" },
    { id: "wasabi", labelKey: "remotes.presets.wasabi_label", hintKey: "remotes.presets.wasabi_hint", provider: "Wasabi",       endpoint: "https://s3.us-east-1.wasabisys.com",                        region: "us-east-1" },
    { id: "minio",  labelKey: "remotes.presets.minio_label",  hintKey: "remotes.presets.minio_hint",  provider: "Minio",        endpoint: "http://localhost:9000",                                     region: "us-east-1" },
    { id: "do",     labelKey: "remotes.presets.do_label",     hintKey: "remotes.presets.do_hint",     provider: "DigitalOcean", endpoint: "https://nyc3.digitaloceanspaces.com",                       region: "us-east-1" },
    { id: "other",  labelKey: "remotes.presets.other_label",  hintKey: "remotes.presets.other_hint",  provider: "Other",        endpoint: "",                                                          region: "us-east-1" },
  ];

  let remotes = $state<string[]>([]);
  let loading = $state(true);
  let listErr = $state("");

  let presetId = $state(PRESETS[0].id);
  const preset = $derived(PRESETS.find((p) => p.id === presetId)!);

  let name = $state("");
  let accessKeyId = $state("");
  let secretAccessKey = $state("");
  let endpoint = $state(PRESETS[0].endpoint);
  let region = $state(PRESETS[0].region);

  let creating = $state(false);
  let createMsg = $state("");
  let createErr = $state("");

  let testing = $state<string | null>(null);
  let testMsg = $state<{ name: string; ok: boolean; text: string } | null>(null);
  let deleting = $state<string | null>(null);

  $effect(() => {
    // Each preset change snaps the editable fields to its template — but
    // only when the user actually picks a different preset, so values
    // they typed don't get clobbered while they fill the form.
    endpoint = preset.endpoint;
    region = preset.region;
  });

  async function loadRemotes() {
    listErr = "";
    loading = true;
    try {
      remotes = await invoke<string[]>("rclone_list_remotes");
    } catch (e) {
      listErr = tErr(e);
    } finally {
      loading = false;
    }
  }

  function nameValid(n: string) {
    return /^[a-z0-9_-]{1,32}$/i.test(n);
  }

  async function createRemote() {
    createErr = "";
    createMsg = "";
    if (!nameValid(name)) {
      createErr = $_("remotes.name_invalid");
      return;
    }
    if (!accessKeyId || !secretAccessKey) {
      createErr = $_("remotes.creds_required");
      return;
    }
    creating = true;
    try {
      await invoke("rclone_create_s3_remote", {
        config: {
          name,
          provider: preset.provider,
          accessKeyId,
          secretAccessKey,
          endpoint: endpoint || null,
          region: region || null,
        },
      });
      createMsg = $_("remotes.create_ok", { values: { name } });
      // wipe sensitive fields once they've made it into rclone (it stores
      // the secret obscured — we never want plaintext lingering in memory)
      accessKeyId = "";
      secretAccessKey = "";
      name = "";
      await loadRemotes();
    } catch (e) {
      createErr = tErr(e);
    } finally {
      creating = false;
    }
  }

  async function testRemote(remoteName: string) {
    const clean = remoteName.replace(/:$/, "");
    testing = clean;
    testMsg = null;
    try {
      await invoke("rclone_test_remote", { name: clean, path: "" });
      testMsg = { name: clean, ok: true, text: $_("remotes.test_ok") };
    } catch (e) {
      testMsg = { name: clean, ok: false, text: tErr(e) };
    } finally {
      testing = null;
    }
  }

  async function deleteRemote(remoteName: string) {
    const clean = remoteName.replace(/:$/, "");
    if (!confirm($_("remotes.confirm_delete", { values: { name: clean } }))) return;
    deleting = clean;
    try {
      await invoke("rclone_delete_remote", { name: clean });
      if (testMsg?.name === clean) testMsg = null;
      await loadRemotes();
    } catch (e) {
      listErr = tErr(e);
    } finally {
      deleting = null;
    }
  }

  function back() {
    goto("/");
  }

  onMount(loadRemotes);
</script>

<section class="topnav">
  <button class="back" onclick={back} aria-label={$_("common.back")}>
    <span class="back-arrow">◀</span> {$_("common.back")}
  </button>
</section>

<section class="head">
  <div class="head-row">
    <span class="led led-amber"></span>
    <h1>{$_("remotes.title")}</h1>
    <span class="state-tag">{$_("remotes.subtitle")}</span>
  </div>
  <p class="head-id">{$_("remotes.module")}</p>
</section>

<section class="card">
  <header class="card-head">
    <span class="card-tag">{$_("remotes.active_tag")}</span>
    <span class="card-meta">{$_("remotes.active_count", { values: { n: remotes.length } })}</span>
  </header>

  {#if loading}
    <p class="empty-msg">// {$_("common.loading")}…</p>
  {:else if listErr}
    <p class="error-line">! {listErr}</p>
  {:else if remotes.length === 0}
    <p class="empty-msg">{$_("remotes.empty")}</p>
  {:else}
    <ul class="remote-list">
      {#each remotes as r (r)}
        {@const clean = r.replace(/:$/, "")}
        <li class="remote-row">
          <span class="remote-name">{clean}</span>
          <div class="remote-actions">
            <button
              class="btn btn-thin"
              onclick={() => testRemote(r)}
              disabled={testing === clean}
            >
              {testing === clean ? "…" : $_("remotes.test_btn")}
            </button>
            <button
              class="btn btn-thin btn-danger"
              onclick={() => deleteRemote(r)}
              disabled={deleting === clean}
            >
              {deleting === clean ? "…" : $_("remotes.delete_btn")}
            </button>
          </div>
        </li>
        {#if testMsg?.name === clean}
          <li class="test-out" class:ok={testMsg.ok} class:err={!testMsg.ok}>
            {testMsg.text}
          </li>
        {/if}
      {/each}
    </ul>
  {/if}
</section>

<section class="card">
  <header class="card-head">
    <span class="card-tag">{$_("remotes.new_tag")}</span>
    <span class="card-meta">{$_("remotes.new_subtitle")}</span>
  </header>

  <div class="field">
    <label class="field-label" for="preset">{$_("remotes.field_preset")}</label>
    <select id="preset" class="field-input" bind:value={presetId}>
      {#each PRESETS as p (p.id)}
        <option value={p.id}>{$_(p.labelKey)}</option>
      {/each}
    </select>
    <p class="hint-line">// {$_(preset.hintKey)}</p>
  </div>

  <div class="field">
    <label class="field-label" for="rname">{$_("remotes.field_name")}</label>
    <input
      id="rname"
      type="text"
      class="field-input"
      bind:value={name}
      placeholder="ex: s3backup"
      autocomplete="off"
      spellcheck="false"
    />
  </div>

  <div class="field">
    <label class="field-label" for="ak">{$_("remotes.field_access_key")}</label>
    <input
      id="ak"
      type="text"
      class="field-input mono"
      bind:value={accessKeyId}
      autocomplete="off"
      spellcheck="false"
    />
  </div>

  <div class="field">
    <label class="field-label" for="sk">{$_("remotes.field_secret_key")}</label>
    <input
      id="sk"
      type="password"
      class="field-input mono"
      bind:value={secretAccessKey}
      autocomplete="off"
      spellcheck="false"
    />
  </div>

  <div class="field-grid">
    <div class="field">
      <label class="field-label" for="endp">{$_("remotes.field_endpoint")}</label>
      <input
        id="endp"
        type="text"
        class="field-input mono"
        bind:value={endpoint}
        placeholder={preset.id === "aws" ? $_("remotes.endpoint_aws_placeholder") : "https://…"}
        autocomplete="off"
        spellcheck="false"
      />
    </div>
    <div class="field">
      <label class="field-label" for="reg">{$_("remotes.field_region")}</label>
      <input
        id="reg"
        type="text"
        class="field-input mono"
        bind:value={region}
        autocomplete="off"
        spellcheck="false"
      />
    </div>
  </div>

  {#if createErr}
    <div class="alert">
      <span class="alert-tag">{$_("common.error_tag")}</span>
      <span>{createErr}</span>
    </div>
  {/if}

  {#if createMsg}
    <div class="ok-line">{createMsg}</div>
  {/if}

  <div class="field-actions">
    <button class="btn" onclick={createRemote} disabled={creating}>
      {creating ? $_("remotes.creating") : $_("remotes.create_btn")}
    </button>
  </div>
</section>

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

  .empty-msg {
    color: var(--text-muted);
    font-style: italic;
    font-size: 0.78rem;
    margin: 0.3rem 0;
  }

  .error-line {
    color: var(--error-text, #e05c5c);
    font-size: 0.78rem;
    margin: 0;
  }

  .ok-line {
    color: var(--success);
    font-size: 0.76rem;
    margin: 0.4rem 0;
  }

  .remote-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .remote-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0;
    border-bottom: 1px dotted var(--border);
    gap: 0.7rem;
  }

  .remote-row:last-child {
    border-bottom: none;
  }

  .remote-name {
    font-family: "Major Mono Display", monospace;
    color: var(--text-bright);
    font-size: 0.9rem;
    letter-spacing: 0.04em;
  }

  .remote-actions {
    display: flex;
    gap: 0.4rem;
  }

  .test-out {
    list-style: none;
    padding: 0.35rem 0.55rem;
    margin: 0 0 0.35rem;
    font-size: 0.74rem;
    border-left: 2px solid var(--border-strong);
    background: var(--bg-hint);
    color: var(--text-soft);
    word-break: break-word;
  }

  .test-out.ok {
    color: var(--success);
    border-left-color: var(--success-border);
  }

  .test-out.err {
    color: var(--error-text, #e05c5c);
    border-left-color: var(--error-text, #e05c5c);
  }

  .field {
    margin-bottom: 0.8rem;
  }

  .field:last-of-type {
    margin-bottom: 0;
  }

  .field-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.7rem;
  }

  @media (max-width: 540px) {
    .field-grid {
      grid-template-columns: 1fr;
    }
  }

  .field-label {
    display: block;
    font-size: 0.68rem;
    color: var(--text-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-bottom: 0.3rem;
  }

  .field-input {
    width: 100%;
    box-sizing: border-box;
    background: var(--bg-input);
    border: 1px solid var(--border-strong);
    color: var(--text-bright);
    font-family: inherit;
    font-size: 0.78rem;
    padding: 0.45rem 0.6rem;
    letter-spacing: 0.02em;
  }

  .field-input.mono {
    font-family: "Major Mono Display", monospace;
    font-size: 0.74rem;
  }

  .field-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .hint-line {
    margin: 0.25rem 0 0;
    font-size: 0.68rem;
    color: var(--text-faint);
    font-style: italic;
    letter-spacing: 0.04em;
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
    white-space: nowrap;
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
    padding: 0.35rem 0.6rem;
    font-size: 0.72rem;
  }

  .btn-danger {
    color: var(--error-text, #e05c5c);
    border-color: var(--error-text, #e05c5c);
    opacity: 0.75;
  }

  .btn-danger:hover:not(:disabled) {
    opacity: 1;
    background: var(--hover-tint);
  }

  .alert {
    margin: 0.5rem 0;
    padding: 0.45rem 0.6rem;
    background: var(--error-bg, rgba(224, 92, 92, 0.1));
    border-left: 2px solid var(--error-text, #e05c5c);
    color: var(--error-text, #e05c5c);
    font-size: 0.74rem;
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
  }

  .alert-tag {
    flex-shrink: 0;
    letter-spacing: 0.06em;
  }
</style>
