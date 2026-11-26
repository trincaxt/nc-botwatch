# nc-botwatch (fase 1: coletor)

    cargo run --release -- --url http://10.8.0.2:23061/graphql --db botwatch.db
    cargo run --release -- --follow            # fica acompanhando o tip

- Sem `--from` e sem banco prévio: começa em tip-1000. Depois retoma de `last_block`.
- Reexecutar é seguro (INSERT OR IGNORE).

# Fase 2: arestas e relatório

    cargo build --release
    ./target/release/edges edges          # decodifica raw -> tabela edges (transfer + market)
    ./target/release/report               # hubs: >= 8 origens em 50 blocos; gera report_<hub>.csv
    ./target/release/report --min-origens 5 --janela 100 --ignore-file ignorar.txt
    ./watch.sh                            # repete tudo a cada 10 min (deixe o coletor com --follow)

`ignorar.txt`: um endereço (minúsculo, 0x...) por linha. A ponte já é ignorada por padrão.
