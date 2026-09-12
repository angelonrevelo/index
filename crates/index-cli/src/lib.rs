//! `index_cli` — the pipe half of `index`, as a library.
//!
//! The binary is a thin `main.rs` over these modules. They are a library so that a second
//! consumer — today the benchmark crate, which parses real dumps to measure real repos — reads
//! rows through the SAME code the tool runs, not through a copy that drifts.
//!
//! - [`row`]: the three printed-row formats (CSV, TSV, JSONL) — "a client prints rows".
//! - [`sql`]: SQL dumps (`INSERT` statements, `COPY … FROM stdin`) — "a dump a database wrote".
//! - [`json`]: the hand-written JSON reader the JSONL format flattens through.
//! - [`collection`]: reading and writing multi-segment collections on disk.

pub mod collection;
pub mod json;
pub mod row;
pub mod sql;
