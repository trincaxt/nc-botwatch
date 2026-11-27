// Radar de Triangulação Multi-Account & Bots (nc-botwatch) — v2
// Vértices: (1) Convergência financeira, (2) Perfil robótico refinado,
//           (3) Funil de mercado, (4) Saída/Ponte,
//           (5) Nonce sequencial, (6) Timing intra-bloco.
// Anti-FP: score proporcional, whitelist de reward values, descontadores
//           por gameplay real, janela temporal nos anéis de mercado.
// Output: terminal colorido, CSV enriquecido, NDJSON opcional (--json).
//
// Uso:
//   report [--db botwatch.db] [--min-origens 5] [--janela 60]
//          [--ignore 0x..] [--ignore-file lista.txt]
//          [--json] [--somente-novos] [--inativo-blocos 1000] [--score-min 25]
use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, HashSet};
use std::fs;

const BRIDGE: &str = "0x1c2ae97380cfb4f732049e454f6d9a25d4967c6f";

// Valores de NCG que são rewards normais do jogo — não flagrar como script 0.92×
// (enchantment rewards, crafting, daily, event prizes comuns)
const REWARD_VALUES_NCG: &[f64] = &[
    1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 10.0, 11.5, 12.0, 13.8, 15.0,
    20.0, 23.0, 25.0, 46.0, 50.0, 100.0, 115.0, 230.0,
];

// (lista de gameplay types usada em edges.rs — mantida aqui apenas como referência documental)

struct Cfg {
    db: String,
    min_src: i64,
    window: i64,
    ignore: HashSet<String>,
    json: bool,
    only_new: bool,
    inactive_blocks: i64,
    score_min: u32,
}

/// Carrega um arquivo de whitelist (um endereço por linha, # = comentário)
fn load_whitelist(path: &str, set: &mut HashSet<String>) -> usize {
    let Ok(contents) = fs::read_to_string(path) else { return 0 };
    let mut n = 0;
    for line in contents.lines() {
        // ignora comentários e texto após '#'
        let clean = line.splitn(2, '#').next().unwrap_or("").trim();
        if clean.starts_with("0x") {
            set.insert(clean.to_lowercase());
            n += 1;
        }
    }
    n
}

fn parse_args() -> Cfg {
    let mut c = Cfg {
        db: "botwatch.db".into(),
        min_src: 5,
        window: 60,
        ignore: HashSet::new(),
        json: false,
        only_new: false,
        inactive_blocks: 1000,
        score_min: 25,
    };
    // Bridge sempre ignorado
    c.ignore.insert(BRIDGE.to_string());

    // Carrega whitelist.txt automaticamente se existir no diretório atual
    const DEFAULT_WHITELIST: &str = "whitelist.txt";
    let n = load_whitelist(DEFAULT_WHITELIST, &mut c.ignore);
    if n > 0 {
        eprintln!("whitelist: {n} endereço(s) carregado(s) de {DEFAULT_WHITELIST}");
    }
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
            "--json" => c.json = true,
            "--verbose" => c.only_new = false,
            "--somente-novos" => c.only_new = true,
            "--inativo-blocos" => {
                c.inactive_blocks = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--inativo-blocos precisa de número");
            }
            "--score-min" => {
                c.score_min = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--score-min precisa de número");
            }
            o => eprintln!("argumento ignorado: {o}"),
        }
    }
    c
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn nivel_risco(score: u32) -> &'static str {
    match score {
        80..=100 => "CRÍTICO (Fazenda / Multi-Account)",
        55..=79  => "ALTO (Coordenação Forte)",
        35..=54  => "MÉDIO (Suspeito em Observação)",
        25..=34  => "BAIXO (Monitoramento)",
        _        => "NEGLIGÍVEL",
    }
}

/// Verifica se um valor de NCG é um reward natural do jogo (não script)
fn is_reward_value(ncg: f64) -> bool {
    REWARD_VALUES_NCG.iter().any(|&r| (ncg - r).abs() < 0.01)
}

