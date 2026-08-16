pub mod inline {
    #[path = "custom/child.rs"]
    pub mod child;
}

#[path = "custom.rs"]
pub mod custom;
