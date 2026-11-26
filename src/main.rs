use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

const BATCH: i64 = 100;
const DEFAULT_BACKFILL: i64 = 1000;

#[derive(Deserialize)]
struct Block {
    index: i64,
    hash: String,
    timestamp: String,
    transactions: Vec<Tx>,
}

#[derive(Deserialize)]
struct Tx {
    id: String,
    signer: String,
    nonce: Value, // pode vir como número ou string, dependendo do scalar Long
    timestamp: String,
    actions: Vec<Action>,
}

#[derive(Deserialize)]
struct Action {
    raw: Option<String>,
    inspection: Option<String>,
}

struct Args {
    url: String,
    db: String,
    from: Option<i64>,
    follow: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        url: "http://localhost:23061/graphql".into(),
        db: "botwatch.db".into(),
        from: None,
        follow: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--url" => a.url = it.next().expect("--url precisa de valor"),
            "--db" => a.db = it.next().expect("--db precisa de valor"),
            "--from" => a.from = it.next().and_then(|v| v.parse().ok()),
            "--follow" => a.follow = true,
            other => eprintln!("argumento ignorado: {other}"),
        }
    }
    a
}

async fn gql(client: &reqwest::Client, url: &str, query: &str, vars: Value) -> Result<Value> {
    let resp: Value = client
    .post(url)
    .json(&json!({ "query": query, "variables": vars }))
    .send()
    .await?
    .error_for_status()?
    .json()
    .await?;
    if let Some(errs) = resp.get("errors") {
        bail!("GraphQL errors: {errs}");
    }
    resp.get("data").cloned().context("resposta sem campo data")
}

async fn tip_index(client: &reqwest::Client, url: &str) -> Result<i64> {
    let q = "query{chainQuery{blockQuery{blocks(desc:true,limit:1){index}}}}";
    let d = gql(client, url, q, json!({})).await?;
    d["chainQuery"]["blockQuery"]["blocks"][0]["index"]
    .as_i64()
    .context("não consegui ler o índice do tip")
}

async fn fetch_blocks(client: &reqwest::Client, url: &str, offset: i64, limit: i64) -> Result<Vec<Block>> {
    let q = r#"query($offset:Int!,$limit:Int!){
    chainQuery{blockQuery{blocks(offset:$offset,limit:$limit,desc:false){
    index hash timestamp
    transactions{ id signer nonce timestamp actions{ raw inspection } }
}}}}"#;
let d = gql(client, url, q, json!({ "offset": offset, "limit": limit })).await?;
let v = d["chainQuery"]["blockQuery"]["blocks"].clone();
Ok(serde_json::from_value(v)?)
}

fn nonce_to_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn type_id_of(insp: &Option<String>) -> Option<String> {
    let s = insp.as_deref()?;
    let k = "\"type_id\": \"";
    let start = s.find(k)? + k.len();
    let end = s[start..].find('"')? + start;
    Some(s[start..end].to_string())
}

fn store(conn: &mut Connection, blocks: &[Block]) -> Result<()> {
    let tx = conn.transaction()?;
    for b in blocks {
        tx.execute(
            "INSERT OR IGNORE INTO blocks(idx,hash,ts,tx_count) VALUES (?1,?2,?3,?4)",
                   params![b.index, b.hash, b.timestamp, b.transactions.len() as i64],
        )?;
        for t in &b.transactions {
            tx.execute(
                "INSERT OR IGNORE INTO txs(id,block_idx,signer,nonce,ts,action_count) VALUES (?1,?2,?3,?4,?5,?6)",
                       params![t.id, b.index, t.signer, nonce_to_i64(&t.nonce), t.timestamp, t.actions.len() as i64],
            )?;
            for (pos, a) in t.actions.iter().enumerate() {
                tx.execute(
                    "INSERT OR IGNORE INTO actions(tx_id,pos,inspection,raw,type_id) VALUES (?1,?2,?3,?4,?5)",
                           params![t.id, pos as i64, a.inspection, a.raw, type_id_of(&a.inspection)],
                )?;
            }
        }
    }
    if let Some(last) = blocks.last() {
        tx.execute(
            "INSERT INTO meta(key,value) VALUES ('last_block',?1)
        ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                   params![last.index.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn last_block(conn: &Connection) -> Option<i64> {
    conn.query_row("SELECT value FROM meta WHERE key='last_block'", [], |r| r.get::<_, String>(0))
    .ok()
    .and_then(|s| s.parse().ok())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    let mut conn = Connection::open(&args.db)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(include_str!("../schema.sql"))?;

    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;

    // ponto de partida: --from > retomada > tip - 1000
    let mut next = match (args.from, last_block(&conn)) {
        (Some(f), _) => f,
        (None, Some(l)) => l + 1,
        (None, None) => (tip_index(&client, &args.url).await? - DEFAULT_BACKFILL).max(0),
    };
    eprintln!("começando no bloco {next}");

    loop {
        let blocks = fetch_blocks(&client, &args.url, next, BATCH).await?;
        if blocks.is_empty() {
            if !args.follow {
                break;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        }
        let txs: usize = blocks.iter().map(|b| b.transactions.len()).sum();
        store(&mut conn, &blocks)?;
        next = blocks.last().unwrap().index + 1;
        eprintln!("ok até o bloco {} ({} blocos, {} txs)", next - 1, blocks.len(), txs);
    }
    Ok(())
}
