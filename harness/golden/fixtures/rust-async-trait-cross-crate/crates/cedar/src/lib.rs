use harness_domain::Harness;

pub struct CedarPolicyHarness;

#[async_trait]
impl Harness for CedarPolicyHarness {
    async fn check(&self) {}
}
