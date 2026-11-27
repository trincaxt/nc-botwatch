// Decodifica o `raw` (hex de Bencodex) das ações e extrai arestas.
//   cargo run --release --bin edges -- dump <type_id> [n]   # mostra a estrutura decodificada
//   cargo run --release --bin edges -- edges                # preenche a tabela `edges` e mostra um resumo
use anyhow::Result;
use rusqlite::{params, types::ValueRef, Connection};

#[derive(Debug)]
enum V {
    Null,
    Bool(bool),
    Int(i128),
    Bytes(Vec<u8>),
    Text(String),
    List(Vec<V>),
    Dict(Vec<(V, V)>),
}

fn len_bytes(b: &[u8], i: &mut usize) -> Option<Vec<u8>> {
    let colon = b[*i..].iter().position(|&c| c == b':')? + *i;
    let len: usize = std::str::from_utf8(&b[*i..colon]).ok()?.parse().ok()?;
    let (start, end) = (colon + 1, colon + 1 + len);
    let s = b.get(start..end)?.to_vec();
    *i = end;
    Some(s)
}

fn parse(b: &[u8], i: &mut usize) -> Option<V> {
    match *b.get(*i)? {
        b'n' => { *i += 1; Some(V::Null) }
        b't' => { *i += 1; Some(V::Bool(true)) }
        b'f' => { *i += 1; Some(V::Bool(false)) }
        b'i' => {
            let end = b[*i..].iter().position(|&c| c == b'e')? + *i;
            let n = std::str::from_utf8(&b[*i + 1..end]).ok()?.parse().ok()?;
            *i = end + 1;
            Some(V::Int(n))
        }
        b'l' => {
            *i += 1;
            let mut v = vec![];
            while *b.get(*i)? != b'e' { v.push(parse(b, i)?); }
            *i += 1;
            Some(V::List(v))
        }
        b'd' => {
            *i += 1;
            let mut v = vec![];
            while *b.get(*i)? != b'e' {
                let k = parse(b, i)?;
                let val = parse(b, i)?;
                v.push((k, val));
            }
            *i += 1;
            Some(V::Dict(v))
        }
        b'u' => { *i += 1; Some(V::Text(String::from_utf8(len_bytes(b, i)?).ok()?)) }
        b'0'..=b'9' => Some(V::Bytes(len_bytes(b, i)?)),
        _ => None,
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() { return None; }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

fn get<'a>(v: &'a V, key: &str) -> Option<&'a V> {
    if let V::Dict(d) = v {
        for (k, val) in d {
            if let V::Text(t) = k { if t == key { return Some(val); } }
        }
    }
    None
}

fn money(v: &V) -> (Option<String>, Option<f64>) {
    if let V::List(a) = v {
        if let (Some(cur), Some(V::Int(q))) = (a.first(), a.get(1)) {
            let dec = match get(cur, "decimalPlaces") {
                Some(V::Bytes(d)) => d.first().copied().unwrap_or(0) as i32,
                _ => 0,
            };
            let t = match get(cur, "ticker") { Some(V::Text(t)) => Some(t.clone()), _ => None };
            return (t, Some(*q as f64 / 10f64.powi(dec)));
        }
    }
    (None, None)
}

fn show(v: &V, ind: usize) {
    let pad = " ".repeat(ind);
    match v {
        V::Null => println!("{pad}null"),
        V::Bool(b) => println!("{pad}{b}"),
        V::Int(n) => println!("{pad}{n}"),
        V::Bytes(b) => println!("{pad}0x{}", hex(b)),
        V::Text(t) => println!("{pad}\"{t}\""),
        V::List(l) => {
            println!("{pad}[");
            for x in l { show(x, ind + 2); }
            println!("{pad}]");
        }
        V::Dict(d) => {
            println!("{pad}{{");
            for (k, x) in d {
                match k { V::Text(t) => println!("{pad}  {t}:"), o => println!("{pad}  {o:?}:") }
                show(x, ind + 4);
            }
            println!("{pad}}}");
        }
    }
}

