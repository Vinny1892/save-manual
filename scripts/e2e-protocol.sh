#!/usr/bin/env bash
# Ciclo completo do protocolo de sync contra o server rodando de verdade.
# Dois devices pareados: um sobe um save, o outro tem que recebe-lo.
#
# Requer: cargo build -p save-sync-server
# Uso:    bash scripts/e2e-protocol.sh
set -uo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BIN=./target/debug/save-sync-server
[ -x "$BIN" ] || BIN=./target/debug/save-sync-server.exe
if [ ! -x "$BIN" ]; then
  echo "binário não encontrado — rode: cargo build -p save-sync-server" >&2
  exit 1
fi

DATA=./.e2e
PORT=${PORT:-8791}
URL=http://127.0.0.1:$PORT
ok=0; fail=0

check() { # check <descricao> <esperado> <obtido>
  if [ "$2" == "$3" ]; then echo "  ok   $1"; ok=$((ok+1))
  else echo "  FALHA $1 — esperado [$2], obtido [$3]"; fail=$((fail+1)); fi
}

rm -rf "$DATA"; mkdir -p "$DATA"

CODE2=$(SAVE_SYNC_DATA=$DATA $BIN --pair 2>/dev/null | head -1 | sed 's/.*: //')
CODE1=$(SAVE_SYNC_DATA=$DATA $BIN --pair 2>/dev/null | head -1 | sed 's/.*: //')

SAVE_SYNC_ADDR=127.0.0.1:$PORT SAVE_SYNC_WEB=apps/web/build SAVE_SYNC_DATA=$DATA $BIN >/dev/null 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
for _ in $(seq 1 40); do curl -sf $URL/health >/dev/null 2>&1 && break; done

echo "== pareamento =="
T1=$(curl -s -X POST $URL/api/v1/pair -H 'content-type: application/json' \
     -d "{\"code\":\"$CODE1\",\"device_name\":\"pc\",\"platform\":\"windows\"}" \
     | sed -n 's/.*"device_token":"\([^"]*\)".*/\1/p')
check "device 1 recebeu token" 64 "${#T1}"

DUP=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/pair \
      -H 'content-type: application/json' -d "{\"code\":\"$CODE1\"}")
check "código de uso único recusa segunda vez" 403 "$DUP"

T2=$(curl -s -X POST $URL/api/v1/pair -H 'content-type: application/json' \
     -d "{\"code\":\"$CODE2\",\"device_name\":\"celular\",\"platform\":\"android\"}" \
     | sed -n 's/.*"device_token":"\([^"]*\)".*/\1/p')
check "device 2 recebeu token" 64 "${#T2}"

echo "== autenticação =="
NOAUTH=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/sync/eden/plan \
         -H 'content-type: application/json' -d '{"last_rev":0,"changes":[]}')
check "plan sem token é 401" 401 "$NOAUTH"

BADEMU=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/sync/n64/plan \
         -H "authorization: Bearer $T1" -H 'content-type: application/json' \
         -d '{"last_rev":0,"changes":[]}')
check "emulador desconhecido é 404" 404 "$BADEMU"

echo "== path traversal =="
EVIL=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/sync/eden/plan \
       -H "authorization: Bearer $T1" -H 'content-type: application/json' \
       -d '{"last_rev":0,"changes":[{"path":"../../etc/passwd","op":"put","size":1,"mtime":1,"hash":"x"}]}')
check "traversal é 400" 400 "$EVIL"

OUT=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/sync/eden/plan \
      -H "authorization: Bearer $T1" -H 'content-type: application/json' \
      -d '{"last_rev":0,"changes":[{"path":"system/Contents/x.nca","op":"put","size":1,"mtime":1,"hash":"x"}]}')
check "fora da whitelist do eden é 400" 400 "$OUT"

echo "== device 1 sobe um save =="
CONTENT="save do zelda"
HASH=$(printf '%s' "$CONTENT" | sha256sum | cut -d' ' -f1)
P="user/save/0000/0100abc/game.sav"

PLAN=$(curl -s -X POST $URL/api/v1/sync/eden/plan -H "authorization: Bearer $T1" \
       -H 'content-type: application/json' \
       -d "{\"last_rev\":0,\"changes\":[{\"path\":\"$P\",\"op\":\"put\",\"size\":${#CONTENT},\"mtime\":1000,\"hash\":\"$HASH\"}]}")
