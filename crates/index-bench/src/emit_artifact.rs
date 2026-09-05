//! `emit-artifact` — build a real, shippable index file.
//!
//! This produces exactly what an application would deploy: one binary index plus a label sidecar,
//! built from **profstopick's real Ateneo registrar snapshot**. `js/demo.mjs` then loads the binary
//! through the WASM module and answers queries, which is the end-to-end proof that the engine works
//! outside Rust.
//!
//! The comparison that matters is against what that application ships **today**: a 2,505,813-byte
//! JSON search index occupying **95.6 % of the browser's 5 MB localStorage quota**.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use serde_json::Value;
use std::path::PathBuf;

fn main() {
    let dir = std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    });
    let src = dir.join("profstopick").join("research-pack").join("ateneo-professor-course.json");
    let Ok(text) = std::fs::read_to_string(&src) else {
        eprintln!("corpus not found: {}", src.display());
        eprintln!("set INDEX_CORPUS_DIR to the directory holding the profstopick checkout");
        std::process::exit(2);
    };
    let v: Value = serde_json::from_str(&text).expect("parse corpus");
    let prof = v.get("professor").and_then(|x| x.as_array()).expect("professor array");

    let schema = Schema::new(vec![
        Field::new("name", 3.0, 0.4),
        Field::new("course_code", 1.5, 0.4),
        Field::new("course_title", 1.0, 0.6),
    ]);
    let mut b = IndexBuilder::new(schema);
    let mut label: Vec<String> = Vec::with_capacity(prof.len());

    for p in prof {
        let name = p.get("instructor_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let mut code = String::new();
        let mut title = String::new();
        if let Some(c) = p.get("course").and_then(|x| x.as_array()) {
            for course in c {
                if let Some(s) = course.get("course_code").and_then(|x| x.as_str()) {
                    code.push_str(s);
                    code.push(' ');
                }
                if let Some(s) = course.get("title").and_then(|x| x.as_str()) {
                    title.push_str(s);
                    title.push(' ');
                }
            }
        }
        b.add(&Doc::new([name.clone(), code, title]));
        label.push(name);
    }

    let t0 = std::time::Instant::now();
    let ix = b.build().expect("build");
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let bytes = ix.to_bytes();

    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("artifact");
    std::fs::create_dir_all(&out).expect("mkdir artifact");
    std::fs::write(out.join("profstopick.idx"), &bytes).expect("write index");
    std::fs::write(
        out.join("profstopick.label.json"),
        serde_json::to_string(&label).expect("serialize label"),
    )
    .expect("write label");

    // What the application ships today. **This is NOT like-for-like** and the difference is
    // printed rather than buried: their shard indexes 11,949 entries (4,038 professors +
    // 7,911 courses); the corpus available here is the 1,322-professor registrar snapshot.
    const JSON_SHARD_BYTE: usize = 2_505_813;
    const JSON_SHARD_ENTRY: usize = 11_949;

    println!("emit-artifact :: profstopick, real Ateneo registrar snapshot");
    println!("  documents        {}", ix.doc_count());
    println!("  terms            {}", ix.term_count());
    println!("  build            {build_ms:.0} ms");
    println!("  index file       {} bytes", bytes.len());
    println!("    of which dict  {} bytes", ix.dict_byte_len());
    println!("  label sidecar    {} bytes", std::fs::metadata(out.join("profstopick.label.json")).map(|m| m.len()).unwrap_or(0));
    println!();
    println!("  the app ships today: {JSON_SHARD_BYTE} bytes of JSON over {JSON_SHARD_ENTRY} entries");
    println!("    = 95.6% of the 5 MB localStorage quota, with NO typo tolerance");
    println!("  this index:          {} bytes over {} entries, WITH typo tolerance", bytes.len(), ix.doc_count());
    println!();
    println!("  NOT a like-for-like total: their shard covers 11,949 entries (professors AND");
    println!("  courses); this covers the 1,322-professor snapshot, which is the corpus available");
    println!("  on disk. Per entry the honest comparison is:");
    println!(
        "    their JSON {:.0} B/entry   vs   this index {:.0} B/entry ({:.2}x)",
        JSON_SHARD_BYTE as f64 / JSON_SHARD_ENTRY as f64,
        bytes.len() as f64 / ix.doc_count() as f64,
        (bytes.len() as f64 / ix.doc_count() as f64)
            / (JSON_SHARD_BYTE as f64 / JSON_SHARD_ENTRY as f64)
    );
    println!("  These documents are far richer (every course code and title per professor), so");
    println!("  even a higher per-entry figure is not a regression - and the dictionary alone,");
    println!("  which is what buys typo tolerance, is {} bytes.", ix.dict_byte_len());
    println!("\nwrote {}", out.join("profstopick.idx").display());
}
