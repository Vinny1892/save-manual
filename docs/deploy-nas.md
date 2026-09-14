# Rodando o server no NAS

Guia do começo ao fim: subir o container, criar o usuário, parear o PC.

Escrito pensando num **UGREEN NASync DH4300 Plus** (Rockchip RK3588, arm64,
UGOS Pro), mas nada aqui é específico dele — serve em qualquer Docker.

---

## Antes de começar

| | |
|---|---|
| Arquitetura | a imagem é multi-arch (amd64 + arm64); o mesmo `latest` serve os dois |
| Espaço | a imagem tem ~60 MB. Os dados crescem com teus saves e o histórico |
| Onde os dados ficam | **num volume do array**, não no eMMC. O eMMC do DH4300 é soldado, tem 32 GB e não se troca |
| Rede | a porta 8787 precisa ser alcançável do PC |

---

## 1. Subir o container

### Pelo compose (recomendado)

Copie [`docker/compose.yaml`](../docker/compose.yaml) pro NAS, ajuste o
caminho do volume, e:

```bash
docker compose pull
docker compose up -d
docker compose logs -f
```

O log deve terminar com uma linha `save-sync-server addr=0.0.0.0:8787` e um
aviso de que ainda não há usuário — é o esperado no primeiro boot.

### Pela UI do UGOS

O UGOS tem uma tela de Docker que aceita imagem de registry. Preencha:

- **Imagem**: `ghcr.io/vinny1892/save-manual:latest`
- **Porta**: `8787` → `8787`
- **Volume**: uma pasta do array → `/data`
- **Reinício**: sempre

### Sem acesso ao registry

Cada build do CI publica também um tarball (`save-sync-server-arm64-tarball`
na aba Actions):

```bash
docker load < save-sync-server-arm64.tar.gz
```

---

## 2. Criar o primeiro usuário

O primeiro usuário **não nasce pela web UI**, porque a web UI exige estar
logado. Ele nasce pela CLI, e isso é proposital: exigir acesso de shell ao
NAS garante que ninguém na rede crie a primeira conta antes de você.

```bash
docker compose exec save-sync-server \
  save-sync-server --create-user vinicius
```

Sem senha na entrada, o server gera uma e **imprime uma vez só**. Se preferir
escolher a sua:

```bash
docker compose exec -T save-sync-server \
  sh -c 'echo "minha-senha-boa" | save-sync-server --create-user vinicius'
```

Mínimo de 8 caracteres. Cinco tentativas erradas travam a conta por 5
minutos — inclusive pra senha certa, que é o ponto.

---

## 3. Entrar

Abra `http://<ip-do-nas>:8787` e faça login. Do celular funciona igual: é a
mesma página.

Nesse momento a lista de emuladores aparece vazia — nada foi sincronizado
ainda. Os controles de path e watcher não aparecem no browser de propósito:
eles dependem da máquina onde o emulador roda, e quem os tem é o client de
PC.

---

## 4. Parear o PC

Na web UI logada, gere um **código de pareamento** — 8 caracteres, 10
minutos, uso único. Pela CLI dá no mesmo:

```bash
docker compose exec save-sync-server save-sync-server --pair
```

Depois é no client de PC. Ver [`client-pc.md`](client-pc.md).

---

## 5. Acesso de fora de casa

**Não exponha a porta 8787 direto na internet.** Duas saídas, em ordem de
preferência:

### Tailscale (mais simples)

Instale o Tailscale no NAS e no celular. O server passa a ser alcançável
pelo IP da tailnet de qualquer lugar, sem abrir porta no roteador e sem
certificado. Nada muda na configuração do save-sync.

### Reverse proxy com TLS

Se preferir um domínio de verdade, ponha um proxy (Caddy, nginx, ou o do
UGOS) na frente, com certificado. Nesse caso **ligue o cookie `Secure`**:

```yaml
environment:
  SAVE_SYNC_SECURE_COOKIE: "1"
```

E publique a porta só no loopback, pra que ninguém alcance o server por
fora do proxy:

```yaml
ports:
  - "127.0.0.1:8787:8787"
```

A razão do `Secure` ser opt-in e não padrão: ligado sempre, o login
quebraria em HTTP na LAN, que é o caso mais comum num NAS; desligado atrás
de HTTPS, o cookie viajaria em claro. Não existe default certo pros dois,
então é escolha explícita.

---

## Variáveis de ambiente

| Variável | Default | O que faz |
|---|---|---|
| `SAVE_SYNC_ADDR` | `0.0.0.0:8787` | endereço de escuta |
| `SAVE_SYNC_DATA` | `/data` | raiz dos dados persistentes |
| `SAVE_SYNC_WEB` | `/srv/web` | diretório da SPA (já vem na imagem) |
| `SAVE_SYNC_SECURE_COOKIE` | desligado | `1` marca o cookie de sessão como `Secure` |
| `RUST_LOG` | `info` | `debug` pra investigar sync |

---

## O que fica em `/data`

```
/data
├── save-sync-server.db       SQLite: índice, devices, usuários, sessões
├── live/<emulador>/          a árvore viva — o que os devices veem
├── .history/<emulador>/      snapshots, irmão de live/ e nunca dentro
├── staging/<sessão>/         uploads em voo, limpos no commit
├── titledb.json              ~83 MB, baixado no primeiro boot
└── ps2-gameindex.yaml        ~10 MB, idem
```

As duas bases de título são baixadas **em background** depois que o HTTP
sobe. Até terminarem, saves de Switch e PS2 aparecem com o id cru em vez do
nome do jogo. Isso é normal e se resolve sozinho.

`.history/` ser irmão de `live/` e não estar dentro dele é invariante, não
detalhe de arrumação: dentro, o histórico entraria no próprio sync e seria
replicado de volta pros devices.

### Backup

O que importa preservar é `/data` inteiro. O `save-sync-server.db` e a
árvore `live/` precisam ser consistentes entre si — o banco descreve o que
está nos arquivos. Copiar um sem o outro produz um estado que o server não
sabe interpretar.

Pra backup off-site, o server ainda carrega o rclone: replicar `/data` pra
S3/R2 é o caminho previsto (ainda não exposto na UI).

---

## Problemas comuns

**A web UI abre mas todo request dá 401.**
Sessão expirada ou cookie bloqueado. Se estiver atrás de HTTPS com
`SAVE_SYNC_SECURE_COOKIE` desligado, o browser pode recusar o cookie —
ligue. Se estiver em HTTP com ele ligado, desligue.

**O client de PC diz `resync_required`.**
O device ficou offline por mais tempo que a janela de tombstone (90 dias) e
o server já não sabe o que foi apagado nesse intervalo. O próximo sync
refaz o baseline sozinho; nada se perde.

**Um emulador responde `busy` (429).**
Há outro device com sync em andamento nesse emulador. O lock solta no
commit ou após 1h de inatividade da sessão.

**O log avisa "nenhum usuário cadastrado".**
Passo 2 não foi feito.

**A imagem não sobe no NAS: "exec format error".**
Arquitetura errada. O `latest` é multi-arch e o Docker escolhe sozinho; se
alguém fixou uma tag de arquitetura específica, volte pro `latest`.
