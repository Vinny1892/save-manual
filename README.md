# save-sync

Replicação de save data entre instalações de emuladores (eden / rpcs3 / pcsx2) e
opcionalmente cloud (em desenvolvimento, via librclone).

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
        └── rclone.rs              # FFI dynamic load do librclone via libloading
```

---

## Arquitetura por componente

### Estado persistente

| Local | Conteúdo |
|---|---|
| `%APPDATA%\com.savesync.app\save-sync.db` | SQLite — emuladores, paths, settings |
| `%APPDATA%\com.savesync.app\titledb.json` | cache do blawar (Switch title-id → nome) |
| `%APPDATA%\com.savesync.app\ps2-gameindex.yaml` | cache do PCSX2 GameIndex (PS2 serial → nome) |
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

`do_sync(id, source, dest)` em `lib.rs`:
- Cria `<dest>/<id>/` (wrap por emulador pra isolamento quando vários compartilham o mesmo backup folder).
- `eden` chama `copy_eden_saves` (copia só `system/save/8000000000000010` + `user/save/`, não a NAND inteira).
- `rpcs3`/`pcsx2` caem em `copy_saves` (copia o source root inteiro com `copy_inside: true`).

Sync individual de save (`sync_one`) em `saves.rs` é só pra eden/rpcs3 — pcsx2 está em list-only mode.

### Cloud / rclone (em desenvolvimento)

Integração via `librclone` (rclone como C-shared library) carregado dinamicamente:

- `scripts/build-librclone.ps1` (ou `.sh`): clona rclone, builda `librclone.{dll,so,dylib}` (~50 MB) + header em `src-tauri/lib/<triple>/`.
- `src-tauri/build.rs`: stage da DLL pro target dir do cargo (pra ficar ao lado do exe em runtime).
- `src-tauri/src/rclone.rs`: FFI via crate `libloading` — não link-time, ABI-agnóstico (DLL MinGW + Rust MSVC convivem).
- API exposta:
  - `rpc(method, input_json) -> Result<String, String>` — wrapper de `RcloneRPC`.
  - `rpc_json(method, Value) -> Result<Value, String>` — versão tipada com `serde_json`.
- Comandos Tauri: `rclone_version`, `rclone_list_remotes` (smoke test).

**Pendente**: trait `Backend`, `RcloneBackend`, schema `dest_kind`/`dest_remote`, UI de gerenciamento de remotes, OAuth flow via `config/create`.

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
| Rclone | `rclone_version`, `rclone_list_remotes` |

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

---

## Roadmap

- [x] Eden / RPCS3 / PCSX2 listing + sync individual (eden, rpcs3) + bulk sync
- [x] Watcher + proc-watch
- [x] Auto-detect paths
- [x] PS2 memcard parsing + title resolution
- [x] Switch title-id resolution via blawar + nlib.cc fallback
- [x] librclone dynamic loading
- [ ] Backend trait abstrato (LocalBackend + RcloneBackend)
- [ ] UI de gerenciamento de remotes (Drive OAuth, S3, etc)
- [ ] DB schema migration pra `dest_kind` + `dest_remote`
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
