pub struct LiveOrder { pub id: u64 }

/// Query the live orders from the trader session.
pub fn query_live_orders(session: &Session) -> Vec<LiveOrder> {
    session.read_orders()
}
