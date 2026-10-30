/// In-memory storage used only by the synthetic map report fixture.
pub struct ParcelStore {
    capacity: usize,
    ids: Vec<String>,
}

impl ParcelStore {
    pub fn new(capacity: usize) -> Self {
        Self { capacity, ids: Vec::new() }
    }

    /// Return false when validation fails or the configured capacity is full.
    pub fn insert(&mut self, id: String) -> bool {
        if id.trim().is_empty() || self.ids.len() >= self.capacity {
            return false;
        }
        self.ids.push(id);
        true
    }
}
