pub mod inner;

use inner::Thing;
use Option::*;

pub fn optional(value: Option<Thing>) -> Option<Thing> {
    match value {
        Some(value) => Some(value),
        None => None,
    }
}
