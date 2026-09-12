use index_cli::sql::SqlDumpReader;
fn main() {
    let sql = "INSERT INTO public.vendor (id, name) VALUES (1, 'North'), (2, 'South');";
    let r = SqlDumpReader::new(std::io::Cursor::new(sql.to_string()), &[]);
    for rec in r {
        match rec {
            Ok(row) => println!("ROW {row:?}"),
            Err(e) => { println!("ERR {e}"); break; }
        }
    }
}
