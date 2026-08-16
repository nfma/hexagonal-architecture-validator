use core::fmt::Debug;
use core::mem::size_of;
use std::collections::HashMap;
use std::fmt::Display;

pub fn standard_library<T: Debug + Display>() -> (HashMap<String, T>, usize) {
    (HashMap::new(), size_of::<T>())
}
