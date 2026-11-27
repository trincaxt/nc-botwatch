// Radar de Triangulação Multi-Account & Bots (nc-botwatch) — v3
// Vértices: (1) Convergência financeira, (2) Perfil robótico refinado,
//           (3) Funil de mercado, (4) Saída/Ponte,
//           (5) Nonce sequencial, (6) Timing real da rajada (por tx_id).
// Anti-FP: score proporcional, whitelist de reward values em centavos (i64),
//          descontadores por gameplay real, janela temporal nos anéis de mercado.
// Meta-Cluster: grafo persistente cruzando hubs por Jaccard >= 20%,
//               transferência direta hub→hub ou saída externa compartilhada.
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

// Valores de NCG em centavos (i64 exato: NCG * 100) que são rewards normais do jogo
const REWARD_VALUES_CENTS: &[i64] = &[
    100, 200, 300, 400, 500, 600, 700, 800, 1000, 1150, 1200, 1380, 1500,
    2000, 2300, 2500, 4600, 5000, 10000, 11500, 23000,
];

fn to_cents(ncg: f64) -> i64 {
    (ncg * 100.0).round() as i64
}

fn fmt_cents(c: i64) -> String {
    let sign = if c < 0 { "-" } else { "" };
    let abs = c.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

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

/// Verifica se um valor em centavos é um reward natural do jogo (não script)
fn is_reward_cents(cents: i64) -> bool {
    REWARD_VALUES_CENTS.contains(&cents)
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

/// Span em segundos entre a primeira e a última tx de drenagem (filtrado por tx_id do cluster)
fn timing_span_seconds(conn: &Connection, tx_ids: &[String]) -> Option<f64> {
    if tx_ids.is_empty() {
        return None;
    }
    let placeholders: Vec<String> = (1..=tx_ids.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT MAX(CAST(strftime('%s', ts) AS REAL)) - MIN(CAST(strftime('%s', ts) AS REAL))
         FROM txs WHERE id IN ({})",
        placeholders.join(",")
    );
    let mut st = conn.prepare(&sql).ok()?;
    let result: Option<f64> = {
        let mut rows = st.query(rusqlite::params_from_iter(tx_ids.iter())).ok()?;
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

struct HubCluster {
    hub: String,
    score: u32,
    is_active: bool,
    start: i64,
    end: i64,
    total_cents: i64,
    satellites: HashSet<String>,
    exits_to: HashSet<String>,
    non_bridge_exits: HashSet<String>,
    bridge_cents: i64,
    exit_flows: Vec<(i64, String, i64)>,
}

fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        parent[rb] = ra;
    }
}

fn emit_meta_clusters(hubs: &[HubCluster], json_mode: bool) -> Result<()> {
    if hubs.len() < 2 {
        return Ok(());
    }
    let mut parent: Vec<usize> = (0..hubs.len()).collect();
    let mut link_reasons: Vec<(usize, usize, String)> = Vec::new();

    for i in 0..hubs.len() {
        for j in (i + 1)..hubs.len() {
            let a = &hubs[i];
            let b = &hubs[j];

            let inter = a.satellites.intersection(&b.satellites).count();
            let union_len = a.satellites.len() + b.satellites.len() - inter;
            let jaccard_ok = union_len > 0 && inter * 5 >= union_len; // >= 20%

            let direct_ab = a.exits_to.contains(&b.hub) || b.satellites.contains(&a.hub);
            let direct_ba = b.exits_to.contains(&a.hub) || a.satellites.contains(&b.hub);
            let shared_ext: Vec<&String> = a
                .non_bridge_exits
                .intersection(&b.non_bridge_exits)
                .collect();

            if jaccard_ok || direct_ab || direct_ba || !shared_ext.is_empty() {
                uf_union(&mut parent, i, j);
                let mut reasons = Vec::new();
                if jaccard_ok {
                    let pct = (inter as f64 / union_len as f64) * 100.0;
                    reasons.push(format!("Jaccard {pct:.0}% ({inter}/{union_len} satélites)"));
                }
                if direct_ab {
                    reasons.push(format!("{} → {}", &a.hub[..10], &b.hub[..10]));
                }
                if direct_ba {
                    reasons.push(format!("{} → {}", &b.hub[..10], &a.hub[..10]));
                }
                if !shared_ext.is_empty() {
                    reasons.push(format!("saída comum {}", &shared_ext[0][..10]));
                }
                link_reasons.push((i, j, reasons.join(" + ")));
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..hubs.len() {
        let r = uf_find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    for (_root, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let member_set: HashSet<&str> = members.iter().map(|&idx| hubs[idx].hub.as_str()).collect();
        let mut unique_sats: HashSet<&str> = HashSet::new();
        let mut total_bruto_cents: i64 = 0;
        let mut total_ponte_cents: i64 = 0;
        let mut min_blk = i64::MAX;
        let mut max_blk = i64::MIN;
        let mut any_active = false;
        let mut max_score = 0u32;

        for &idx in &members {
            let h = &hubs[idx];
            total_bruto_cents += h.total_cents;
            total_ponte_cents += h.bridge_cents;
            min_blk = min_blk.min(h.start);
            max_blk = max_blk.max(h.end);
            any_active |= h.is_active;
            max_score = max_score.max(h.score);
            for s in &h.satellites {
                if !member_set.contains(s.as_str()) {
                    unique_sats.insert(s.as_str());
                }
            }
        }

        // Fluxos diretos entre hubs do meta-cluster
        let mut hierarchy_flows: Vec<(i64, &str, &str, i64)> = Vec::new();
        let mut internal_cents: i64 = 0;
        for &idx in &members {
            let h = &hubs[idx];
            for (blk, dst, c) in &h.exit_flows {
                if member_set.contains(dst.as_str()) {
                    internal_cents += *c;
                    hierarchy_flows.push((*blk, h.hub.as_str(), dst.as_str(), *c));
                }
            }
        }
        hierarchy_flows.sort_by_key(|f| f.0);

        let first_hub = &hubs[members[0]].hub;
        let meta_path = format!("meta_cluster_{}.csv", &first_hub[2..10]);
        let mut csv = String::from("tipo,origem,destino,block_idx,ncg,score,ativo,satelites_hub\n");
        for &idx in &members {
            let h = &hubs[idx];
            csv.push_str(&format!(
                "hub,{},,{},{},{},{},{}\n",
                h.hub,
                h.start,
                fmt_cents(h.total_cents),
                h.score,
                h.is_active,
                h.satellites.len()
            ));
        }
        for (blk, src, dst, c) in &hierarchy_flows {
            csv.push_str(&format!(
                "fluxo_hub_para_hub,{src},{dst},{blk},{},,,\n",
                fmt_cents(*c)
            ));
        }
        for &idx in &members {
            let h = &hubs[idx];
            for (blk, dst, c) in &h.exit_flows {
                if !member_set.contains(dst.as_str()) {
                    let kind = if dst == BRIDGE { "saida_ponte" } else { "saida_externa" };
                    csv.push_str(&format!(
                        "{kind},{},{dst},{blk},{},,,\n",
                        h.hub,
                        fmt_cents(*c)
                    ));
                }
            }
        }
        fs::write(&meta_path, csv)?;

        let status_icon = if any_active { "🟢 ATIVO" } else { "⚫ OFF" };
        if json_mode {
            let hub_list: Vec<String> = members.iter().map(|&i| format!("\"{}\"", hubs[i].hub)).collect();
            println!(
                "{{\"meta_cluster\":true,\"hubs\":[{}],\"ativo\":{any_active},\"score_max\":{max_score},\
                 \"satelites_unicos\":{},\"total_bruto_ncg\":{},\"total_ponte_ncg\":{},\
                 \"fluxo_interno_ncg\":{},\"dossie\":\"{meta_path}\"}}",
                hub_list.join(","),
                unique_sats.len(),
                fmt_cents(total_bruto_cents),
                fmt_cents(total_ponte_cents),
                fmt_cents(internal_cents)
            );
        }

        println!(
            "\n🕸️  META-CLUSTER UNIFICADO [{status_icon}] ({} hubs conectados | Score máx: {max_score}/100)",
            members.len()
        );
        println!(
            "├── Escala Total:   {} NCG brutos ({} NCG direto p/ PONTE | {} NCG entre hubs)",
            fmt_cents(total_bruto_cents),
            fmt_cents(total_ponte_cents),
            fmt_cents(internal_cents)
        );
        println!(
            "├── Frota Única:    {} contas-satélite distintas | blocos {min_blk}..{max_blk}",
            unique_sats.len()
        );
        println!("├── Hubs na Árvore:");
        for &idx in &members {
            let h = &hubs[idx];
            let st = if h.is_active { "🟢 ATIVO" } else { "⚫ OFF" };
            println!(
                "│   ├── {} [{st}] ({}/100) — {} NCG, {} satélites (blocos {}..{})",
                h.hub,
                h.score,
                fmt_cents(h.total_cents),
                h.satellites.len(),
                h.start,
                h.end
            );
        }
        if !hierarchy_flows.is_empty() {
            println!("├── Hierarquia (Hub → Hub):");
            for (blk, src, dst, c) in &hierarchy_flows {
                println!("│   ├── {src} → {dst}: {} NCG (bloco {blk})", fmt_cents(*c));
            }
        }
        let member_idx_set: HashSet<usize> = members.iter().copied().collect();
        println!("├── Conexões Detectadas:");
        for (i, j, reason) in &link_reasons {
            if member_idx_set.contains(i) && member_idx_set.contains(j) {
                println!("│   ├── {reason}");
            }
        }
        println!("└── Dossiê Unificado: {meta_path}");
    }

    Ok(())
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
    let mut detected_hubs: Vec<HubCluster> = Vec::new();
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

            // Múltiplos exatos de 0,92 NCG (92 centavos pós-taxa 8%) — aritmética inteira i64
            let cents = to_cents(ncg);
            if cents > 0 && !is_reward_cents(cents) && cents % 92 == 0 {
                multiplos_092 += 1;
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

        // ── Vértice 6: Timing real da rajada (filtrado pelos tx_ids do cluster) ──
        let tx_ids: Vec<String> = srcs.iter().map(|s| s.1.clone()).collect();
        let sat_signers: Vec<String> = {
            let uniq: HashSet<&String> = srcs.iter().map(|s| &s.2).collect();
            uniq.into_iter().cloned().collect()
        };
        let timing_span = timing_span_seconds(&conn, &tx_ids);

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

        // V4: Saída / Monetização (aritmética inteira em centavos)
        let sai_ponte = exits.iter().any(|e| e.2 == BRIDGE);
        let sai_redondo = exits.iter().any(|e| {
            let c = to_cents(e.3);
            c >= 10_000 && c % 100 == 0
        });
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

        // V6: Timing real da rajada de drenagem (tx_id IN cluster)
        if let Some(span_s) = timing_span {
            if span_s <= 60.0 && n >= 10 {
                score += 20;
                evidencias.push(format!(
                    "[x] V6 Rajada sincronizada: {n} contas drenaram em {span_s:.1}s → +20pts"
                ));
            } else if span_s <= 240.0 && n >= 8 {
                score += 10;
                evidencias.push(format!(
                    "[x] V6 Drenagem coordenada: {n} contas drenaram em {span_s:.1}s → +10pts"
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

        // Registra hub qualificado para o grafo de Meta-Clusters
        let sat_set: HashSet<String> = srcs.iter().map(|s| s.2.clone()).collect();
        let total_cents: i64 = srcs.iter().map(|s| to_cents(s.3)).sum();
        let mut exits_to: HashSet<String> = HashSet::new();
        let mut non_bridge_exits: HashSet<String> = HashSet::new();
        let mut bridge_cents: i64 = 0;
        let mut exit_flows: Vec<(i64, String, i64)> = Vec::new();
        for e in &exits {
            let c = to_cents(e.3);
            exits_to.insert(e.2.clone());
            if e.2 == BRIDGE {
                bridge_cents += c;
            } else {
                non_bridge_exits.insert(e.2.clone());
            }
            exit_flows.push((e.0, e.2.clone(), c));
        }
        detected_hubs.push(HubCluster {
            hub,
            score,
            is_active,
            start,
            end,
            total_cents,
            satellites: sat_set,
            exits_to,
            non_bridge_exits,
            bridge_cents,
            exit_flows,
        });
    }

    // ── 2. Agrupador de Meta-Clusters (Grafo Cruzado entre Hubs) ─────────
    emit_meta_clusters(&detected_hubs, cfg.json)?;

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
