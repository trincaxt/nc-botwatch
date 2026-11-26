CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS blocks (
    idx      INTEGER PRIMARY KEY,
    hash     TEXT NOT NULL,
    ts       TEXT NOT NULL,          -- timestamp do bloco, como veio do GraphQL
    tx_count INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS txs (
    id           TEXT PRIMARY KEY,
    block_idx    INTEGER NOT NULL REFERENCES blocks(idx),
    signer       TEXT NOT NULL,
    nonce        INTEGER,
    ts           TEXT NOT NULL,      -- timestamp da tx (criada pelo cliente)
    action_count INTEGER NOT NULL
);

-- raw = Bencodex em hex (para decodificar transfers depois);
-- inspection = texto que o nó devolve, guardado como veio
CREATE TABLE IF NOT EXISTS actions (
    tx_id      TEXT NOT NULL REFERENCES txs(id),
    pos        INTEGER NOT NULL,
    inspection TEXT,
    raw        TEXT,
    type_id    TEXT,
    PRIMARY KEY (tx_id, pos)
);

CREATE INDEX IF NOT EXISTS idx_txs_signer_ts ON txs(signer, ts);
CREATE INDEX IF NOT EXISTS idx_txs_block     ON txs(block_idx);
