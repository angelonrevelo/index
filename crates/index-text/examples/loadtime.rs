//! Measures the block-FOR decode share of `Index::from_bytes` on a real index file.
fn main() {
    let path = std::env::args().nth(1).expect("usage: loadtime <file.idx>");
    let bytes = std::fs::read(&path).unwrap();
    // warm the page cache
    let _ = index_text::Index::from_bytes(&bytes).unwrap();
    let mut t = Vec::new();
    for _ in 0..5 {
        let a = std::time::Instant::now();
        let ix = index_text::Index::from_bytes(&bytes).unwrap();
        t.push(a.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(&ix);
    }
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("{path}: {} B, from_bytes median {:.1} ms (of 5)", bytes.len(), t[2]);
}
