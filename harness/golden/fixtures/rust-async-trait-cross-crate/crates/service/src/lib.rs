use harness_domain::Harness;

pub struct HarnessGrpcService {
    harness: std::sync::Arc<dyn Harness>,
}

impl HarnessGrpcService {
    pub async fn handle(&self) {
        self.harness.check().await;
    }
}
