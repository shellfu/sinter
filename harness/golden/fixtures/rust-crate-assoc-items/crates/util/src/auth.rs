/// Token validator.
pub struct Validator {
    pub audience: u32,
}

impl Validator {
    /// Builds a validator.
    pub fn new(audience: u32) -> Validator {
        Validator { audience }
    }
}

/// Names the validator kind.
pub fn validator_kind() -> u32 {
    1
}
