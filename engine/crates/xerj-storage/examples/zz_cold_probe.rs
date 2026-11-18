use std::time::Instant;
use xerj_storage::segment::{SectionType, SegmentReader};
use xerj_storage::stored_codec::*;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let r = SegmentReader::open(&path).unwrap();
    let raw = r.section(SectionType::Stored).unwrap().unwrap();
    println!("stored section {} bytes", raw.len());
    let t = Instant::now();
    let full = decode_stored(raw).unwrap();
    println!("decode_stored {:?} -> {} bytes", t.elapsed(), full.len());
    let n: usize = std::env::args()
        .nth(2)
        .map(|s| s.parse().unwrap())
        .unwrap_or(30000);
    let all: Vec<usize> = (0..n).collect();
    let t = Instant::now();
    let res = decode_stored_v2_rows_projected(raw, &all, &[]);
    match res {
        Ok(StoredV2RowHydrationResult::Hydrated { rows, .. }) => {
            println!("identity rows {} {:?}", rows.len(), t.elapsed())
        }
        Ok(_) => println!("other"),
        Err(e) => println!("err {e}"),
    }
    let five: Vec<usize> = vec![1, 100, 2000, 7000, n - 1];
    let t = Instant::now();
    let res = decode_stored_v2_rows(raw, &five);
    match res {
        Ok(StoredV2RowHydrationResult::Hydrated { rows, .. }) => {
            println!("5 full rows {} {:?}", rows.len(), t.elapsed())
        }
        Ok(_) => println!("other"),
        Err(e) => println!("err {e}"),
    }
    let t = Instant::now();
    let res = decode_stored_v2_rows_projected_parallel(raw, &five, None);
    match res {
        Ok(StoredV2RowHydrationResult::Hydrated { rows, .. }) => {
            println!("5 full rows PAR {} {:?}", rows.len(), t.elapsed())
        }
        Ok(_) => println!("other"),
        Err(e) => println!("err {e}"),
    }
    if let Ok(Some(names)) = stored_v2_source_column_names(raw) {
        for nm in &names {
            let t = Instant::now();
            let _ = decode_stored_v2_rows_projected(raw, &five, &[nm.as_str()]);
            let el = t.elapsed();
            if el.as_millis() > 5 {
                println!("col {nm} {:?}", el);
            }
        }
        println!("ncols {}", names.len());
    }
}
