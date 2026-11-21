pub struct Position {
    pub symbol: String,
    pub qty: i64,
    pub open_price: f64,
    pub avg_price: f64,
}

impl Position {
    pub fn pnl(&self, last: f64) -> f64 { (last - self.avg_price) * self.qty as f64 }
}
