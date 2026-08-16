#[cfg(any())]
mod nightly_only {
    use test::Bencher;

    fn benchmark(_: &mut Bencher) {}
}
