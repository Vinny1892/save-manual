# Client de PC

Instalar, parear com o NAS, apontar pros emuladores.

O client existe e não é opcional: o watcher de filesystem e o de processo
precisam rodar **na máquina onde o emulador está**. Um server no NAS não
enxerga o teu `%APPDATA%` nem o processo do emulador rodando aqui.

---

## 1. Instalar

Baixe da aba **Actions** do GitHub, na run mais recente do `master`:

| Sistema | Artefato | O que tem dentro |
|---|---|---|
| Windows | `save-sync-windows-x64-<sha>` | instalador NSIS, MSI e um portable `.zip` |
| Linux x64 | `save-sync-linux-x64-<sha>` | AppImage e `.deb` |
| Linux ARM64 | `save-sync-linux-arm64-<sha>` | AppImage e `.deb` |

O **portable** do Windows é só `save-sync.exe` + `librclone.dll`: roda em
qualquer Windows 10/11 com WebView2 (que já vem no Win 11), sem instalar
nada e sem entrada no menu Iniciar.

No Linux, o `.deb` e o AppImage já carregam o `librclone.so` dentro.

---

## 2. Parear com o server

1. Na web UI do NAS, logado, gere um **código de pareamento**
2. No client, home → **[ server do NAS ]**
3. Preencha o endereço (`192.168.0.10:8787` basta — sem esquema assume
   `http`) e o código
4. **[ parear ]**

O código vale 10 minutos e serve uma vez só. Depois disso o client guarda um
token que não expira e é revogável pela web UI do server.

Se preferir gerar o código pelo terminal do NAS:

```bash
docker compose exec save-sync-server save-sync-server --pair
```

### Sem parear

O client funciona sozinho: sem server, o sync segue no modo antigo, com
destino numa pasta local ou num remote do rclone. É o caminho pra quem ainda
não tem o NAS. O que muda ao parear é **quem resolve conflito e guarda
histórico** — passa a ser o server.

---

## 3. Apontar pros emuladores

Na home, clique no emulador. No card **[ paths ]**:

- **source**: a pasta raiz do emulador. O botão de detectar varre os
  lugares conhecidos
- **[ commit paths ]** pra salvar

O que cada emulador espera:

| Emulador | source | Estrutura esperada dentro |
|---|---|---|
| eden | `<eden>/user/nand` | `user/save/...` e `system/save/...` |
| rpcs3 | `<rpcs3>/dev_hdd0` | `home/<user>/savedata/` |
| pcsx2 | `<pcsx2>/memcards` | arquivos `.ps2` |

Do NAND do eden, **só as subárvores de save entram no sync** — o resto tem
gigabytes de conteúdo de sistema que não é save.

---

## 4. Quando o sync roda

Três gatilhos, configuráveis por emulador no card **[ ops ]**:

| Gatilho | Quando dispara |
|---|---|
| manual | botão `[ sync now ]` |
| watcher | mudança na pasta de saves, com 2s de espera pra acumular |
| proc-watch | quando o processo do emulador **fecha** |

O proc-watch é o mais confiável na prática: o emulador costuma escrever o
save no fim, e sincronizar quando ele fecha pega o estado completo em vez
de um arquivo pela metade. Exige preencher o nome do processo (`eden.exe`,
`pcsx2-qt.exe`, `rpcs3.exe`).

---

## 5. Conflitos

Quando dois devices editam o mesmo save entre syncs, **o mtime mais recente
vence e o perdedor nunca é descartado** — ele fica preservado como
`<arquivo>.conflict1` e aparece no card `[ conflicts ]`, com três ações:

- **keep current** — apaga o `.conflict`
- **use conflict** — sobrescreve o atual com o preservado
- **keep both** — renomeia pra nome permanente (`Mcd001-conflict1.ps2`)

Pro PCSX2 a duplicação é automática: o `.conflict1` vira um memcard de nome
próprio no fim do sync, porque o emulador precisa enxergar os dois como
memcards válidos.

Um caso que a regra trata de propósito de forma assimétrica: **edição vence
deleção**. Se um lado editou e o outro apagou o mesmo save, o arquivo
sobrevive. Apagar um save que alguém acabou de modificar é perda
irreversível; ressuscitar um que alguém queria apagado custa um delete.

---

## 6. Histórico e revert

Cada sync pode guardar versões anteriores, com política por emulador no card
`[ history ]`:

- **incremental** — só o que foi sobrescrito naquele sync
- **full** — snapshot da árvore inteira antes de mexer

Os dois são independentes: pode ligar um, outro, ambos, ou nenhum. Emulador
file-based (pcsx2) só aceita full — a unidade de save é um binário inteiro, e
incremental degeneraria pra full de qualquer jeito.

Retenção por idade e por tamanho, com o corte de idade primeiro. Pra
reverter, entre no save e use o card `[ history ]`.

---

## Problemas comuns

**`config_incomplete_source`** — falta apontar a pasta do emulador no card
`[ paths ]`.

**O pareamento falha.** Confira que o endereço abre no browser do PC. Código
expirado (10 min) ou já usado também dá erro — gere outro.

**`resync_required` no sync.** O client ficou parado mais que a janela de
tombstone do server (90 dias). O próximo sync refaz o baseline sozinho; nada
se perde.

**`busy` (429).** Outro device está sincronizando esse mesmo emulador.
Espera o commit dele.

**Os saves aparecem com o id cru em vez do nome do jogo.** As bases de
título ainda estão carregando no server (83 MB + 10 MB, baixadas no
primeiro boot). Resolve sozinho.

**No Windows o app não abre.** Precisa do WebView2. Vem por padrão no
Win 11; no Win 10 antigo, instale o runtime da Microsoft.

---

## Rodando do código

```bash
npm install
.\scripts\build-librclone.ps1      # ou: bash scripts/build-librclone.sh
npm run tauri dev
```

O `librclone` precisa de Go e gcc pra buildar — ver o README principal.
