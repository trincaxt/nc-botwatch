#!/usr/bin/env bash
# Radar contínuo: extrai arestas, roda triangulação e dispara pop-up no KDE se houver novidade.
cd "$(dirname "$0")" || exit 1
cargo build --release || exit 1

# Whitelist carregada automaticamente pelo report se whitelist.txt existir no mesmo dir.
# Para adicionar endereços conhecidos, edite whitelist.txt (um 0x... por linha, # = comentário).

while true; do
  ./target/release/edges edges > /dev/null
  SAIDA=$(./target/release/report "$@")
  { date -Is; echo "$SAIDA"; } | tee -a report.log

  # Dispara alerta visual no KDE Plasma somente quando houver cluster NOVO ou ATUALIZADO
  if echo "$SAIDA" | grep -qE "\[(NOVO|ATUALIZADO)\]"; then
    RESUMO=$(echo "$SAIDA" | grep -E "ALERTA DE CLUSTER|Conta-Mãe|Saída do Hub" | head -n 4)
    command -v notify-send > /dev/null && notify-send -u critical "🚨 nc-botwatch: Suspeito Detectado" "$RESUMO"
  fi

  sleep 600
done