/// Conta nonces sem buracos (sequência perfeita → sinal de bot)
fn nonce_gaps(conn: &Connection, signer: &str) -> i64 {
    // LAG disponível desde SQLite 3.25 (2018) — OK
    conn.query_row(
        "SELECT COUNT(*) FROM (
            SELECT nonce,
                   LAG(nonce) OVER (ORDER BY nonce) AS prev
            FROM txs WHERE lower(signer)=?1 AND nonce IS NOT NULL
         ) WHERE nonce - prev > 1",
        params![signer],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(999) // se falhar (poucas txs) devolve valor alto → não penaliza
}

/// Span em segundos entre a primeira e a última tx das contas-satélite no cluster
fn timing_span_seconds(conn: &Connection, signers: &[String]) -> Option<f64> {
    if signers.is_empty() {
        return None;
    }
    // monta placeholders: ?1, ?2, ...
    let placeholders: Vec<String> = (1..=signers.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT MAX(CAST(strftime('%s', ts) AS REAL)) - MIN(CAST(strftime('%s', ts) AS REAL))
         FROM txs WHERE lower(signer) IN ({})",
        placeholders.join(",")
    );
    let mut st = conn.prepare(&sql).ok()?;
    // bind cada parâmetro dinamicamente
    let result: Option<f64> = {
        let mut rows = st.query(rusqlite::params_from_iter(signers.iter())).ok()?;
        rows.next().ok()??.get(0).ok()?
    };
    result
}

