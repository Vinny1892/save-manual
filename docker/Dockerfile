# Imagem do save-sync-server.
#
# Publicada pelo CI em ghcr.io/vinny1892/save-manual, multi-arch (amd64 +
# arm64). O NAS alvo é um UGREEN DH4300 Plus — Rockchip RK3588, arm64.
#
# Esta imagem NÃO compila: espera um contexto já montado, produzido pelo CI
# no runner **nativo** de cada arquitetura. Compilar Rust sob emulação QEMU
# custa dezenas de minutos; buildar nativo e só empacotar aqui leva segundos.
#
# Contexto esperado:
#   save-sync-server      binário do alvo
#   librclone.so          artefato do scripts/build-librclone.sh
#   web/                  saída do `npm run build -w @save-sync/web`
#
# Pra buildar à mão (a partir da raiz, com dist/ montado como o CI monta):
#   docker buildx build -f docker/Dockerfile -t save-sync-server dist/

FROM --platform=$TARGETPLATFORM debian:bookworm-slim

# `image.source` é o que faz o pacote no ghcr aparecer ligado ao repositório
# — sem ele a página do pacote fica órfã, sem README nem link pro código.
LABEL org.opencontainers.image.source="https://github.com/Vinny1892/save-manual" \
      org.opencontainers.image.description="save-sync server — sync de save data de emuladores" \
      org.opencontainers.image.licenses="MIT"

# ca-certificates: o server baixa as title DBs (blawar, PCSX2 GameIndex) por
# HTTPS e fala com S3/R2 via rclone.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY save-sync-server /usr/local/bin/save-sync-server
COPY librclone.so /usr/local/lib/librclone.so
COPY web/ /srv/web/

RUN chmod +x /usr/local/bin/save-sync-server

# rclone.rs procura o librclone primeiro pelo nome puro, que resolve via
# LD_LIBRARY_PATH — por isso /usr/local/lib basta, sem ldconfig.
ENV LD_LIBRARY_PATH=/usr/local/lib \
    SAVE_SYNC_ADDR=0.0.0.0:8787 \
    SAVE_SYNC_WEB=/srv/web \
    SAVE_SYNC_DATA=/data

# /data guarda o SQLite, as title DBs e os saves. Tem que ser volume no array
# do NAS, não no eMMC de 32 GB soldado.
VOLUME ["/data"]
EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD ["/usr/local/bin/save-sync-server", "--health-check"]

ENTRYPOINT ["/usr/local/bin/save-sync-server"]
