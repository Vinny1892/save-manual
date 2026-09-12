# save-sync — client Android

Ainda não implementado. Placeholder pra manter o lugar no monorepo.

## Por que Kotlin nativo e não PWA

Uma PWA não serve: `showDirectoryPicker()` não existe no Chrome Android
(o Android não tem um seletor de sistema que mapeie na File System Access
API), e o que sobra pra web — OPFS e `<input type="file">` — ou é sandbox do
browser, ou é cópia one-shot sem escrita de volta. Sem escrita de volta não
existe sync de save: o arquivo tem que voltar pra pasta do emulador.

## A restrição que define o escopo

O eden guarda os saves em
`Android/data/dev.eden.eden_emulator/files/nand/user/save/`. Desde o Android
11 esse diretório tem uma flag que impede até o SAF de conceder acesso —
`MANAGE_EXTERNAL_STORAGE` não resolve, e no Android 13+ nem o acesso por
`/sdcard/Android/data` via root funciona.

Os caminhos que funcionam, em ordem de preferência:

1. **Eden com pasta de dados customizável** — feature request aberta
   ([#251](https://github.com/eden-emulator/Issue-Reports/issues/251),
   [#252](https://github.com/eden-emulator/Issue-Reports/issues/252)).
   Quando existir, um app comum com `MANAGE_EXTERNAL_STORAGE` resolve e esta
   seção inteira vira obsoleta.
2. **Aparelho com root / ROM custom** — acessar por `/data/media/0/Android/data/...`
   em vez de `/sdcard/Android/data/...` contorna o scoped storage.
3. **Shizuku** — privilégio de shell sem root. Funciona, mas não sobrevive a
   reboot e o projeto original está sem manutenção.

## Protocolo

Fala o mesmo HTTP que o client de PC — ver `apps/server`. Sem rclone e sem
SMB: nenhum dos dois é viável de dentro de um app Android.
