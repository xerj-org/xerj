pub enum Msg { Open, Price(f64), Position(u64) }
pub fn encode(m: &Msg) -> Vec<u8> { let _ = m; vec![] }
