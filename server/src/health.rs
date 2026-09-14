use async_trait::async_trait;

#[async_trait]
pub trait DependencyHealth: Send + Sync {
    async fn healthy(&self) -> bool;

    fn name(&self) -> &'static str;
}