fn decode(raw: &str) -> Option<V> {
    let b = unhex(raw)?;
    let mut i = 0;
    parse(&b, &mut i)
}

fn print_q(conn: &Connection, sql: &str) -> Result<()> {
    let mut st = conn.prepare(sql)?;
    let nc = st.column_count();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let cells: Vec<String> = (0..nc)
        .map(|k| match r.get_ref(k) {
            Ok(ValueRef::Null) => "-".into(),
             Ok(ValueRef::Integer(i)) => i.to_string(),
             Ok(ValueRef::Real(f)) => format!("{f:.2}"),
             Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
             _ => "?".into(),
        })
        .collect();
        println!("{}", cells.join("  "));
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let db = std::env::var("DB").unwrap_or_else(|_| "botwatch.db".into());
    let mut conn = Connection::open(&db)?;

    match args.first().map(|s| s.as_str()) {
        Some("dump") => {
            let ty = args.get(1).expect("uso: dump <type_id> [n]").clone();
            let n: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
            let mut st = conn.prepare("SELECT raw FROM actions WHERE type_id=?1 LIMIT ?2")?;
            let raws = st.query_map(params![ty, n], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            for raw in raws {
                match decode(&raw) {
                    Some(v) => show(&v, 0),
                    None => println!("falha ao decodificar"),
                }
                println!("----");
            }
        }
        Some("edges") => {
            conn.execute_batch(
                "DROP TABLE IF EXISTS edges;
                 CREATE TABLE edges(
                     tx_id TEXT, pos INTEGER, kind TEXT, block_idx INTEGER,
                     src TEXT, dst TEXT, ticker TEXT, amount REAL, memo TEXT,
                     PRIMARY KEY (tx_id, kind, pos));
                 CREATE TABLE IF NOT EXISTS gameplay_flags(
                     tx_id       TEXT    NOT NULL,
                     block_idx   INTEGER NOT NULL,
                     signer      TEXT    NOT NULL,
                     action_type TEXT    NOT NULL,
                     PRIMARY KEY (tx_id, action_type));
                 CREATE INDEX IF NOT EXISTS idx_gf_signer ON gameplay_flags(signer);",
            )?;

            // Tipos que provam gameplay real (descontam score de bots)
            const GAMEPLAY_TYPES: &[&str] = &[
                "hack_and_slash",   "hack_and_slash2",  "hack_and_slash8",
                "hack_and_slash9",  "hack_and_slash10", "hack_and_slash11",
                "hack_and_slash12", "hack_and_slash13", "hack_and_slash14",
                "hack_and_slash15", "hack_and_slash16", "hack_and_slash17",
                "hack_and_slash18", "hack_and_slash19", "hack_and_slash20",
                "hack_and_slash21", "combination_equipment",
                "combination_equipment2", "combination_equipment3",
                "combination_consumable", "combination_consumable2",
                "rapid_combination", "rapid_combination2", "rapid_combination3",
                "item_enhancement",  "item_enhancement2", "item_enhancement3",
                "battle_arena", "join_arena", "raid", "raid2",
                "grinding", "claim_raid_reward",
            ];

            // Registra gameplay_flags (sem raw necessário — usa type_id da coluna já extraída)
            {
                let gf_rows: Vec<(String, i64, String, String)> = {
                    let placeholders: Vec<String> = GAMEPLAY_TYPES
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 1))
                        .collect();
                    let sql = format!(
                        "SELECT t.id, t.block_idx, lower(t.signer), a.type_id
                         FROM actions a JOIN txs t ON t.id=a.tx_id
                         WHERE a.type_id IN ({})",
                        placeholders.join(",")
                    );
                    let mut st = conn.prepare(&sql)?;
                    let it = st.query_map(
                        rusqlite::params_from_iter(GAMEPLAY_TYPES.iter()),
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                    it.collect::<Result<Vec<_>, _>>()?
                };
                let gftx = conn.transaction()?;
                let mut gf_n = 0usize;
                for (id, blk, sig, ty) in &gf_rows {
                    gftx.execute(
                        "INSERT OR IGNORE INTO gameplay_flags(tx_id, block_idx, signer, action_type)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, blk, sig, ty],
                    )?;
                    gf_n += 1;
                }
                gftx.commit()?;
                eprintln!("{gf_n} gameplay_flags registradas");
            }

            let rows: Vec<(String, i64, String, String)> = {
                let mut st = conn.prepare(
                    "SELECT t.id, t.block_idx, lower(t.signer), a.raw
                     FROM actions a JOIN txs t ON t.id = a.tx_id
                     WHERE a.type_id IN ('transfer_asset5','buy_product3')
                       AND a.raw IS NOT NULL",
                )?;
                let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                it.collect::<Result<Vec<_>, _>>()?
            };
            let dbtx = conn.transaction()?;
            let mut n = 0;
            for (id, blk, src, raw) in rows {
                let Some(root) = decode(&raw) else { continue };
                let Some(vals) = get(&root, "values") else { continue };
                match get(&root, "type_id") {
                    Some(V::Text(t)) if t == "transfer_asset5" => {
                        let Some(V::Bytes(r)) = get(vals, "recipient") else { continue };
                        let memo = match get(vals, "memo") { Some(V::Text(m)) => Some(m.clone()), _ => None };
                        let (ticker, amount) = get(vals, "amount").map(money).unwrap_or((None, None));
                        dbtx.execute(
                            "INSERT OR REPLACE INTO edges VALUES (?1,0,'transfer',?2,?3,?4,?5,?6,?7)",
                            params![id, blk, src, format!("0x{}", hex(r)), ticker, amount, memo],
                        )?;
                        n += 1;
                    }
                    Some(V::Text(t)) if t == "buy_product3" => {
                        let Some(V::List(ps)) = get(vals, "p") else { continue };
                        for (pos, p) in ps.iter().enumerate() {
                            let V::List(f) = p else { continue };
                            let (Some(price), Some(V::Bytes(seller))) = (f.get(1), f.get(2)) else { continue };
                            let (ticker, amount) = money(price);
                            dbtx.execute(
                                "INSERT OR REPLACE INTO edges VALUES (?1,?2,'market',?3,?4,?5,?6,?7,NULL)",
                                params![id, pos as i64, blk, src, format!("0x{}", hex(seller)), ticker, amount],
                            )?;
                            n += 1;
                        }
                    }
                    _ => {}
                }
            }
            dbtx.commit()?;
            println!("{n} arestas\n\n== transfer: destinos (dst, txs, origens distintas, total) ==");
            print_q(&conn, "SELECT dst, COUNT(*), COUNT(DISTINCT src), ROUND(SUM(amount),2) FROM edges WHERE kind='transfer' GROUP BY dst ORDER BY 3 DESC, 2 DESC LIMIT 15")?;
            println!("\n== transfer: memos (memo, txs, origens distintas) ==");
            print_q(&conn, "SELECT memo, COUNT(*), COUNT(DISTINCT src) FROM edges WHERE memo IS NOT NULL GROUP BY memo ORDER BY 3 DESC, 2 DESC LIMIT 15")?;
            println!("\n== market: vendedores (dst, itens, compradores distintos, total) ==");
            print_q(&conn, "SELECT dst, COUNT(*), COUNT(DISTINCT src), ROUND(SUM(amount),2) FROM edges WHERE kind='market' GROUP BY dst ORDER BY 3 DESC, 2 DESC LIMIT 15")?;
            println!("\n== market: pares comprador->vendedor repetidos (src, dst, itens, total) ==");
            print_q(&conn, "SELECT src, dst, COUNT(*), ROUND(SUM(amount),2) FROM edges WHERE kind='market' GROUP BY src, dst HAVING COUNT(*) > 1 ORDER BY 3 DESC LIMIT 15")?;
            println!("\n== gameplay: contas com ações reais (top 15) ==");
            print_q(&conn, "SELECT signer, COUNT(DISTINCT action_type), COUNT(*) FROM gameplay_flags GROUP BY signer ORDER BY 2 DESC, 3 DESC LIMIT 15")?;
        }
        _ => eprintln!("uso: edges dump <type_id> [n] | edges edges"),
    }
    Ok(())
}
