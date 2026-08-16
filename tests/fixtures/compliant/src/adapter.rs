use crate::core::Order;
use crate::ports::OrderStore;

pub struct InMemoryStore;

impl OrderStore for InMemoryStore {
    fn save(&self, _order: &Order) {}
}
