Markdown
# nc-botwatch

Real-time on-chain forensics, state-bloat analysis, and multi-account bot detection tool for **Nine Chronicles**, written in Rust.

`nc-botwatch` tails a Nine Chronicles headless GraphQL endpoint, decodes raw Bencodex transaction payloads from scratch, reconstructs asset transfer and marketplace trade graphs in SQLite, and triangulates coordinated bot farms and wash-trading rings into scored reports and CSV dossiers.

---

## Architecture & Pipeline

1. **Incremental Chain Ingestion (`src/main.rs` → SQLite WAL)**
   Continuously indexes blocks, signers, nonces, timestamps, and raw hex Bencodex action payloads via GraphQL (`--follow`), maintaining high read/write concurrency without impacting node performance.
2. **Zero-Dependency Bencodex Decoder (`src/bin/edges.rs`)**
   Parses raw Bencodex payloads directly (bypassing truncated `inspection` strings) to reconstruct full economic edges:
   - `transfer_asset5` — sender, recipient, exact NCG amount, and memo.
   - `buy_product3` — buyer, seller, item batch count, and unit price.
3. **Multi-Vertex Triangulation & Risk Scoring (`src/bin/report.rs`)**
   Cross-correlates four behavioral and financial vectors to assign a `0–100` risk score and export evidence dossiers (`dossie_<hub>.csv`):
   - **Synchronized Sweeps:** Multiple satellite accounts draining NCG into a single consolidation hub within a narrow block window.
   - **Non-Human Action Profiles:** Satellite wallets exhibiting near-zero gameplay diversity (`<= 2` distinct action types across their entire history).
   - **Market Laundering / Wash-Trading Funnels:** Structured item sales at fixed unit prices followed by exact post-fee (`0.92x`) net NCG sweeps, or circular ring-trading funded by fixed NCG tranches.
   - **Bridge Exit Tracking:** Immediate round-number consolidation transfers from low-activity hub wallets straight to the NCG→WNCG Ethereum bridge.

---

## Quick Start

### Prerequisites
- Rust toolchain (`cargo` / `rustc`)
- A synced Nine Chronicles headless node with GraphQL enabled (e.g., `http://localhost:23061/graphql`)

### 1. Build
```bash
cargo build --release
2. Phase 1 — Ingest Blocks (Backfill + Live Tail)
Bash
# Backfill the last 1,000 blocks into botwatch.db
cargo run --release -- --url http://localhost:23061/graphql --db botwatch.db

# Follow the chain tip in real time
cargo run --release -- --url http://localhost:23061/graphql --follow
Without --from and without an existing database, ingestion starts at tip - 1000 and subsequently resumes from last_block.

Re-running is idempotent (INSERT OR IGNORE).

3. Phase 2 — Extract Economic Edges & Generate Reports
Bash
# Decode raw Bencodex payloads into the `edges` table (transfers + market trades)
./target/release/edges edges

# Run triangulation (default: >= 8 origins within 50 blocks) and export CSV dossiers
./target/release/report

# Custom thresholds and ignore list
./target/release/report --min-origens 5 --janela 100 --ignore-file ignorar.txt
ignorar.txt: one lowercase wallet address (0x...) per line. Known official bridge contracts are excluded by default.

Continuous Monitoring (watch.sh)
Repeats edge extraction and triangulation every 10 minutes while the collector runs with --follow in the background (appending alerts to report.log and sending desktop notifications via notify-send when available):

Bash
./watch.sh
Disclaimer
This tool flags statistical and behavioral coordination signals on public blockchain data. Automated alerts represent on-chain heuristics and should be reviewed alongside full transaction dossiers before taking moderation action.

License
MIT
