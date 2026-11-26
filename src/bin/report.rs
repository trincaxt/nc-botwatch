// Radar de Triangulação Multi-Account & Bots (nc-botwatch)
// Cruza: (1) Convergência financeira (Hubs), (2) Funil de mercado,
//        (3) Comportamento robótico (baixa diversidade de ações) e (4) Saída/Ponte.
//
// Uso:
//   report [--db botwatch.db] [--min-origens 5] [--janela 60] [--ignore 0x..] [--ignore-file lista.txt]
use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, HashSet};
use std::fs;

const BRIDGE: &str = "0x1c2ae97380cfb4f732049e454f6d9a25d4967c6f";

struct Cfg {
    db: String,
    min_src: i64,
    window: i64,
    ignore: HashSet<String>,
}

fn parse_args() -> Cfg {
    let mut c = Cfg {
        db: "botwatch.db".into(),
        min_src: 5,
        window: 60,
        ignore: HashSet::new(),
    };
    c.ignore.insert(BRIDGE.to_string());
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--db" => c.db = it.next().expect("--db precisa de valor"),
            "--min-origens" => {
                c.min_src = it
                .next()
                .and_then(|v| v.parse().ok())
                .expect("--min-origens precisa de número")
            }
            "--janela" => {
                c.window = it
                .next()
                .and_then(|v| v.parse().ok())
                .expect("--janela precisa de número")
            }
            "--ignore" => {
                c.ignore
                .insert(it.next().expect("--ignore precisa de endereço").to_lowercase());
            }
            "--ignore-file" => {
                let p = it.next().expect("--ignore-file precisa de caminho");
                if let Ok(s) = fs::read_to_string(&p) {
                    for l in s.lines() {
                        if let Some(a) = l.split_whitespace().next() {
                            let a = a.to_lowercase();
                            if a.starts_with("0x") {
                                c.ignore.insert(a);
                            }
                        }
                    }
                }
            }
            o => eprintln!("argumento ignorado: {o}"),
        }
    }
    c
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn nivel_risco(score: u32) -> &'static str {
    match score {
        80..=100 => "CRÍTICO (Fazenda / Multi-Account)",
        55..=79 => "ALTO (Coordenação Forte)",
        35..=54 => "MÉDIO (Suspeito em Observação)",
        _ => "BAIXO",
    }
}

