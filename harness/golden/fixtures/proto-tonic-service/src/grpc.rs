use crate::pb::harness_service_server::HarnessService;

pub struct HarnessGrpcService;

impl HarnessGrpcService {
    pub fn adjudicates(&self) -> bool {
        true
    }
}

impl HarnessService for HarnessGrpcService {
    async fn adjudicates(&self, req: AdjudicateRequest) -> AdjudicateReply {
        AdjudicateReply { accepted: true }
    }

    async fn get_verdict_by_id(&self, id: VerdictId) -> AdjudicateReply {
        AdjudicateReply { accepted: false }
    }
}
