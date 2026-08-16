mod adapter {
    pub struct Console;
}

mod core {
    pub fn start(console: crate::adapter::Console) {
        let _ = console;
    }
}
