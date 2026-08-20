pub struct Arc<T: ?Sized>(*const T);

pub trait Harness {
    fn check(&self);
}

pub struct CedarPolicyHarness;

impl Harness for CedarPolicyHarness {
    fn check(&self) {}
}

pub struct HarnessGrpcService {
    harness: Arc<dyn Harness>,
}

impl HarnessGrpcService {
    pub fn handle(&self) {
        self.harness.check();
    }
}
