# Protocolo de sync — v1

Especificação do que o client de PC e o client Android falam com o server.
Um protocolo só: nem SMB nem rclone entram aqui, porque nenhum dos dois é
viável de dentro de um app Android.

Status: **especificação**. Implementação em [#3](https://github.com/Vinny1892/save-manual/issues/3).

O rclone não morre — continua no server, usando o `Backend::Rclone` que já
existe, pro backup off-site opcional do NAS pra S3/R2. Ele só sai do caminho
client ↔ server.

---

## 1. O que isso substitui

Hoje o sync é `rclone bisync` rodando no client, com o destino sendo um
remote. O `bisync` guarda *listing files* em `~/.cache/rclone/bisync/` — um
retrato de como os dois lados estavam no fim do último sync. É com isso que
ele distingue "esse arquivo mudou do meu lado" de "mudou do outro lado".

Esse estado-âncora é a parte que mais importa replicar. Sem ele o protocolo
fica incorreto de um jeito que só aparece em produção — a seção 2 explica.

O flag `bisync_initialized` do `history_settings` vira `last_rev == 0` neste
protocolo, e `mark_bisync_needs_resync()` vira "zera o `last_rev` do device".

---

## 2. Por que não é só "diff de manifesto"

A ideia intuitiva — client manda a lista dos arquivos que tem, server compara
com a dele, e a diferença é o que transfere — **está errada**, e vale gastar
um parágrafo com isso porque é o erro que a v1 tem que não cometer.

Um arquivo que existe no server e não existe no client tem duas explicações
opostas:

- o server recebeu esse save de outro device → o client **deve baixar**
- o client apagou esse save → o server **deve apagar**

Comparar os dois estados atuais não distingue os casos. O que distingue é um
terceiro ponto: **como estava no fim do último sync**. Se o arquivo estava no
baseline, sumir do client é uma deleção; se não estava, é coisa nova do
server.

O mesmo vale pra modificação: sem baseline, "os dois lados têm conteúdo
diferente" não diz quem mexeu. Com baseline dá pra saber se mexeu um, o
outro, ou os dois — e só o último caso é conflito de verdade.

Por isso o modelo abaixo tem estado dos dois lados, e não só manifesto.

---

## 3. Modelo de estado

### No server, por emulador

Um índice de arquivos e um contador monotônico:

| Campo | Significado |
|---|---|
| `path` | caminho relativo à raiz do emulador, separador `/` |
| `rev` | valor do `head_rev` quando esta entrada mudou pela última vez |
| `size` | bytes |
| `mtime` | epoch em milissegundos, UTC |
| `hash` | SHA-256 do conteúdo, hex minúsculo |
| `deleted` | tombstone — a entrada existe pra propagar a deleção |

`head_rev` é por emulador e só cresce. Todo commit bem-sucedido incrementa em
1 e carimba nas entradas alteradas.

### No client, por emulador

| Campo | Significado |
|---|---|
| `last_rev` | o `head_rev` do último commit que este device aplicou por inteiro |
| baseline | `path → {size, mtime, hash}` no momento daquele commit |

O baseline é o que o `bisync` guardava nos listing files. Vive no SQLite
local do client.

### Os paths

São relativos à raiz do emulador e limitados às subárvores que já entram no
sync hoje (`engine::sync_subtrees`) — pro eden, `system/save/8000000000000010`
e `user/save`; pros outros, a raiz inteira. O client não manda o que está
fora disso, e o server recusa path fora da whitelist.

Regras: separador sempre `/`, sem `.` ou `..`, sem caminho absoluto, sem
barra à esquerda. O server valida e responde `400 invalid_path`.

---

## 4. Ciclo de um sync

Três fases. Nenhuma escrita na árvore viva acontece antes do commit.

```
client                                            server
  │
  │ 1. varre local, compara com baseline
  │    → local_changes[]
  │
  ├─ POST /sync/{emu}/plan ─────────────────────────▶
  │    {last_rev, changes[]}                    calcula remote_changes =
  │                                             entradas com rev > last_rev;
  │                                             cruza com local_changes;
  │                                             resolve conflitos;
  │                                             abre sessão
  ◀──────────────────────────── {session, upload[], download[],
  │                              delete_local[], conflicts[]}
  │
  │ 2. transfere (até 4 em paralelo)
  ├─ PUT /sync/{emu}/blob?session=&path= ──────────▶  grava em staging
  ├─ GET /sync/{emu}/blob?path=&rev= ──────────────▶  devolve conteúdo
  │
  ├─ POST /sync/{emu}/commit ──────────────────────▶
  │    {session}                                 snapshot de history,
  │                                              move staging → live,
  │                                              head_rev += 1
  ◀──────────────────────────── {new_rev, applied}
  │
  │ 3. aplica local, grava baseline novo e last_rev = new_rev
```

A varredura local do passo 1 usa `size + mtime` como pré-filtro: só calcula
hash quando um dos dois difere do baseline. Numa árvore parada, um sync não
lê conteúdo nenhum.

---

## 5. Endpoints

Prefixo `/api/v1`. Todos exigem `Authorization: Bearer <device_token>`,
exceto os de pareamento. `{emu}` é `eden` | `rpcs3` | `pcsx2`.

### `POST /sync/{emu}/plan`

```jsonc
{
  "last_rev": 42,
  "changes": [
    {"path": "user/save/0000.../0100.../game.sav", "op": "put",
     "size": 1048576, "mtime": 1757650000000, "hash": "9f86d0…"},
    {"path": "user/save/0000.../0100.../antigo.sav", "op": "delete"}
  ]
}
```

Resposta:

```jsonc
{
  "session": "01J9…",           // opaco, expira com 1h de inatividade
  "head_rev": 47,
  "upload":       [{"path": "…"}],
  "download":     [{"path": "…", "rev": 45, "size": 2048, "hash": "…", "mtime": 1757650000000}],
  "delete_local": [{"path": "…"}],
  "conflicts":    [{"path": "…", "winner": "client", "loser_path": "….conflict1"}]
}
```

`upload` é o que o server quer receber; `download` e `delete_local` é o que o
client deve aplicar. Um path aparece em no máximo uma das listas.

### `PUT /sync/{emu}/blob?session={s}&path={p}`

Corpo é o conteúdo bruto. Headers obrigatórios: `Content-Length`,
`X-Save-Sync-Hash` (SHA-256 hex) e `X-Save-Sync-Mtime` (epoch ms). O server
recusa com `422 hash_mismatch` se o que chegou não bate — o hash é verificado
na ingestão, não no commit, pra falhar cedo.

Idempotente: reenviar o mesmo `(session, path, hash)` responde `200` sem
regravar.

### `GET /sync/{emu}/blob?path={p}&rev={r}`

Conteúdo bruto, com os mesmos headers de hash e mtime na resposta. `rev` é o
que veio no plano; se aquela versão já não for a corrente, o server responde
`409 stale_rev` e o client refaz o plano.

Aceita `Range` — é daqui que sai o resume de download.

### `POST /sync/{emu}/commit`

```jsonc
{"session": "01J9…"}
```

O server, em ordem: verifica que todo `upload` do plano chegou (senão
`409 incomplete_session` com a lista do que falta), tira o snapshot de
history conforme as `history_settings` do emulador, move o staging pra
árvore viva, aplica os tombstones, incrementa `head_rev` e roda o prune.

```jsonc
{"new_rev": 48, "applied": {"uploaded": 3, "downloaded": 0, "deleted": 1}}
```

O client só grava `last_rev = new_rev` depois de aplicar tudo localmente. Se
morrer no meio, o `last_rev` antigo continua valendo e o próximo plano
recalcula — a fase 3 é idempotente.

### `POST /sync/{emu}/resync`

Troca de manifesto completo. Ver seção 8.

---

## 6. Conflitos

A regra não muda, porque já está decidida e testada: **mtime mais recente
ganha, e o perdedor nunca é descartado.**

O server resolve no `plan`, não no `commit` — assim o client já sabe o que
vai acontecer antes de transferir um byte. Pra um path que mudou dos dois
lados desde o baseline:

1. compara `mtime`; o maior vence
2. empate de mtime com hash igual → não é conflito, é o mesmo conteúdo; a
   entrada só é reconciliada
3. empate de mtime com hash diferente → vence o **server**, por desempate
   determinístico (nenhum dos dois lados tem como saber a ordem real)
4. o perdedor é preservado como `<path>.conflict<n>`, com `n` sendo o menor
   inteiro livre

Isso mantém a invariante que já existe: **zero perda de dado por resolução
automática**. Os `.conflictN` continuam aparecendo no card `[ conflicts ]` da
UI, e as três ações (`keep_current`, `use_conflict`, `keep_both`) continuam
valendo — agora executadas no server, sobre o mesmo código de
`core::history`.

### Edição contra deleção

O caso em que um lado **editou** e o outro **apagou** o mesmo arquivo desde o
baseline não é simétrico aos de cima, e a regra é:

> **edição sempre vence deleção.**

Se o client editou e o server tinha apagado, o arquivo sobe e ressuscita. Se
o server tem versão nova e o client tinha apagado, o arquivo desce e volta.

O raciocínio é o mesmo que rege o resto: apagar um save que alguém acabou de
modificar é perda de dado irreversível, enquanto ressuscitar um save que
alguém queria apagado custa um delete a mais. Os dois erros não têm o mesmo
peso, então a regra não é simétrica de propósito.

Isso vale só quando as duas coisas acontecem **no mesmo intervalo** entre
syncs. Deleção que o outro lado não contradisse propaga normalmente, como
descrito na seção 7.

### Onde o perdedor fica

O `.conflictN` entra no conjunto sincronizado como arquivo comum, então ele
aparece nos dois lados no fim do mesmo ciclo:

| Quem venceu | O que acontece |
|---|---|
| client | o server renomeia a versão dele pra `.conflictN` (sem transferir), o client sobe a sua, e o `.conflictN` volta pro client na lista de `download` |
| server | o client renomeia a versão local pra `.conflictN` e sobe esse arquivo, e baixa a versão do server pro path original |

O plano é explícito quanto a isso: o `loser_path` aparece em `upload` ou em
`download` conforme o lado que segura o perdedor, pra que o client não
precise inferir nada.

Emuladores file-based (pcsx2) mantêm a duplicação automática:
`Mcd001.ps2.conflict1` vira `Mcd001-conflict1.ps2` no fim do commit, porque o
emulador precisa enxergar como memcard válido. É o
`auto_duplicate_file_conflicts` que já existe.

---

## 7. Deleções

Deleção é uma entrada com `deleted: true` e um `rev` — não é a ausência da
entrada. Sem isso, um device offline ressuscitaria o arquivo no próximo sync,
porque pra ele o arquivo "existe e o server não tem".

Tombstones são mantidos por **90 dias** e então removidos de vez. A
consequência está na seção 8.

---

## 8. Resync

Três situações levam ao resync, que é o equivalente do `--resync` de hoje:

1. **Primeiro sync do device** (`last_rev == 0`)
2. **Device velho demais**: `last_rev` anterior ao tombstone mais antigo ainda
   vivo. O server não consegue mais dizer o que foi apagado nesse intervalo,
   então responde `410 resync_required`
3. **Revert**: depois de restaurar uma versão antiga o estado "regrediu" dos
   dois lados, e um plano normal marcaria conflito artificial. É o mesmo
   motivo que hoje faz o revert zerar `bisync_initialized`

No resync o client manda o manifesto **inteiro** e o server compara com o
índice dele sem baseline. Sem ancestral, a regra é a mesma do
`do_initial_bisync` de hoje:

| Lado com dado | Direção |
|---|---|
| só client | sobe tudo |
| só server | desce tudo |
| os dois | merge por mtime, com o perdedor virando `.conflictN` |
| nenhum | erro `initial_sync_both_empty` |

---

## 9. As três decisões que estavam em aberto

### Hash: SHA-256, não blake3

Blake3 é mais rápido e era o candidato, mas o client Android é Kotlin:
SHA-256 vem no `MessageDigest` da JVM, com aceleração de hardware em qualquer
ARMv8, enquanto blake3 exigiria JNI ou uma implementação pura em Kotlin. Do
lado Rust, `sha2` também tem assembly.

O custo é irrelevante no nosso caso: o maior arquivo do domínio é um memcard
PS2 de 8 MB, e o gargalo é a rede do NAS, não a CPU. Trocar uma dependência
nativa no Android por alguns milissegundos de hash é bom negócio.

### Transfer: por arquivo

Um `PUT` por arquivo, até 4 em paralelo. Não em lote.

Os saves são pequenos e em quantidade modesta, então o ganho de empacotar
seria pequeno diante do custo: lote exige formato de container, torna o
resume mais grosso (falhou o lote, refaz o lote) e complica a verificação de
hash por arquivo. Se um dia o eden com muitos perfis mostrar que o
round-trip domina, dá pra acrescentar um `POST /blobs` em lote **sem mudar o
resto** — o plano e o commit não sabem como os bytes chegaram.

### Resume: por arquivo, com sessão persistente

A sessão sobrevive a queda de conexão (1h de inatividade) e o server lembra o
que já recebeu por `(session, path, hash)`. Retomar é refazer os `PUT` que
faltam; os que já chegaram respondem `200` na hora.

Download retoma por `Range`. Não há resume no meio de um upload de arquivo
único — pra 8 MB não compensa a complexidade de upload em partes.

---

## 10. Autenticação e pareamento

O usuário loga na web UI (sessão por cookie). Pra parear um device:

1. na web UI, gera um **código de pareamento** — 8 caracteres, validade de 10
   minutos, uso único
2. o client manda `POST /api/v1/pair` com `{code, device_name, platform}`
3. o server responde `{device_id, device_token}`

O `device_token` é o `Bearer` de todas as chamadas de sync, não expira e é
revogável na web UI. Fica guardado no SQLite local do client — que existe
também pra que o client não perca configuração quando o NAS estiver fora do
ar.

---

## 11. Erros

Corpo de erro é sempre `{"error": "<código>", "detail": "<texto opcional>"}`.
O código é estável e traduzível — mesma convenção que o backend já usa hoje
(`save_not_found`, `config_incomplete_source` etc), consumida pelo `tErr()`
do frontend.

| HTTP | Código | Quando |
|---|---|---|
| 400 | `invalid_path` | path malformado ou fora da whitelist de subárvores |
| 401 | `unauthorized` | token ausente, inválido ou revogado |
| 404 | `unknown_emulator` | `{emu}` não é um emulador conhecido |
| 409 | `stale_rev` | a versão pedida no GET já não é a corrente |
| 409 | `incomplete_session` | commit com upload faltando |
| 410 | `resync_required` | `last_rev` velho demais (tombstone já expirou) |
| 413 | `too_large` | arquivo acima do limite configurado |
| 422 | `hash_mismatch` | o conteúdo recebido não bate com o hash declarado |
| 429 | `busy` | já existe sync em andamento pra esse emulador |
| 500 | `internal` | o resto |

`429 busy` é o que impede dois devices de commitarem ao mesmo tempo no mesmo
emulador. O lock é por emulador, pego no `plan` e solto no `commit` ou na
expiração da sessão.

---

## 12. Fora do escopo da v1

- **Sync parcial por save**: o `sync_one` de hoje é push-only e continua como
  está. O protocolo sincroniza o conjunto.
- **Compressão no transporte**: save data comprime mal e o link é LAN.
- **Delta dentro do arquivo** (rsync/zsync): não compensa em arquivo de
  poucos MB.
- **Push do server pro client**: o client é quem inicia. Notificação
  server→client (pra sincronizar assim que outro device commitou) fica pra
  depois, sobre o mesmo SSE do progresso.
