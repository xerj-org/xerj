/// Open-jump strategy: compares the open price with the previous close price.
pub fn open_jump_strategy(open: f64, price: f64, open_price: f64) -> bool {
    let jump = open_price - price;
    let open_gap = open - price;
    jump > open_gap && open > price && open_price > price
}

pub fn price_open(open: f64, price: f64) -> f64 { open * price }
