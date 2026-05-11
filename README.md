# save-sync

Replicação de save data entre instalações de emuladores (eden / rpcs3 / pcsx2),
com destino opcional em **qualquer remote do rclone** (S3, R2, B2, MinIO, etc)
embarcado in-process via librclone.

Tauri 2 + SvelteKit + Rust. UI estética CRT/terminal com 3 temas (dark / light / blue).

---

## Quick start

Pré-requisitos:

- Node.js 20+
- Rust stable (com toolchain `x86_64-pc-windows-msvc` no Windows)
- Para construir `librclone` localmente: Go 1.21+, gcc (MinGW-w64 no Windows), Git

```bash
# 1. dependências do frontend
npm install

# 2. build librclone (~2-3 min, primeira vez clona ~30 MB do rclone)
#    Windows:
.\scripts\build-librclone.ps1
#    Linux/macOS:
bash scripts/build-librclone.sh

# 3. dev mode
npm run tauri dev
```

A primeira execução do app baixa duas bases de dados em background (~93 MB
total, em `%APPDATA%\com.savesync.app\`):

- `titledb.json` (~83 MB) — base do blawar/titledb pra resolver title-id de Switch → nome
- `ps2-gameindex.yaml` (~10 MB) — `GameIndex.yaml` do PCSX2 pra resolver serial PS2 → nome

Até elas terminarem de carregar, saves de eden/pcsx2 mostram o ID bruto em vez do nome legível.

---

## Build deps no Windows (one-time setup)

```powershell
winget install -e --id GoLang.Go
winget install -e --id BrechtSanders.WinLibs.POSIX.UCRT.LLVM
winget install -e --id Git.Git
```

Fechar e reabrir o PowerShell antes de rodar o build script (pra pegar o PATH atualizado).

Se o `winget` não estiver no PATH:

```powershell
$add = "$env:LOCALAPPDATA\Microsoft\WindowsApps"
$cur = [Environment]::GetEnvironmentVariable("Path", "User")
if ($cur -notlike "*$add*") {
    [Environment]::SetEnvironmentVariable("Path", "$add;$cur", "User")
}
```

---

## Estrutura

```
save-sync/
├── icon.svg                       # ícone do app (cartucho âmbar + LED verde)
├── scripts/
│   ├── build-librclone.ps1        # build local Windows
│   └── build-librclone.sh         # build CI Linux/macOS
├── src/                           # frontend SvelteKit
│   ├── app.css                    # tokens CSS por tema
│   ├── app.html
│   ├── lib/
│   │   ├── store.ts               # store Svelte do estado dos emuladores
│   │   └── theme.ts               # toggle dark/light/blue
│   └── routes/
│       ├── +layout.svelte         # title bar sticky + listener emulator-changed
│       ├── +page.svelte           # listagem de unidades (home)
│       └── emulator/[id]/
│           ├── +page.svelte       # detalhe da unit
│           └── saves/
│               ├── +page.svelte   # lista de saves (grid/list, eden+rpcs3+pcsx2)
│               └── [raw_id]/
│                   ├── +page.svelte             # detalhe de save (eden/rpcs3)
│                   └── games/
│                       ├── +page.svelte         # saves dentro do memcard PS2
│                       └── [save_name]/+page.svelte  # detalhe de save PS2 (read-only)
└── src-tauri/
    ├── build.rs                   # tauri-build + stage da DLL do librclone
    ├── icons/                     # gerados por `tauri icon`
    ├── lib/<triple>/              # librclone artifacts (gitignored)
    │   ├── librclone.dll/.so/.dylib
    │   └── librclone.h
    └── src/
        ├── lib.rs                 # entry-point: setup, AppState, comandos Tauri
        ├── db.rs                  # SQLite (rusqlite) — schema dos emuladores
        ├── detect.rs              # auto-detecção de paths por emulador
        ├── saves.rs               # listagem/sync de saves (eden/rpcs3/pcsx2)
        ├── ps2mc.rs               # parser de .ps2 (memcard PS2, ECC + FAT)
        ├── ps2db.rs               # download/parse PCSX2 GameIndex.yaml
        ├── titledb.rs             # download/parse blawar US.en.json (Switch)
        ├── sync.rs                # filesystem watcher + bulk sync (eden custom)
        ├── backend.rs             # enum Backend (Local | Rclone) — abstração de destino
        └── rclone.rs              # FFI dynamic load do librclone + helpers S3
```

---

## Arquitetura por componente

### Estado persistente

| Local | Conteúdo |
|---|---|
| `%APPDATA%\com.savesync.app\save-sync.db` | SQLite — emuladores, paths, settings |
| `%APPDATA%\com.savesync.app\titledb.json` | cache do blawar (Switch title-id → nome) |
| `%APPDATA%\com.savesync.app\ps2-gameindex.yaml` | cache do PCSX2 GameIndex (PS2 serial → nome) |
| `%APPDATA%\rclone\rclone.conf` | rclone-managed — remotes (creds ofuscadas) |
| `localStorage["save-sync-theme"]` | tema selecionado (dark/light/blue) |

### Emuladores suportados

Cada um tem um row na tabela `emulators` (id, name, hint, source_path, dest_path, enabled, last_sync, last_error, process_name).

| ID | Plataforma | source_path esperado | Estrutura interna |
|---|---|---|---|
| `eden` | Switch | `<eden>/user/nand` | `user/save/0000000000000000/<uuid 32-hex>/<title-id 16-hex>/` |
| `rpcs3` | PS3 | `<rpcs3>/dev_hdd0` | `home/<user>/savedata/<save-id>/PARAM.SFO` |
| `pcsx2` | PS2 | `<pcsx2>/memcards` | arquivos `.ps2` (8 MB com ECC, formato Sony) |

#### Eden (Switch)

- Estrutura real do NAND: o nível 1 sob `user/save/` é o `save_data_id` (geralmente `0000000000000000`); o nível 2 é o UUID do perfil; o nível 3 é o title-id.
- `list_eden` em `saves.rs` itera UUIDs, depois title-ids, dedupa por title-id (caso múltiplos perfis tenham save do mesmo jogo).
- Title resolution: lookup em `titledb.rs` map. Cache miss cai em `nlib.cc` per-save em `get_save_entry`.

#### RPCS3 (PS3)

- `list_rpcs3` em `saves.rs` lê `PARAM.SFO` de cada save; o nome do jogo vem do campo `TITLE` do SFO (parsing custom em `read_sfo_title`).
- Sem dependência de DB externa.

#### PCSX2 (PS2)

- **Read-only mode**: a UI não expõe sync nem delete individual. Memcard inteiro é a unidade de backup.
- `list_pcsx2` em `saves.rs` lista arquivos `.ps2`, filtra os não-formatados (header zerado).
- Click num memcard → `/games` (rota nova). `list_memcard_saves` parseia o filesystem do `.ps2`:
  - `ps2mc.rs` detecta ECC pelo file size (8.25 MB = ECC, 8 MB = no-ECC) e strip dos 16 bytes de ECC por página antes de parsear.
  - SuperBlock @ offset 0 (`Sony PS2 Memory Card Format `).
  - FAT chain via indirect FAT clusters (`ifc_list[32]`).
  - Root dir entries: filtra `mode & 0x8020 == 0x8020` (exists + dir), descarta `.` / `..`.
  - Serial extraído por regex `S[A-Z]{3}-\d{5}` no nome da pasta — entradas sem serial (BADATA-SYSTEM e similares) são puladas.
- Title resolution: lookup em `ps2db.rs` map (carregado de `GameIndex.yaml`).

### Resolução de nome → capa

Pipeline comum aos 3 emuladores: backend retorna `title` resolvido (via DB específica do emulador), frontend chama `fetch_cover_url(title, kind?)` → SteamGridDB.

- `kind = "grid"` (default): `/api/v2/grids/game/<id>?dimensions=600x900&limit=1` — usado pelos cards (grid view) e pela tela de detalhe.
- `kind = "icon"`: `/api/v2/icons/game/<id>?limit=1` — usado pela list view (thumb 40×40 quadrada). Cache separado (`gridUrls` / `iconUrls`).

### Sync

Dois gatilhos independentes, configuráveis por emulador:

- **watcher** (`start_watch` / `notify` crate): observa mudanças no source_path. A cada evento, debounce 2s e chama `do_sync`.
- **proc-watch** (`start_proc_watch` / `sysinfo`): polla processos a cada 2s. Quando o `process_name` configurado transita de "running" pra "not running" (= emulador fechou), chama `do_sync`.

`do_sync(emu, source, history)` em `lib.rs`:

1. Resolve `Backend::for_emulator(emu)` — Local ou Rclone (ver [Backend trait](#backend-trait-local-vs-rclone)).
2. Se `history.bisync_initialized == false`: detecta qual lado tem dados e roda `rclone bisync --resync --resync-mode {path1|path2|newer}`. Marca `bisync_initialized = 1` ao sucesso.
3. Senão: se modo = `full`, tira um snapshot completo `live → .history/<ts>/` antes; se modo = `incremental`, passa `--backupdir2 = .history/<ts>` no próprio bisync.
4. Itera `sync_subtrees(emu.id)` — eden bisynca duas subtrees (`system/save/8000000000000010` e `user/save`); pcsx2/rpcs3 bisyncam a source inteira.

Sync individual de save (`sync_one`) em `saves.rs` continua one-way (push-only via `Backend::copy_dir_contents`/`copy_file`) — usado pelo botão `[ sync ]` por save no detail page. Não passa por bisync porque a granularidade é de um único save, não do dataset todo.

### Backup history

Cada emulador tem política de history independente, gravada em `history_settings`:

| Campo | Valores | Default |
|---|---|---|
| `enabled` | bool | true |
| `incremental_enabled` | bool | true (eden/rpcs3) / false (pcsx2, locked off) |
| `full_enabled` | bool | false (eden/rpcs3) / true (pcsx2, default) |
| `retention_days` | int | 30 |
| `retention_max_mb` | int | 500 |
| `bisync_initialized` | bool | false (auto-reset quando paths mudam) |

Os dois modos são **independentes** — pode ligar só um, só outro, ou os dois (storage 2x). Backend valida "pelo menos um quando enabled=true"; UI desabilita o último checkbox ligado pra impedir o estado inválido na origem.

**File-based emuladores** (pcsx2 hoje, duckstation futuro) só suportam `full` — a unidade de save é um arquivo binário (memcard), incremental degeneraria pra full mesmo. `db::supports_incremental_history(emu_id)` é a fonte da verdade dessa classificação; `set_history_settings` coage `incremental_enabled = false` no servidor mesmo se o cliente mandar `true`.

**Modo `incremental_enabled`** (rclone `--backupdir2`):
- Cada sync, só os arquivos overwritten/deletados no remote vão pra `.history/<ts>/delta/<sub>/`.
- Storage cresce por delta — ~10 MB/dia se 1-2 saves de eden mudam por dia.

**Modo `full_enabled`** (sync/copy antes do bisync):
- Snapshot completo do live root pra `.history/<ts>/full/` antes de cada sync.
- Server-side copy em S3-family (CopyObject) — sem bandwidth, mas storage cheio.

**Layout no remote** (importante — explica por que history não corrompe o sync):

```
remote:bucket/saves/
├── eden/                                ← LIVE (path2 do bisync)
│   ├── system/save/8000000000000010/
│   └── user/save/...
└── .history/                            ← OUT-OF-BAND (não sincronizado)
    └── eden/
        ├── 2026-05-08T19-45-12Z/        ← um sync run
        │   ├── full/                    ← se full_enabled
        │   │   ├── system/save/8000.../
        │   │   └── user/save/...
        │   └── delta/                   ← se incremental_enabled
        │       ├── system/save/8000.../ ← overwrites de bisync 1
        │       └── user/save/...        ← overwrites de bisync 2
        └── 2026-05-09T14-30-00Z/
            └── ...
```

Por que os subdirs `full/` e `delta/`: quando os dois modos estão on no mesmo run, eles compartilham o mesmo `<ts>` mas precisam de paths separados senão o snapshot full sobrescreveria o que o `--backupdir2` salvou. Subdirs garantem isolamento.

A `.history/` é **sibling** do `<emu_id>/`, não nested dentro dele. Se estivesse nested, bisync tentaria mirroring de volta pra source e bagunçaria tudo. `Backend::history_root_fs()` garante essa invariante; testes unitários cobrem (`rclone_history_root_is_sibling_of_live`, `snapshot_full_and_delta_never_collide`).

### Backend trait (Local vs Rclone)

`backend.rs` expõe um enum `Backend` com duas variantes:

```rust
pub enum Backend {
    Local  { root: PathBuf },
    Rclone { remote: String, path: String },
}
```

Por que enum em vez de `dyn Trait`? O conjunto de backends é fechado (não há
plano de plugins externos), e o pattern-match deixa o dispatch visível no diff
sem custo de Box/vtable por sync.

API:

| Método | Retorna | Uso |
|---|---|---|
| `for_emulator(&emu)` | `Backend` | constrói a partir do `dest_kind`/`dest_remote`/`dest_path` da emulator row, já com `<emu_id>` apendado ao root |
| `live_fs()` | `String` | rclone-style fs string (`remote:path` ou caminho absoluto local) do live root — usado como `path2` do bisync |
| `live_fs_at(sub)` | `String` | `live_fs()` + `/sub`, usado nas subtrees por emulador |
| `history_root_fs()` | `String` | `<base>/.history/<emu_id>` (parent de todos os timestamps) |
| `snapshot_run_fs(ts)` | `String` | `<base>/.history/<emu_id>/<ts>` — pasta do run |
| `snapshot_full_fs(ts)` | `String` | `<run>/full` — destino do snapshot completo |
| `snapshot_delta_fs_at(ts, sub)` | `String` | `<run>/delta[/<sub>]` — destino do `--backupdir2` por subtree |
| `live_has_data()` | `Result<bool>` | detecta lado populado pra escolher `--resync-mode` no primeiro sync |
| `snapshot_full(ts)` | `Result<()>` | `live → <run>/full` via rclone `sync/copy` (CopyObject server-side em S3) |
| `copy_dir_contents(src)` | `Result<()>` | push one-way (usado por `sync_one`) |
| `copy_file(src)` | `Result<()>` | push one-way single file (pcsx2 single-save sync) |
| `child(segment)` | `Backend` | desce uma pasta dentro do live root |
| `ensure_dir()` | `Result<()>` | mkdir do live root (no-op pra rclone — `sync/copy` cria sob demanda) |

### Rclone (S3-compatible)

Integração via `librclone` (rclone como C-shared library) carregado dinamicamente:

- `scripts/build-librclone.ps1` (ou `.sh`): clona rclone, builda
  `librclone.{dll,so,dylib}` (~50 MB) + header em `src-tauri/lib/<triple>/`.
- `src-tauri/build.rs`: stage da DLL pro target dir do cargo (pra ficar ao lado
  do exe em runtime).
- `src-tauri/src/rclone.rs`: FFI via `libloading` — não link-time, ABI-agnóstico
  (DLL MinGW + Rust MSVC convivem). Wrappers expostos:
  - `rpc(method, input_json) -> Result<String, String>` — base, mapeia `RcloneRPC`.
  - `rpc_json(method, Value) -> Result<Value, String>` — versão tipada.
  - `create_s3_remote(cfg)` — chama `config/create` (ou `config/update` se já
    existe) com `obscure: true` pra encriptar o secret no `rclone.conf`.
  - `delete_remote(name)` — `config/delete`, idempotente.
  - `get_remote(name)` — `config/get`, retorna config sanitizado (secrets ofuscados).
  - `test_remote(name, path)` — `operations/list` com `maxDepth: 1`, smoke test
    de auth/conectividade que não transfere bytes.
  - `list_remotes()` — `config/listremotes`.
  - `bisync(BisyncOpts)` — `sync/bisync` com `path1`/`path2`, `backupdir2`,
    `conflictResolve` (`newer`/`older`/`larger`/`smaller`/`path1`/`path2`/`none`),
    `resync` + `resyncMode` (pra primeira sync de um pair).
  - `copy_fs(src_fs, dst_fs)` — `sync/copy` server-side, usado pra full
    snapshot do live root.
  - `copyfile`/`purge` — single-file copy e recursive delete (placeholder
    pras fases 2/3 — revert e prune).
  - `has_entries(fs, path)` — `operations/list` com `maxDepth: 1`, usado pra
    detectar qual lado tem dados no primeiro sync.

**Setup S3 via UI** (botão `[ manage remotes ]` no home → `/remotes`):

| Provider | Provider field | Endpoint | Region |
|---|---|---|---|
| Amazon S3 | `AWS` | (deixe vazio — derivado da region) | `us-east-1` |
| Cloudflare R2 | `Cloudflare` | `https://<account-id>.r2.cloudflarestorage.com` | `auto` |
| Backblaze B2 (S3 API) | `Other` | `https://s3.<region>.backblazeb2.com` | `us-west-002` |
| Wasabi | `Wasabi` | `https://s3.<region>.wasabisys.com` | varia |
| MinIO / self-hosted | `Minio` | URL do servidor | qualquer |
| DigitalOcean Spaces | `DigitalOcean` | `https://<region>.digitaloceanspaces.com` | qualquer |

Após criar o remote, em `/emulator/<id>` no card `[ paths ]`:

1. Toggle `[ rclone ]` em **destination kind**.
2. Selecione o remote no dropdown.
3. **path no remote** = `bucket/prefix` (ex.: `meu-bucket/save-sync`).
4. `[ commit paths ]`.

O destino final fica em `<remote>:<path>/<emu_id>/`. Sync segue o mesmo
gatilho do local (sync now / watcher / proc-watch).

**Limitações da implementação atual:**

- Sync via librclone é blocking por design — chamamos `do_sync` dentro de
  `tokio::task::spawn_blocking` pra não travar o runtime, e um reporter
  paralelo polla `core/stats` a cada 500ms emitindo o event
  `sync-progress { id, active, stats }`. UI mostra um banner sticky no
  rodapé com bytes transferidos / total / speed / ETA. Banner some no
  primeiro event com `active: false` (emitido após `do_sync` retornar).
- `core/stats` é global ao processo rclone — se duas syncs rodarem em
  paralelo (raro, mas possível com múltiplos watchers ativos), o banner
  mostra stats agregados.
- Conflitos são resolvidos por `--conflict-resolve newer` (mtime mais recente
  ganha). O loser **NÃO é descartado** — `--conflict-loser num` preserva
  como `<arquivo>.conflict1`. UI surfaceia esses no card `[ conflicts ]`
  do emulator detail com 3 ações: `[ keep current ]` (apaga `.conflict`),
  `[ use conflict ]` (sobrescreve current), `[ keep both ]` (renomeia
  `.conflict1` pra nome permanente).
- OAuth (Drive, Dropbox) ainda não implementado — só remotes que aceitam
  credenciais estáticas funcionam por enquanto.
- Revert pela UI: card `[ history ]` no save detail lista versões em
  ordem reversa cronológica com badges `[ full ]` / `[ delta ]` e tamanho.
  `[ revert ]` copia pro live E pro source local em paralelo, depois
  invalida `bisync_initialized` (próximo sync re-baselina com `--resync`
  pra evitar conflito artificial). Confirmação inline antes de
  sobrescrever — sem dialog modal por enquanto.

### i18n (3 idiomas)

`svelte-i18n` carrega 3 dicionários em `src/lib/i18n/{pt-BR,en,es}.json`. Default = `pt-BR` (com fallback pra navegador via `navigator.language`), persiste em `localStorage["save-sync-locale"]`. Toggle no header cicla BR → EN → ES.

Padrão de uso:

```svelte
<span>{$_("emulator.history.tag")}</span>
<button>{$_("common.confirm")}</button>
```

**Erros do backend** seguem convenção de código: cada `Err(...)` retorna uma string-código tipo `"save_not_found"` em vez de mensagem humana. `tErr()` em `src/lib/i18n/index.ts` mapeia `<code>` → `errors.<code>` no dict ativo. Strings que não casam com chave conhecida caem em fallback raw — então código legado continua exibindo, só sem tradução.

Códigos existentes (todos com tradução nos 3 idiomas):
`config_incomplete_source`, `config_incomplete_dest`, `config_incomplete_remote`, `dest_kind_invalid`, `emulator_disabled`, `process_name_missing`, `save_not_found`, `save_not_found_in_history`, `conflict_marker_invalid`, `resolve_action_invalid`, `history_mode_required`, `initial_sync_both_empty`, `memcard_not_supported`, `memcard_not_found`, `db_already_refreshing`.

Adicionar idioma novo: criar `src/lib/i18n/<locale>.json` espelhando a hierarquia dos 3 existentes, adicionar em `SUPPORTED_LOCALES` no index.ts, e `register()` o import.

### Tema

3 temas em `src/app.css` com CSS vars (`--bg`, `--accent`, `--text`, etc).

- `dark` (default) — preto + âmbar, vibe CRT terminal
- `light` — creme + âmbar, papel envelhecido
- `blue` — navy + âmbar (accent quente sobre frio)

Toggle cicla os 3, persiste em `localStorage`. Glyph no botão indica o próximo tema (`☼` → light, `❄` → blue, `☾` → dark).

### Comandos Tauri (lib.rs)

| Domínio | Comandos |
|---|---|
| Emuladores | `list_emulators`, `get_emulator`, `set_emulator_paths`, `set_process_name`, `set_enabled` |
| Sync | `sync_now`, `start_watch`, `stop_watch`, `start_proc_watch`, `stop_proc_watch` |
| Detecção | `detect_save_paths`, `get_eden_uuid` |
| Saves | `list_saves`, `get_save_entry`, `delete_save_entry`, `sync_one_save`, `open_save_folder` |
| Settings | `get_setting`, `set_setting` |
| TitleDBs | `title_db_status`, `refresh_title_db`, `ps2_db_status`, `refresh_ps2_db` |
| PS2 memcard | `list_memcard_saves` |
| Covers | `fetch_cover_url(title, kind?)` |
| Rclone | `rclone_version`, `rclone_list_remotes`, `rclone_create_s3_remote`, `rclone_delete_remote`, `rclone_get_remote`, `rclone_test_remote` |
| History | `get_history_settings`, `set_history_settings`, `supports_incremental_history`, `list_save_history`, `revert_save` |
| Conflicts | `list_conflicts`, `resolve_conflict` |
| Retention | `prune_history_now` (auto-rodado no fim de cada sync) |

### Eventos (do backend pro frontend)

- `emulator-changed` — payload é o `EmulatorView` atualizado. Disparado após qualquer mudança de estado (paths, watch on/off, sync resultou).
- `title-db-status` — `"refreshing" | "ready" | "error: ..."` durante download/parse do blawar.
- `ps2-db-status` — idem pro GameIndex.yaml.

---

## Decisões de design (não-óbvias, pra quem retornar daqui a 6 meses)

1. **Lazy-fetch das title DBs em vez de bundlar**: `titledb.json` (83 MB) e `GameIndex.yaml` (10 MB) ficam stale rápido. Bundlar engorda o installer e fica desatualizado entre releases. Cachear em `app_data_dir` + UI de refresh resolve.

2. **`<dest>/<emulator_id>/` wrap automático**: bulk sync e sync individual sempre criam um subdir por emulador. Permite apontar 3 emuladores pro mesmo backup folder sem conflito visual.

3. **PCSX2 read-only**: write-back num memcard parseado é arriscado (corromper save) e dobra o trabalho. Backup é por memcard inteiro — robusto e reversível. UI bloqueia delete/sync individual via `READ_ONLY_EMUS = new Set(["pcsx2"])`.

4. **librclone via libloading (não link-time)**: o cargo Windows usa toolchain MSVC por default; Go produz `librclone.dll` com ABI MinGW. Sem `.lib` de import, MSVC não consegue link-time. Carregar dinâmicamente em runtime resolve — `extern "C"` é ABI-compatível entre MinGW e MSVC pra calls simples.

5. **Backdrop blur na title bar sticky**: `backdrop-filter: blur(10px)` mantém o efeito de "vidro" sobre o conteúdo que rola por baixo. Necessário porque `--bg-elev` é semi-transparente e a barra é full-width sticky no topo.

6. **`overflow-x: hidden` só no `html`, não no `body`**: setar no body cria um contexto de scroll que quebra `position: sticky` no Chromium/WebView2. Movido pro html como fix.

7. **Ícone via SGDB icon endpoint vs grid endpoint** (per view mode): grid 600x900 fica esmagado em thumb 40×40 da list view. List view chama `fetch_cover_url(title, "icon")`; grid view chama com `"grid"` (default).

8. **`.history/` é sibling do live root, não nested**: se `<live>` é `bucket/saves/eden/` e `.history/` ficasse em `bucket/saves/eden/.history/`, bisync iria mirroring o próprio history pra source. Por isso fica em `bucket/saves/.history/eden/` — bisync nunca vê. Backend tem teste unitário pra essa invariante (`rclone_history_root_is_sibling_of_live`).

9. **`bisync_initialized` é per-pair, não global**: cada par `(source, dest_kind+dest_remote+dest_path)` precisa de um `--resync` inicial. Mudar qualquer um dos paths (`db::set_paths`) automaticamente zera o flag — próximo sync redetecta lados populados e roda `--resync` com mode apropriado. Editar history settings (retention, mode) **não** zera — apenas paths.

10. **Per-emulator history settings**: `pcsx2` (memcards binários) só permite `full_enabled = true`. `set_history_settings` coage `incremental_enabled = false` no backend mesmo se a UI for ignorada — `db::supports_incremental_history(emu_id)` é a fonte da verdade. Adicionar Duckstation no futuro = adicionar `"duckstation"` ao match dessa função.

11. **Modes são booleanos independentes, não enum**: pode ligar incremental, full, ambos, ou nenhum (se `enabled=false`). Migration v5 traduziu o antigo `mode: string` em `incremental_enabled` + `full_enabled`. Ambos ligados significa "cada sync gera `.history/<ts>/full/` E `.history/<ts>/delta/`" — storage 2x, mas tanto faz reverter "tudo de antes" quanto "um arquivo overwritten específico".

12. **`.history/<ts>/{full,delta}/` subdirs**: quando os dois modos estão on no mesmo run, eles compartilham `<ts>` mas precisam de paths separados. Sem subdir, snapshot full sobrescreveria o que `--backupdir2` salvou. Teste unitário `snapshot_full_and_delta_never_collide` trava essa invariante.

13. **Revert invalida `bisync_initialized`**: depois de copiar uma versão antiga pra live + source, os listing files do rclone bisync (em `~/.cache/rclone/bisync/`) ainda refletem o estado pré-revert. Bisync sem `--resync` veria os dois lados "regredidos" e marcaria como conflito de modificação dupla. `db::mark_bisync_needs_resync()` zera o flag pra que o próximo sync re-baselinе.

14. **Save detection inclui prefix + boundary slash**: `group_history_entries` filtra entradas do listing recursivo por `sub_path`. Aceita match exato OU prefix seguido de `/` — sem o check de slash, "Mcd001.ps2" também casaria com "Mcd001.ps2.bak" e "Mcd0011.ps2". Teste `group_history_handles_exact_file_match_pcsx2_style` cobre.

15. **`--conflict-loser num` sempre, nunca delete**: bisync hoje sempre preserva o loser como `.conflict1`. Zero data-loss por design. O custo é poluição moderada no save folder se há muitos conflitos não resolvidos — o card `[ conflicts ]` no emulator detail é o "inbox" pra limpar. Pisca em accent quando há pendentes.

16. **`use_conflict` opera por arquivo, não por save**: pra eden/rpcs3, se uma única sync gera múltiplos `.conflict1` dentro do mesmo título (ex: 3 arquivos overwritten ao mesmo tempo), cada um é uma linha separada no card. Resolver tem que ser feito por arquivo. v2 pode agrupar por save.

17. **`keep_both` renomeia movendo o sufixo pra antes da extensão**: `Mcd001.ps2.conflict1` → `Mcd001-conflict1.ps2`. Mantém a extensão útil (`.ps2`) pra emulador identificar o tipo. Pra arquivos sem extensão, simplesmente apenda `-conflictN`. Helper `rename_keep_both_path` tem teste cobrindo paths com dots no diretório (`1.0.0/save.conflict1`) que não devem ser confundidos com extensão.

18. **Prune roda como step-final do sync, best-effort**: depois do(s) bisync(s) subsequent, `prune_history` aplica retention. Falha do prune **não** falha o sync — o que importa é o sync ter copiado os dados; deletar snapshot antigo é gravy. Loga e segue. Botão `[ prune now ]` no card history dispara manualmente quando o usuário quer reagir a uma mudança de retention sem esperar próxima sync.

19. **Duas regras de retention, age first depois size**: `retention_days` marca timestamps acima da idade pra remover, depois `retention_max_mb` mata os mais antigos sobreviventes até o total caber. Qualquer regra com valor `<= 0` está desabilitada. Lógica é pura em `pick_snapshots_to_prune`, com testes cobrindo cada combinação (só age, só size, ambos, ambos desabilitados, sob o cap, timestamps malformados). Timestamps que não parseiam (ex: pasta com nome estranho deixada por outro processo) **não** são marcados pra deleção — seguro por default.

20. **Formato do timestamp tem hífens em vez de dois pontos**: `YYYY-MM-DDTHH-MM-SSZ`. Dois-pontos é caractere inválido pra nome de arquivo no Windows e cria atrito em ferramentas que escapam mal. `parse_snapshot_ts` reverte os hífens da parte HH-MM-SS pra dois-pontos antes de chamar o parser ISO8601 da chrono.

21. **PCSX2 conflict = duplicação automática, eden/rpcs3 conflict = UI manual**: pra emus file-based (pcsx2), no fim do `do_sync` o `.conflict1` é renomeado pra `Mcd001-conflict1.ps2` automaticamente, em live + source, via `auto_duplicate_file_conflicts`. O emulador enxerga como memcard válido (extensão `.ps2` preservada). Pra emus dir-based (eden/rpcs3), o usuário resolve via card `[ conflicts ]` no detail do emulador. Critério: `db::supports_incremental_history(emu_id)` — file-based = !incremental.

22. **Async sync via spawn_blocking + progress reporter**: `do_sync_async` envolve `do_sync` em `tokio::task::spawn_blocking` (librclone é blocking, não pode rodar no async runtime sem isso) e roda um task paralelo polling `core/stats` a cada 500ms. Quando `do_sync` retorna, o reporter recebe stop via mpsc, e um event final com `active: false` é emitido pra UI limpar o banner. Watchers (file watcher + proc-watch) também chamam `do_sync_async` pra ter o mesmo flow.

---

## Roadmap

- [x] Eden / RPCS3 / PCSX2 listing + sync individual (eden, rpcs3) + bulk sync
- [x] Watcher + proc-watch
- [x] Auto-detect paths
- [x] PS2 memcard parsing + title resolution
- [x] Switch title-id resolution via blawar + nlib.cc fallback
- [x] librclone dynamic loading
- [x] Backend trait abstrato (Local + Rclone)
- [x] DB schema migration pra `dest_kind` + `dest_remote`
- [x] UI de gerenciamento de remotes (S3 / R2 / B2 / MinIO / Wasabi / DO)
- [x] Sync via rclone end-to-end (S3 family)
- [x] Bidirectional sync via `rclone bisync` (com auto-resync detection)
- [x] History modes independentes (incremental via `--backupdir2`, full via pre-snapshot, qualquer combinação)
- [x] Settings por emulador (enabled, incremental_enabled, full_enabled, retention_days, retention_max_mb)
- [x] **Fase 2**: UI de revert (`list_save_history` + `revert_save` + card `[ history ]` no save detail)
- [x] **Fase 3**: Conflict resolution via `--conflict-loser num` (loser preservado como `.conflict1`, card `[ conflicts ]` no emulator detail com 3 ações: keep_current / use_conflict / keep_both)
- [x] **Fase 4**: Prune automático (retention_days + retention_max_mb enforced ao fim de cada sync; botão `[ prune now ]` manual disponível)
- [x] **i18n**: pt-BR + en + es via `svelte-i18n`, toggle no header (BR/EN/ES), backend emite códigos (`save_not_found`, etc) que o frontend traduz
- [x] **PCSX2 duplicação automática**: arquivos `.conflict1` em emus file-based são renomeados pra `<base>-conflict1.<ext>` no fim do sync — PCSX2 enxerga ambos como memcards válidos
- [x] **Async sync com progress**: `do_sync` roda em `spawn_blocking`, reporter paralelo polla `core/stats` a cada 500ms e emite event `sync-progress`. UI tem banner sticky no rodapé com bytes/speed/ETA
- [x] Testes unitários (81 testes em backend/db/lib/rclone — `cargo test --lib`)
- [ ] Duplicação de memcard PCSX2 em vez de overwrite quando há conflito
- [ ] Async sync com progress (core/stats)
- [ ] OAuth flow (Drive, Dropbox, OneDrive) via `config/create` + callback HTTP
- [ ] Linux build + AppImage via CI
- [ ] Android port (REST nativo, sem rclone — limitação Android)
- [ ] Duckstation (PS1) — list-only, similar ao pcsx2

---

## Solução de problemas

**`librclone not found at ...` durante `cargo build`**

Esqueceu de rodar `scripts/build-librclone.ps1`. Esse script é pré-requisito.

**Smoke test do rclone retorna `ERR :: dlopen ...`**

A DLL não foi encontrada em runtime. Confirma que `src-tauri/target/debug/librclone.dll` existe (deveria ser copiado pelo `build.rs` automaticamente). Se não, roda `cargo clean` e rebuilda.

**Saves do eden mostram só title-ids hex (sem nome)**

Title DB ainda não terminou de baixar (~83 MB, primeira run). Aguarda a notificação `title-db-status: ready` ou abre `/emulator/eden` e clica `[ atualizar via blawar ]`.

**Memcard PCSX2 com erro `memcard vazio / não formatado`**

PCSX2 cria `.ps2` placeholder com header zero antes de qualquer save ser escrito. Não é erro, só significa que esse memcard está em branco. `list_pcsx2` filtra esses automaticamente; só aparece se você clicar direto pelo URL.

**Janela do app abre sem ícone customizado**

Cargo não recompilou apesar de você ter rodado `npm run tauri icon`. Solução: `rm src-tauri/target/debug/save-sync.exe` e roda `npm run tauri dev` de novo. Pra forçar reset total: `cd src-tauri && cargo clean`.

**`rclone_test_remote` retorna `directory not found` em bucket S3 vazio**

Esperado. `operations/list` em prefixo inexistente é erro mesmo. Crie qualquer
objeto via console do provider (ou rode um sync) pra que o prefixo exista — ou
deixe `path` vazio e teste contra a raiz do remote, que sempre lista.

**Sync pra S3 falha com `signature mismatch` ou `403 Forbidden`**

Geralmente region errada. R2 usa `region: auto`. AWS exige a region exata do
bucket (`us-east-1`, `sa-east-1`, etc). MinIO/B2/Wasabi exigem region que
combine com a do endpoint. Veja a tabela de presets em [Rclone (S3-compatible)](#rclone-s3-compatible).

**Primeiro sync depois de mudar paths reroda `--resync` e leva muito tempo**

Esperado. `db::set_paths` invalida `bisync_initialized` automaticamente
porque o pareamento rclone bisync mudou. Próximo sync detecta lados
populados, escolhe `resync-mode` e reconstrói as listings — leva ~1 leitura
full de cada lado. Depois disso voltam os deltas rápidos.

**History não cresce mesmo com `full_enabled = true`**

`full_enabled` faz snapshot **antes de cada sync que vai rodar**. Se o
sync é no-op (sem deltas detectados), não há razão pra snapshot e nada é
escrito. Use `[ sync now ]` depois de jogar/modificar um save pra forçar.

**`[ commit history ]` desabilitado mesmo com mudança feita**

Você desligou incremental E full com `history.enabled = true`. Backend
rejeita esse combo ("selecione pelo menos um modo de backup"). Marque um
dos dois ou desligue history inteiro com `[ off ]`.

**Aparecem arquivos `.conflict1` nos meus saves**

Comportamento intencional. Quando bisync detecta o mesmo arquivo
modificado em ambos os lados desde a última sync, o "vencedor" (mtime
mais recente) sobrescreve, mas o "perdedor" é preservado como
`<arquivo>.conflict1` pra você decidir depois. Card `[ conflicts ]` no
emulator detail surfaceia esses pra resolução com 3 ações:

- `[ keep current ]` — apaga o `.conflict1`, fica só o vencedor (default seguro).
- `[ use conflict ]` — sobrescreve current com `.conflict1`, depois apaga (descarta o vencedor inicial).
- `[ keep both ]` — renomeia `Mcd001.ps2.conflict1` → `Mcd001-conflict1.ps2` (mantém a extensão pra emulador identificar). Útil em pcsx2 onde o memcard inteiro é a unidade.

Depois de qualquer resolução, próximo sync re-baseliza com `--resync`
(mesmo motivo do revert: state files do bisync ficaram defasados).

**History não está sendo limpa apesar de ter retention configurado**

`prune_history` roda como step-final do sync. Se você nunca sincronizou
depois de mudar `retention_days` ou `retention_max_mb`, nada apaga.
Botão `[ prune now ]` no card `[ history ]` dispara manualmente.
Lembre que retention `<= 0` desabilita a regra correspondente —
`retention_days = 0` + `retention_max_mb = 0` significa "guarde tudo
pra sempre".

## Testes

```bash
cd src-tauri
cargo test --lib
```

Cobertura atual (81 testes):

| Módulo | Cobertura |
|---|---|
| `backend` | path joining (POSIX vs Windows), live/snapshot fs strings (run/full/delta), history-root-é-sibling invariant, full-vs-delta-never-collide invariant, child(), for_emulator() validation matrix |
| `db` | migrations v3/v4/v5 (inclui translação de mode legado pros booleanos), supports_incremental classification, history settings round-trip com modos independentes, pcsx2 coerção de incremental, at-least-one-mode validation (rejeita ambos off quando enabled, aceita quando disabled), defaults round-trip via set_history (cross-check), bisync_initialized lifecycle |
| `rclone` | split_root pra rclone vs Windows-local (drive letter intacto) vs POSIX absolute vs bare "remote:" |
| `lib` | sync_subtrees per emulator, validate_config matrix (local/rclone, missing fields), group_history_entries (bucket por ts, combina full+delta no mesmo run, filtra por sub_path prefix com boundary slash, soma só files não dirs, ignora modos desconhecidos), strip_conflict_marker (parse válido, rejeita orfãos sem original e sufixos não-numéricos), rename_keep_both_path (move marker pra antes da extensão, sem extensão apenda, dot em diretório não confunde), find_conflicts (pareia current+loser, multi-conflict chain `.conflict1+.conflict2`, ignora órfãos e dir entries), parse_snapshot_ts (valid + 4 rejeições de formato), pick_snapshots_to_prune (só age, só size com oldest-first, multi-drop, ambos combinados, ambos desabilitados, sob o cap, timestamps malformados) |

Tests rodam offline — não exercitam o librclone real (FFI inicializa só em
runtime). Integração end-to-end com S3/MinIO ainda é manual.
