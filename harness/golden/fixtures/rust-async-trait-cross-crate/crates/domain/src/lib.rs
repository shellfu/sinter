#[async_trait]
pub trait Harness {
    async fn check(&self);
}
