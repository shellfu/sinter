use crate::pb::harness_service_client::HarnessServiceClient;

pub async fn adjudicate_remote(req: AdjudicateRequest) -> AdjudicateReply {
    let mut client = HarnessServiceClient::connect("http://[::1]:50051").await.unwrap();
    client.adjudicates(req).await.unwrap().into_inner()
}