S1=$(echo "$PLAN" | sed -n 's/.*"session":"\([^"]*\)".*/\1/p')
check "plan pediu upload do arquivo" 1 "$(echo "$PLAN" | grep -o "\"upload\":\[{\"path\":\"$P\"}\]" | wc -l)"

BADHASH=$(printf '%s' "$CONTENT" | curl -s -o /dev/null -w "%{http_code}" -X PUT \
          "$URL/api/v1/sync/eden/blob?session=$S1&path=$P" -H "authorization: Bearer $T1" \
          -H "x-save-sync-hash: 0000000000000000000000000000000000000000000000000000000000000000" \
          -H "x-save-sync-mtime: 1000" --data-binary @-)
check "hash errado é 422" 422 "$BADHASH"

EARLY=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/sync/eden/commit \
        -H "authorization: Bearer $T1" -H 'content-type: application/json' -d "{\"session\":\"$S1\"}")
check "commit sem o upload é 409" 409 "$EARLY"

UP=$(printf '%s' "$CONTENT" | curl -s -o /dev/null -w "%{http_code}" -X PUT \
     "$URL/api/v1/sync/eden/blob?session=$S1&path=$P" -H "authorization: Bearer $T1" \
     -H "x-save-sync-hash: $HASH" -H "x-save-sync-mtime: 1000" --data-binary @-)
check "upload aceito" 200 "$UP"

AGAIN=$(printf '%s' "$CONTENT" | curl -s -X PUT \
        "$URL/api/v1/sync/eden/blob?session=$S1&path=$P" -H "authorization: Bearer $T1" \
        -H "x-save-sync-hash: $HASH" -H "x-save-sync-mtime: 1000" --data-binary @-)
check "reenviar é idempotente" 1 "$(echo "$AGAIN" | grep -c ja_recebido)"

COMMIT=$(curl -s -X POST $URL/api/v1/sync/eden/commit -H "authorization: Bearer $T1" \
         -H 'content-type: application/json' -d "{\"session\":\"$S1\"}")
check "commit devolve rev 1" 1 "$(echo "$COMMIT" | sed -n 's/.*"new_rev":\([0-9]*\).*/\1/p')"
check "arquivo está na árvore viva" "$CONTENT" "$(cat "$DATA/live/eden/$P" 2>/dev/null)"
check "staging foi limpo" 0 "$(ls "$DATA/staging" 2>/dev/null | wc -l)"

echo "== device 2 recebe o save =="
PLAN2=$(curl -s -X POST $URL/api/v1/sync/eden/plan -H "authorization: Bearer $T2" \
        -H 'content-type: application/json' -d '{"last_rev":0,"changes":[]}')
check "plan do device 2 pede download" 1 "$(echo "$PLAN2" | grep -c "\"download\":\[{\"path\":\"$P\"")"

GOT=$(curl -s "$URL/api/v1/sync/eden/blob?path=$P&rev=1" -H "authorization: Bearer $T2")
check "download devolve o conteúdo" "$CONTENT" "$GOT"

STALE=$(curl -s -o /dev/null -w "%{http_code}" "$URL/api/v1/sync/eden/blob?path=$P&rev=99" \
        -H "authorization: Bearer $T2")
check "rev errada é 409" 409 "$STALE"

echo "== lock por emulador =="
S2=$(echo "$PLAN2" | sed -n 's/.*"session":"\([^"]*\)".*/\1/p')
BUSY=$(curl -s -o /dev/null -w "%{http_code}" -X POST $URL/api/v1/sync/eden/plan \
       -H "authorization: Bearer $T1" -H 'content-type: application/json' \
       -d '{"last_rev":1,"changes":[]}')
check "segundo device concorrente é 429" 429 "$BUSY"

echo "== sessão de outro device =="
STEAL=$(printf 'x' | curl -s -o /dev/null -w "%{http_code}" -X PUT \
        "$URL/api/v1/sync/eden/blob?session=$S2&path=$P" -H "authorization: Bearer $T1" \
        -H "x-save-sync-hash: $(printf 'x' | sha256sum | cut -d' ' -f1)" --data-binary @-)
check "escrever na sessão alheia é 403" 403 "$STEAL"

echo
echo "== $ok passaram, $fail falharam =="
[ "$fail" -eq 0 ]
