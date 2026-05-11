# AUR packages

Templates de PKGBUILD pros dois pacotes que vamos publicar no [AUR](https://aur.archlinux.org/):

| Pacote | O que faz | Tempo de install |
|---|---|---|
| `save-manual-bin` | baixa o AppImage do release no GitHub | ~10s |
| `save-manual` | clona o repo, builda librclone + tauri from source | ~10 min |

## Publicar pela primeira vez

```bash
# 1. cria conta + sobe SSH pub key no AUR:
#    https://aur.archlinux.org/register
#    https://aur.archlinux.org/account → SSH Public Key

# 2. clona o "repo vazio" (cria sob demanda no push)
git clone ssh://aur@aur.archlinux.org/save-manual-bin.git
cd save-manual-bin

# 3. copia o PKGBUILD daqui
cp /caminho/save-manual/aur/save-manual-bin/PKGBUILD .

# 4. atualiza sha256sums (precisa do AppImage estar publicado)
updpkgsums

# 5. testa build local
makepkg -si

# 6. gera o .SRCINFO (OBRIGATÓRIO pelo AUR, é a versão indexável)
makepkg --printsrcinfo > .SRCINFO

# 7. commit + push
git add PKGBUILD .SRCINFO
git commit -m "0.1.0-1: initial import"
git push
```

Mesmos passos pra `save-manual`. Após o push, aparece em `aur.archlinux.org/packages/<nome>`.

## Atualizar (a cada release)

```bash
cd save-manual-bin
# atualiza pkgver + _release no PKGBUILD
sed -i 's/^pkgver=.*/pkgver=0.1.1/' PKGBUILD
sed -i 's/^_release=.*/_release=build-N/' PKGBUILD

updpkgsums
makepkg --printsrcinfo > .SRCINFO
git commit -am "0.1.1-1"
git push
```

## Limitação conhecida do `save-manual-bin`

A "versão" do AppImage no nome do arquivo segue `pkgver` (ex: `save-sync_0.1.0_amd64.AppImage`). Tauri usa o `version` do `tauri.conf.json` — se mudar lá, atualiza aqui também.

O `_release` aponta pra `build-N` no GitHub Releases. Cada push no master gera um build novo, então o número aumenta a cada commit. Se preferir tags semânticas estáveis (`v0.1.0`), troca o `_release` por `v$pkgver` e cria a tag manualmente após validar a build.

## Automatizar update via CI (futuro)

[`KSXGitHub/github-actions-deploy-aur`](https://github.com/KSXGitHub/github-actions-deploy-aur) abre PR ou commita direto no AUR a cada release. Precisa:

1. Gerar SSH key dedicada (sem passphrase)
2. Adicionar a pub key no AUR account
3. Armazenar a private key como secret `AUR_SSH_KEY` no repo GitHub
4. Adicionar um job no `.github/workflows/build.yml`

Setup vale a pena se for atualizar com frequência. Pra v1 manual tá ok.
