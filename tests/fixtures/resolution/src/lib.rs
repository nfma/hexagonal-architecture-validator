pub mod consumer;
pub mod core;
pub mod inner;
pub mod outer;

use core::DomainEntity;

pub fn domain() -> DomainEntity {
    DomainEntity
}
