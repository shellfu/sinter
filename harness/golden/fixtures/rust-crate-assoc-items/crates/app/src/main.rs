fn main() {
    let c = acme_util::Config::new();
    let v = acme_util::Validator::new(7);
    acme_util::validator_kind();
    let _ = (c, v);
}