/// Conta contas-satélite com gameplay real registrado na tabela gameplay_flags
fn gameplay_count(conn: &Connection, signers: &[String]) -> i64 {
    if signers.is_empty() {
        return 0;
    }
    let has_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='gameplay_flags'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_table == 0 {
        return 0;
    }
    let placeholders: Vec<String> = (1..=signers.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT COUNT(DISTINCT lower(signer)) FROM gameplay_flags WHERE lower(signer) IN ({})",
        placeholders.join(",")
    );
    conn.prepare(&sql)
        .ok()
        .and_then(|mut st| {
            st.query_row(rusqlite::params_from_iter(signers.iter()), |r| r.get(0))
                .ok()
        })
        .unwrap_or(0)
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

    // Índices e tabelas auxiliares
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_txs_lsigner  ON txs(lower(signer));
         CREATE INDEX IF NOT EXISTS idx_edges_dst    ON edges(kind, dst, block_idx);
         CREATE INDEX IF NOT EXISTS idx_edges_src    ON edges(kind, src, block_idx);
         CREATE INDEX IF NOT EXISTS idx_txs_nonce    ON txs(lower(signer), nonce);
         CREATE TABLE IF NOT EXISTS seen_hubs(
             hub         TEXT PRIMARY KEY,
             last_origens INTEGER DEFAULT 0,
             last_total   REAL    DEFAULT 0.0,
             first_seen   TEXT    DEFAULT CURRENT_TIMESTAMP
         );
         CREATE TABLE IF NOT EXISTS hub_score_history(
             hub        TEXT    NOT NULL,
             block_idx  INTEGER NOT NULL,
             score      INTEGER NOT NULL,
             origens    INTEGER NOT NULL,
             total_ncg  REAL    NOT NULL,
             recorded_at TEXT   DEFAULT CURRENT_TIMESTAMP,
             PRIMARY KEY (hub, block_idx)
         );",
    )?;
    // migrações silenciosas (caso tabela existisse sem coluna)
    let _ = conn.execute("ALTER TABLE seen_hubs ADD COLUMN last_origens INTEGER DEFAULT 0;", []);
    let _ = conn.execute("ALTER TABLE seen_hubs ADD COLUMN last_total   REAL    DEFAULT 0.0;", []);
    let _ = conn.execute("ALTER TABLE seen_hubs ADD COLUMN last_score   INTEGER DEFAULT 0;", []);

    // ── 1. Busca clusters convergindo para um mesmo destino ────────────────
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

    // Global: captura drenagem em ondas espaçadas além da janela
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
    let tip_block: i64 = conn
        .query_row("SELECT COALESCE(MAX(idx), 0) FROM blocks", [], |r| r.get(0))
        .unwrap_or(0);

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

        // Opcionalmente suprimir hubs já vistos sem mudanças se --somente-novos for passado
        if status_tag == "[já visto]" && cfg.only_new {
            conn.execute(
                "INSERT INTO seen_hubs(hub, last_origens, last_total) VALUES (?1, ?2, ?3)
                 ON CONFLICT(hub) DO UPDATE SET last_origens=excluded.last_origens,
                                               last_total=excluded.last_total",
                params![hub, n, total],
            )?;
            continue;
        }

        let last_hub_tx: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(block_idx), 0) FROM txs WHERE lower(signer)=?1",
                params![hub],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let last_hub_edge: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(block_idx), 0) FROM edges WHERE src=?1 OR dst=?1",
                params![hub],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let last_act = end.max(last_hub_tx).max(last_hub_edge);
        let blocks_since = (tip_block - last_act).max(0);
        let is_active = blocks_since <= cfg.inactive_blocks;
        let activity_tag = if is_active { "🟢 ATIVO" } else { "⚫ OFF" };

        let hub_txs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM txs WHERE lower(signer)=?1",
            params![hub],
            |r| r.get(0),
        )?;

        // ── Satélites: agora com timestamp da tx ──────────────────────────
        // cols: block_idx, tx_id, src, amount, total_txs, tipos_acao, itens_vendidos, ts
        let srcs: Vec<(i64, String, String, f64, i64, i64, i64, String)> = {
            let mut st = conn.prepare(
                "SELECT e.block_idx, e.tx_id, e.src, COALESCE(e.amount,0.0),
                        (SELECT COUNT(*)               FROM txs t WHERE lower(t.signer)=e.src),
                        (SELECT COUNT(DISTINCT a.type_id)
                           FROM txs t JOIN actions a ON a.tx_id=t.id
                          WHERE lower(t.signer)=e.src),
                        (SELECT COUNT(*) FROM edges m WHERE m.kind='market' AND m.dst=e.src),
                        COALESCE(
                            (SELECT t.ts FROM txs t WHERE t.id=e.tx_id LIMIT 1),
                            '')
                 FROM edges e
                 WHERE e.kind='transfer' AND e.dst=?1
                   AND e.block_idx BETWEEN ?2 AND ?3
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
                    r.get(7)?,
                ))
            })?;
            it.collect::<Result<Vec<_>, _>>()?
        };

        let exits: Vec<(i64, String, String, f64, String)> = {
            let mut st = conn.prepare(
                "SELECT block_idx, tx_id, dst, COALESCE(amount,0.0), COALESCE(memo,'')
                 FROM edges WHERE kind='transfer' AND src=?1 AND block_idx >= ?2
                 ORDER BY block_idx",
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
                   (SELECT src FROM edges WHERE kind='transfer' AND dst=?1
                    AND block_idx BETWEEN ?2 AND ?3)
                 GROUP BY src ORDER BY 2 DESC LIMIT 3",
            )?;
            let it = st.query_map(params![hub, start, end], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            it.collect::<Result<Vec<_>, _>>()?
        };

        // ── Triangulação — coleta sinais ──────────────────────────────────
        let mut robo_certo: HashSet<&String> = HashSet::new();   // bot provável
        let mut robo_fraco: HashSet<&String> = HashSet::new();   // conta nova/inativa
        let mut gameplay_sats: HashSet<&String> = HashSet::new(); // gameplay confirmado
        let mut vendedoras: HashSet<&String> = HashSet::new();
        let mut multiplos_092 = 0usize;

        for s in &srcs {
            let tipos = s.5;
            let total_txs = s.4;
            let tem_venda = s.6 > 0;
            let ncg = s.3;

            // Classificação de robô refinada:
            // - poucos tipos E poucas txs → bot provável
            // - poucos tipos E muitas txs → conta velha passiva (sinal fraco)
            if tipos <= 2 && total_txs <= 15 {
                robo_certo.insert(&s.2);
            } else if tipos <= 2 && total_txs > 15 {
                robo_fraco.insert(&s.2);
            } else {
                // tipos > 2 → gameplay detectado
                gameplay_sats.insert(&s.2);
            }

            if tem_venda {
                vendedoras.insert(&s.2);
            }

            // Múltiplos de 0,92 (pós-taxa 8%) — com whitelist de rewards normais
            if ncg > 0.0 && !is_reward_value(ncg) {
                let div = ncg / 0.92;
                if (div - div.round()).abs() < 0.003 {
                    multiplos_092 += 1;
                }
            }
        }

        // ── Vértice 5: Nonce sequencial (por conta distinta) ──────────────
        let mut seen_nonce_signers: HashSet<&String> = HashSet::new();
        let mut elegíveis = 0usize;
        let mut nonces_perfeitos = 0usize;
        for s in &srcs {
            if s.4 >= 10 && seen_nonce_signers.insert(&s.2) {
                elegíveis += 1;
                let gaps = nonce_gaps(&conn, &s.2);
                if gaps == 0 {
                    nonces_perfeitos += 1;
                }
            }
        }

        // ── Vértice 6: Timing intra-bloco (contas distintas) ──────────────
        let sat_signers: Vec<String> = {
            let uniq: HashSet<&String> = srcs.iter().map(|s| &s.2).collect();
            uniq.into_iter().cloned().collect()
        };
        let timing_span = timing_span_seconds(&conn, &sat_signers);

        // ── Gameplay real nas satélites ───────────────────────────────────
        let gameplay_confirmados = gameplay_count(&conn, &sat_signers);

        // ── Cálculo do Score (0–100) ──────────────────────────────────────
        let mut score: u32 = 0;
        let mut evidencias: Vec<String> = Vec::new();
        let span = end - start + 1;
        let density = n as f64 / span.max(1) as f64;

        // V1: Convergência proporcional à densidade
        let v1 = match (n, span) {
            (n, s) if n >= 20 && s <= 10 => { 35 }
            (n, s) if n >= 15 && s <= 20 => { 30 }
            (n, s) if n >= 15 && s <= 60 => { 22 }
            (n, _) if n >= 10            => { 15 }
            (n, _) if n >= 5             => { 8  }
            _                            => { 0  }
        };
        score += v1;
        if v1 > 0 {
            evidencias.push(format!(
                "[x] V1 Convergência: {n} contas/{span} blocos (densidade {density:.2}/bloco) → +{v1}pts"
            ));
        }

        // V2: Perfil robótico — só bot_certo pontua; bot_fraco é info
        let pct_certo = (robo_certo.len() as f64) / (n.max(1) as f64);
        if pct_certo >= 0.6 {
            score += 25;
            evidencias.push(format!(
                "[x] V2 Robôs confirmados: {}/{n} contas com ≤2 tipos de ação E ≤15 txs totais → +25pts",
                robo_certo.len()
            ));
        } else if pct_certo >= 0.3 {
            score += 12;
            evidencias.push(format!(
                "[x] V2 Robôs parciais: {}/{n} contas com perfil bot provável → +12pts",
                robo_certo.len()
            ));
        }
        if !robo_fraco.is_empty() {
            evidencias.push(format!(
                "[~] V2 Contas velhas passivas (sinal fraco): {}/{n} — pode ser jogador inativo",
                robo_fraco.len()
            ));
        }

        // V3: Funil de mercado
        if vendedoras.len() * 10 >= (n.max(1) as usize) * 3 {
            score += 15;
            if let Some((b, itens, vol)) = buyers.first() {
                evidencias.push(format!(
                    "[x] V3 Funil de mercado: {}/{n} venderam → comprador recorrente {b} ({itens} itens, {vol:.2} NCG) → +15pts",
                    vendedoras.len()
                ));
            } else {
                evidencias.push(format!(
                    "[x] V3 Funil de mercado: {}/{n} satélites venderam itens no mercado → +15pts",
                    vendedoras.len()
                ));
            }
        }
        let total_transfers = srcs.len().max(1);
        if multiplos_092 as f64 >= 0.5 * (total_transfers as f64) {
            score += 10;
            evidencias.push(format!(
                "[x] V3 Valores de script: {multiplos_092}/{total_transfers} transferências são múltiplos de 0,92 (pós-taxa 8%, excluídos rewards normais) → +10pts"
            ));
        }

        // V4: Saída / Monetização
        let sai_ponte = exits.iter().any(|e| e.2 == BRIDGE);
        let sai_redondo = exits.iter().any(|e| e.3.fract() == 0.0 && e.3 >= 100.0);
        if sai_ponte {
            score += 20; // elevado: ponte = monetização quase certa
            evidencias.push("[x] V4 Monetização: Hub transferiu direto para a PONTE Ethereum → +20pts".into());
        }
        if sai_redondo {
            score += 5;
            evidencias.push("[x] V4 Saída em valor redondo (≥100 NCG) → +5pts".into());
        }
        if hub_txs <= 3 {
            score += 5;
            evidencias.push(format!("[x] V4 Hub quase não assina txs: {hub_txs} no banco → +5pts"));
        }

        // V5: Nonce sequencial (≥50% das contas com histórico suficiente)
        if elegíveis >= 3 && nonces_perfeitos * 2 >= elegíveis {
            score += 15;
            evidencias.push(format!(
                "[x] V5 Nonce sequencial perfeito: {nonces_perfeitos}/{elegíveis} contas (com ≥10 txs) sem buracos de nonce → +15pts"
            ));
        }

        // V6: Timing impossível para humanos
        if let Some(span_s) = timing_span {
            if span_s < 3.0 && n >= 10 {
                score += 20;
                evidencias.push(format!(
                    "[x] V6 Sincronia extrema: {n} contas assinaram em {span_s:.2}s (impossível humanamente) → +20pts"
                ));
            } else if span_s < 10.0 && n >= 8 {
                score += 10;
                evidencias.push(format!(
                    "[x] V6 Alta sincronia: {n} contas em {span_s:.2}s → +10pts"
                ));
            }
        }

        // ── Descontadores ──────────────────────────────────────────────────
        if hub_txs >= 500 {
            let desc = 15u32;
            score = score.saturating_sub(desc);
            evidencias.push(format!(
                "[-] Hub com histórico longo: {hub_txs} txs assinadas (menos suspeito) → -{desc}pts"
            ));
        } else if hub_txs >= 100 {
            let desc = 7u32;
            score = score.saturating_sub(desc);
            evidencias.push(format!(
                "[-] Hub com histórico moderado: {hub_txs} txs → -{desc}pts"
            ));
        }
        if gameplay_confirmados * 2 >= n as i64 {
            let desc = 15u32;
            score = score.saturating_sub(desc);
            evidencias.push(format!(
                "[-] Gameplay real confirmado: {gameplay_confirmados}/{n} satélites com hack_and_slash/crafting → -{desc}pts"
            ));
        } else if gameplay_confirmados > 0 {
            let desc = 5u32;
            score = score.saturating_sub(desc);
            evidencias.push(format!(
                "[-] Gameplay parcial: {gameplay_confirmados}/{n} satélites com gameplay real → -{desc}pts"
            ));
        }

        score = score.min(100);

        // Filtrar pelo score mínimo
        if score < cfg.score_min {
            conn.execute(
                "INSERT INTO seen_hubs(hub, last_origens, last_total, last_score) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(hub) DO UPDATE SET last_origens=excluded.last_origens,
                                               last_total=excluded.last_total,
                                               last_score=excluded.last_score",
                params![hub, n, total, score],
            )?;
            continue;
        }

        // Salva histórico de score
        let _ = conn.execute(
            "INSERT OR IGNORE INTO hub_score_history(hub, block_idx, score, origens, total_ncg)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![hub, start, score, n, total],
        );

        conn.execute(
            "INSERT INTO seen_hubs(hub, last_origens, last_total, last_score) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(hub) DO UPDATE SET last_origens=excluded.last_origens,
                                           last_total=excluded.last_total,
                                           last_score=excluded.last_score",
            params![hub, n, total, score],
        )?;

        // ── Dossiê CSV enriquecido ────────────────────────────────────────
        let path = format!("dossie_{}.csv", &hub[2..10]);
        let mut out = String::from(
            "papel,block_idx,timestamp_utc,tx_id,origem,destino,ncg,memo,\
             total_txs_conta,tipos_acao_distintos,itens_vendidos,perfil_bot\n",
        );
        for s in &srcs {
            let perfil = if robo_certo.contains(&s.2) {
                "bot_provavel"
            } else if robo_fraco.contains(&s.2) {
                "passivo"
            } else {
                "gameplay"
            };
            out.push_str(&format!(
                "satelite,{},{},{},{},{},{:.2},,{},{},{},{}\n",
                s.0,
                csv_field(&s.7),
                csv_field(&s.1),
                csv_field(&s.2),
                csv_field(&hub),
                s.3,
                s.4,
                s.5,
                s.6,
                perfil,
            ));
        }
        for e in &exits {
            let tipo = if e.2 == BRIDGE { "ponte_eth" } else { "carteira" };
            out.push_str(&format!(
                "saida_hub,{},,,{},{},{:.2},{},,,, {}\n",
                e.0,
                csv_field(&hub),
                csv_field(&e.2),
                e.3,
                csv_field(&e.4),
                tipo,
            ));
        }
        fs::write(&path, &out)?;

        alertas_emitidos += 1;

        // ── Output JSON (--json) ──────────────────────────────────────────
        if cfg.json {
            let ev_json: Vec<String> = evidencias
                .iter()
                .map(|e| format!("\"{}\"", e.replace('"', "\\\"")))
                .collect();
            println!(
                "{{\"hub\":\"{hub}\",\"score\":{score},\"nivel\":\"{}\",\
                 \"ativo\":{is_active},\"ultimo_bloco\":{last_act},\"blocos_sem_mov\":{blocks_since},\
                 \"origens\":{n},\"total_ncg\":{total:.2},\"span_blocos\":{span},\
                 \"ponte\":{sai_ponte},\"status\":\"{status_tag}\",\
                 \"evidencias\":[{}]}}",
                nivel_risco(score),
                ev_json.join(",")
            );
        }

        // ── Output terminal ───────────────────────────────────────────────
        println!(
            "\n🚨 {} [{}] ALERTA DE CLUSTER (Score: {score}/100 — {})",
            status_tag,
            activity_tag,
            nivel_risco(score)
        );
        println!("├── Conta-Mãe (Hub): {hub}");
        if is_active {
            println!("├── Atividade:       {activity_tag} (última mov. bloco {last_act}, há {blocks_since} blocos | tip {tip_block})");
        } else {
            println!("├── Atividade:       {activity_tag} (sem movimentação há {blocks_since} blocos | último bloco {last_act} | tip {tip_block})");
        }
        println!("├── Montante Drenado: {total:.2} NCG ({n} contas | blocos {start}..{end}, span {span} blocos)");
        for e in &exits {
            let tag = if e.2 == BRIDGE { " [PONTE]" } else { "" };
            println!("├── Saída do Hub:    {:.2} NCG → {}{} (bloco {})", e.3, e.2, tag, e.0);
        }
        println!("├── Triangulação:");
        for (idx, ev) in evidencias.iter().enumerate() {
            let branch = if idx + 1 == evidencias.len() { "└──" } else { "├──" };
            println!("│   {branch} {ev}");
        }
        println!("└── Dossiê:   {path}");
    }

    // ── 2. Anéis de mercado (com janela temporal ±500 blocos) ─────────────
    let loops: Vec<(String, String, i64, f64, Option<String>)> = {
        let mut st = conn.prepare(
            "SELECT m.src, m.dst, COUNT(*), ROUND(SUM(m.amount),2),
                    (SELECT f1.src FROM edges f1
                     JOIN edges f2 ON f2.kind='transfer' AND f2.src=f1.src AND f2.dst=m.dst
                     WHERE f1.kind='transfer' AND f1.dst=m.src
                       AND f1.block_idx BETWEEN m.block_idx - 500 AND m.block_idx + 500
                       AND f2.block_idx BETWEEN m.block_idx - 500 AND m.block_idx + 500
                     LIMIT 1)
             FROM edges m
             WHERE m.kind='market'
             GROUP BY m.src, m.dst
             HAVING COUNT(*) >= 4
             ORDER BY 3 DESC LIMIT 50",
        )?;
        let it = st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        it.collect::<Result<Vec<_>, _>>()?
    };

    let aneis: Vec<_> = loops.into_iter().filter(|l| l.4.is_some()).collect();
    if !aneis.is_empty() {
        println!("\n⚠️  ANÉIS DE MERCADO (comprador e vendedor financiados pela mesma conta, janela ±500 blocos):");
        for (comprador, vendedor, itens, vol, fin) in &aneis {
            alertas_emitidos += 1;
            println!(
                "├── {} financiou {comprador} (comprador) e {vendedor} (vendedor): {itens} itens, {vol:.2} NCG",
                fin.as_deref().unwrap_or("?")
            );
        }
    }

    if alertas_emitidos == 0 {
        println!("✅ Nenhum cluster suspeito detectado na janela atual (score mín: {}).", cfg.score_min);
    }

    Ok(())
}