fn main() -> Result<()> {
    let cfg = parse_args();
    let conn = Connection::open(&cfg.db)?;

    let has_edges: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name='edges'",
                                        [],
                                        |r| r.get(0),
    )?;
    if has_edges == 0 {
        eprintln!("tabela edges não existe: rode `edges edges` antes");
        return Ok(());
    }

    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_txs_lsigner ON txs(lower(signer));
    CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(kind, dst, block_idx);
    CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(kind, src, block_idx);
    CREATE TABLE IF NOT EXISTS seen_hubs(
        hub TEXT PRIMARY KEY,
        last_origens INTEGER DEFAULT 0,
        last_total REAL DEFAULT 0.0,
        first_seen TEXT DEFAULT CURRENT_TIMESTAMP
    );",
    )?;
    let _ = conn.execute("ALTER TABLE seen_hubs ADD COLUMN last_origens INTEGER DEFAULT 0;", []);
    let _ = conn.execute("ALTER TABLE seen_hubs ADD COLUMN last_total REAL DEFAULT 0.0;", []);

    // 1. Busca clusters convergindo para um mesmo destino numa janela curta
    let rows: Vec<(String, i64, i64, f64, i64)> = {
        let mut st = conn.prepare(
            "SELECT a.dst, a.block_idx, COUNT(DISTINCT b.src), COALESCE(SUM(b.amount),0.0), MAX(b.block_idx)
        FROM (SELECT DISTINCT dst, block_idx FROM edges WHERE kind='transfer') a
        JOIN edges b ON b.kind='transfer' AND b.dst=a.dst
        AND b.block_idx BETWEEN a.block_idx AND a.block_idx + ?1
        GROUP BY a.dst, a.block_idx
        HAVING COUNT(DISTINCT b.src) >= ?2",
        )?;
        let it = st.query_map(params![cfg.window, cfg.min_src], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        it.collect::<Result<Vec<_>, _>>()?
    };

    let mut best: BTreeMap<String, (i64, i64, f64, i64)> = BTreeMap::new();
    for (dst, start, n, total, end) in rows {
        if cfg.ignore.contains(&dst) {
            continue;
        }
        let e = best.entry(dst).or_insert((start, n, total, end));
        if n > e.1 || (n == e.1 && total > e.2) {
            *e = (start, n, total, end);
        }
    }

    // Também checa o total acumulado global por hub (caso drene em ondas espaçadas > janela)
    let global_rows: Vec<(String, i64, i64, f64, i64)> = {
        let mut st = conn.prepare(
            "SELECT dst, MIN(block_idx), COUNT(DISTINCT src), COALESCE(SUM(amount),0.0), MAX(block_idx)
        FROM edges WHERE kind='transfer'
        GROUP BY dst HAVING COUNT(DISTINCT src) >= ?1",
        )?;
        let it = st.query_map(params![cfg.min_src], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        it.collect::<Result<Vec<_>, _>>()?
    };
    for (dst, start, n, total, end) in global_rows {
        if cfg.ignore.contains(&dst) {
            continue;
        }
        best.entry(dst).or_insert((start, n, total, end));
    }

    let mut hubs: Vec<(String, (i64, i64, f64, i64))> = best.into_iter().collect();
    hubs.sort_by(|a, b| (b.1).1.cmp(&(a.1).1));

    let mut alertas_emitidos = 0;

    for (hub, (start, n, total, end)) in hubs {
        let prev: Option<(i64, f64)> = conn
        .query_row(
            "SELECT last_origens, last_total FROM seen_hubs WHERE hub=?1",
            params![hub],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

        let status_tag = match prev {
            None => "[NOVO]",
            Some((po, pt)) if n > po || (total - pt).abs() > 0.01 => "[ATUALIZADO]",
            Some(_) => "[já visto]",
        };

        conn.execute(
            "INSERT INTO seen_hubs(hub, last_origens, last_total) VALUES (?1, ?2, ?3)
        ON CONFLICT(hub) DO UPDATE SET last_origens=excluded.last_origens, last_total=excluded.last_total",
                     params![hub, n, total],
        )?;

        let hub_txs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM txs WHERE lower(signer)=?1",
                                          params![hub],
                                          |r| r.get(0),
        )?;

        // Analisa cada conta-satélite que mandou NCG para o hub
        let srcs: Vec<(i64, String, String, f64, i64, i64, i64)> = {
            let mut st = conn.prepare(
                "SELECT e.block_idx, e.tx_id, e.src, COALESCE(e.amount,0.0),
                                      (SELECT COUNT(*) FROM txs t WHERE lower(t.signer)=e.src),
                                      (SELECT COUNT(DISTINCT a.type_id) FROM txs t JOIN actions a ON a.tx_id=t.id WHERE lower(t.signer)=e.src),
                                      (SELECT COUNT(*) FROM edges m WHERE m.kind='market' AND m.dst=e.src)
            FROM edges e
            WHERE e.kind='transfer' AND e.dst=?1 AND e.block_idx BETWEEN ?2 AND ?3
            ORDER BY e.block_idx, e.src",
            )?;
            let it = st.query_map(params![hub, start, end], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?;
            it.collect::<Result<Vec<_>, _>>()?
        };

        let exits: Vec<(i64, String, String, f64, String)> = {
            let mut st = conn.prepare(
                "SELECT block_idx, tx_id, dst, COALESCE(amount,0.0), COALESCE(memo,'')
            FROM edges WHERE kind='transfer' AND src=?1 AND block_idx >= ?2 ORDER BY block_idx",
            )?;
            let it = st.query_map(params![hub, start], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            it.collect::<Result<Vec<_>, _>>()?
        };

        let buyers: Vec<(String, i64, f64)> = {
            let mut st = conn.prepare(
                "SELECT src, COUNT(*), COALESCE(SUM(amount),0.0) FROM edges
                WHERE kind='market' AND dst IN
                (SELECT src FROM edges WHERE kind='transfer' AND dst=?1 AND block_idx BETWEEN ?2 AND ?3)
            GROUP BY src ORDER BY 2 DESC LIMIT 3",
            )?;
            let it = st.query_map(params![hub, start, end], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            it.collect::<Result<Vec<_>, _>>()?
        };

        // Triangulação dos vértices
        let mut robo_perfil: HashSet<&String> = HashSet::new();
        let mut vendedoras: HashSet<&String> = HashSet::new();
        let mut multiplos_092 = 0usize;

        for s in &srcs {
            // Conta-satélite sem diversidade de jogo real (<= 2 tipos de ação no histórico todo)
            if s.5 <= 2 {
                robo_perfil.insert(&s.2);
            }
            if s.6 > 0 {
                vendedoras.insert(&s.2);
            }
            // Checa se valor líquido bate com taxa de mercado de 8% (múltiplo de 0.92)
            let div = s.3 / 0.92;
            if s.3 > 0.0 && (div - div.round()).abs() < 0.005 {
                multiplos_092 += 1;
            }
        }

        // Cálculo do Score de Triangulação (0..100)
        let mut score: u32 = 0;
        let mut evidencias: Vec<String> = Vec::new();
        let span = end - start + 1;

        // Vértice 1: Convergência + Sincronia
        if n >= 15 && span <= 30 {
            score += 30;
            evidencias.push(format!(
                "[x] Varredura em rajada: {n} contas drenaram em {span} blocos"
            ));
        } else {
            score += 18;
            evidencias.push(format!(
                "[x] Convergência financeira: {n} contas drenaram em {span} blocos"
            ));
        }

        // Vértice 2: Comportamento robótico / multiaccount
        let pct_robo = (robo_perfil.len() as f64) / (n.max(1) as f64);
        if pct_robo >= 0.6 {
            score += 25;
            evidencias.push(format!(
                "[x] Baixa diversidade de ação: {}/{n} contas fazem <=2 tipos de ação no histórico observado (sinal fraco isolado: conta nova ou inativa também cai aqui)",
                                    robo_perfil.len()
            ));
        }

        // Vértice 3: Funil de mercado / lavagem
        if vendedoras.len() * 10 >= (n.max(1) as usize) * 3 {
            score += 15;
            if let Some((b, itens, vol)) = buyers.first() {
                evidencias.push(format!(
                    "[x] Funil de mercado: {}/{n} venderam {itens} itens ({vol:.2} NCG) para {b}",
                                        vendedoras.len()
                ));
            }
        }
        if multiplos_092 as f64 >= 0.5 * (n as f64) {
            score += 10;
            evidencias.push(format!(
                "[x] Valores de script: {multiplos_092}/{n} transferências são múltiplos exatos de 0,92 NCG (pós-taxa 8%)"
            ));
        }

        // Vértice 4: Saída / Monetização na Ponte
        let sai_ponte = exits.iter().any(|e| e.2 == BRIDGE);
        let sai_redondo = exits.iter().any(|e| e.3.fract() == 0.0 && e.3 >= 100.0);
        if sai_ponte {
            score += 15;
            evidencias.push("[x] Monetização: Hub despachou saldo direto para a PONTE Ethereum".into());
        }
        if hub_txs <= 3 || sai_redondo {
            score += 5;
            if hub_txs <= 3 {
                evidencias.push(format!("[x] Hub quase não assina txs: {hub_txs} no banco"));
            }
            if sai_redondo {
                evidencias.push("[x] Saída do hub em valor redondo (>= 100 NCG)".into());
            }
        }
        score = score.min(100);

        let path = format!("dossie_{}.csv", &hub[2..10]);
        let mut out = String::from(
            "papel,block_idx,tx_id,origem,destino,ncg,memo,total_txs_conta,tipos_acao_distintos,itens_vendidos\n",
        );
        for s in &srcs {
            out.push_str(&format!(
                "satelite,{},{},{},{},{:.2},,{},{},{}\n",
                s.0, s.1, s.2, hub, s.3, s.4, s.5, s.6
            ));
        }
        for e in &exits {
            out.push_str(&format!(
                "saida_hub,{},{},{},{},{:.2},{},,,\n",
                e.0,
                e.1,
                hub,
                e.2,
                e.3,
                q(&e.4)
            ));
        }
        fs::write(&path, out)?;

        alertas_emitidos += 1;
        println!(
            "\n🚨 {} ALERTA DE CLUSTER (Score: {score}/100 - {})",
                 status_tag,
                 nivel_risco(score)
        );
        println!("├── Conta-Mãe (Hub): {hub}");
        println!("├── Montante Drenado: {total:.2} NCG ({n} contas | blocos {start}..{end})");
        for e in &exits {
            let tag = if e.2 == BRIDGE { " [PONTE]" } else { "" };
            println!("├── Saída do Hub:    {:.2} NCG -> {}{} (bloco {})", e.3, e.2, tag, e.0);
        }
        println!("├── Triangulação:");
        for (idx, ev) in evidencias.iter().enumerate() {
            let branch = if idx + 1 == evidencias.len() { "└──" } else { "├──" };
            println!("│   {branch} {ev}");
        }
        println!("└── Dossiê pronto:   {path}");
    }

    // 2. Anéis de mercado: comprador e vendedor recebem transferência do MESMO financiador
    let loops: Vec<(String, String, i64, f64, Option<String>)> = {
        let mut st = conn.prepare(
            "SELECT m.src, m.dst, COUNT(*), ROUND(SUM(m.amount),2),
                                  (SELECT f1.src FROM edges f1
                                  JOIN edges f2 ON f2.kind='transfer' AND f2.src=f1.src AND f2.dst=m.dst
                                  WHERE f1.kind='transfer' AND f1.dst=m.src LIMIT 1)
        FROM edges m
        WHERE m.kind='market'
        GROUP BY m.src, m.dst
        HAVING COUNT(*) >= 4
        ORDER BY 3 DESC LIMIT 50",
        )?;
        let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        it.collect::<Result<Vec<_>, _>>()?
    };

    let aneis: Vec<_> = loops.into_iter().filter(|l| l.4.is_some()).collect();
    if !aneis.is_empty() {
        println!("\n⚠️  ANÉIS DE MERCADO (comprador e vendedor financiados pela mesma conta):");
        for (comprador, vendedor, itens, vol, fin) in aneis {
            alertas_emitidos += 1;
            println!(
                "├── {} financiou {comprador} (comprador) e {vendedor} (vendedor): {itens} itens, {vol:.2} NCG",
                     fin.unwrap_or_default()
            );
        }
    }

    if alertas_emitidos == 0 {
        println!("✅ Nenhum cluster suspeito detectado na janela atual.");
    }

    Ok(())
}
