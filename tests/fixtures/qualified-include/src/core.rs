use crate::adapter::Adapter;

include!("generated_plain.rs");
std::include!("generated_std.rs");
core::include!("generated_core.rs");

pub fn adapter() -> Adapter {
    Adapter
}
