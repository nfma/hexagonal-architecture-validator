use crate::core::Order;

pub trait OrderStore {
    fn save(&self, order: &Order);
}
