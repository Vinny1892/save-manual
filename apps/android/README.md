# save-sync — client Android

**Estado: o protocolo está implementado e testado; o app não existe.**

Essa divisão é deliberada, e a razão está abaixo.

```
apps/android/
├── protocol/          módulo Kotlin/JVM — implementado, com testes
└── (app/)             módulo Android — não iniciado
```

## O que existe

`protocol/` é um módulo **Kotlin/JVM puro**, sem nada de Android. Contém o
client completo do protocolo (`docs/protocol.md`): tipos do wire, varredura
com reuso de hash, diff contra baseline, e o ciclo plan → transferir →
commit → aplicar.

```bash
cargo build -p save-sync-server      # o teste roda o server de verdade
cd apps/android && ./gradlew test
```

Os testes incluem **dois de integração contra o binário do server em Rust**:
dois devices convergindo, deleção propagando, e conflito preservando as duas
versões. É isso que dá sentido a este módulo existir separado: o risco caro
aqui é o wire format divergir entre Kotlin e Rust, e isso só aparece com os
dois rodando. Nada disso precisa de aparelho nem emulador.

O acesso ao disco está atrás da interface `SaveStorage`, com uma
implementação por filesystem comum (`LocalFileStorage`). Isso não é
abstração por gosto — é a seção seguinte.

## Por que o app não foi escrito

**A restrição não é de código.** O eden guarda os saves em
`Android/data/dev.eden.eden_emulator/files/nand/user/save/`, e desde o
Android 11 esse diretório tem uma flag que impede até o SAF de conceder
acesso. `MANAGE_EXTERNAL_STORAGE` não resolve, e no Android 13+ nem o acesso
por `/sdcard/Android/data` com root funciona.

O Syncthing, que é maduro e dedicado a sincronizar arquivos, bate na mesma
parede — a documentação dele diz explicitamente pra não apontar o app pra
`Android/data/`.

Os caminhos que funcionam, em ordem de preferência:

1. **eden implementar pasta de dados customizável** — feature request aberta
   ([#251](https://github.com/eden-emulator/Issue-Reports/issues/251),
   [#252](https://github.com/eden-emulator/Issue-Reports/issues/252)). Azahar
   e PPSSPP já fazem. Quando existir, `LocalFileStorage` + permissão comum
   resolve e esta seção fica obsoleta
2. **Aparelho com root ou ROM custom** — acessar por
   `/data/media/0/Android/data/...` em vez de `/sdcard/Android/data/...`
   contorna o scoped storage. É o que funciona hoje em handhelds
   (GammaOS, Anbernic e afins), e o `LocalFileStorage` atende direto
3. **Shizuku** — privilégio de shell sem root. Funciona, mas não sobrevive a
   reboot, morre em algumas trocas de rede, e o projeto original está sem
   manutenção

**Qual dos três o app precisa implementar depende do aparelho** — e ele
ainda não existe. Escrever a camada de permissões, o seletor de pasta e o
serviço de background agora significaria escrever pra um alvo suposto, sem
poder rodar nada. O que dava pra construir com verificação real foi
construído; o resto espera o aparelho.

PWA foi descartado num passo anterior e não volta: `showDirectoryPicker()`
não existe no Chrome Android, e sem escrita de volta não existe sync de save
— o arquivo tem que retornar à pasta do emulador.

## O que falta, quando houver aparelho

- módulo `app` com Android Gradle Plugin, `minSdk` decidido pelo aparelho
- implementação de `SaveStorage` pra estratégia escolhida (SAF, Shizuku ou
  caminho direto)
- tela de pareamento (o código de 8 caracteres vem da web UI do server)
- serviço em background com o gatilho de sync — no Android não há
  equivalente ao proc-watch do desktop, então provavelmente é sync ao abrir
  o app e agendado via `WorkManager`
- armazenar `device_token` e baseline (Room ou SQLite direto)

## Notas de toolchain

- `protocol/` **não** usa o Android Gradle Plugin de propósito: tem que
  buildar e testar numa máquina sem SDK do Android
- OkHttp e não `java.net.http` — a segunda não existe no Android, e este
  código vai inteiro pro app
- o alvo de bytecode é fixado em 17 sem exigir um JDK 17 instalado, senão o
  build falharia em máquina que só tem o JBR do Android Studio
