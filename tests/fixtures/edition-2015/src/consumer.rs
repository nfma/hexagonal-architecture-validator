use std::fmt::Debug;
use write::Flush;

pub fn flush<T>(value: &T)
where
    T: Debug + Flush,
{
    let _ = value;
}
