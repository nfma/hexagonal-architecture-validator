use crate::core::Order;
use crate::ports::OrderStore;

pub fn place_order(store: &dyn OrderStore) {
    store.save(&Order);
}
