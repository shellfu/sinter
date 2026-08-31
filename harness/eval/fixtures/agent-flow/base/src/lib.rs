/// Low-level operation whose callers form a small blast radius.
pub fn leaf() -> usize {
    1
}

/// Domain dispatch step between the entry point and leaf operation.
pub fn dispatch() -> usize {
    leaf()
}

/// Public entry point for the fixture's request flow.
pub fn entry() -> usize {
    dispatch()
}

/// Maps endpoint roles to the authorization scopes they may request.
pub fn endpoint_role_scope_table() -> &'static [(&'static str, &'static str)] {
    &[("operator", "endpoint:write"), ("viewer", "endpoint:read")]
}

/// Legacy role-to-scope mapping kept for delegated agent sessions.
pub fn delegated_role_scope_table() -> &'static [(&'static str, &'static str)] {
    &[("agent", "endpoint:delegate")]
}

pub mod left {
    pub fn duplicate() -> &'static str {
        "left"
    }
}

pub mod right {
    pub fn duplicate() -> &'static str {
        "right"
    }
}

/// An intentionally unresolved dependency reference.
pub fn external_reference() {
    unavailable_dependency::send();
}

#[cfg(test)]
mod tests {
    use super::dispatch;

    #[test]
    fn dispatch_works() {
        // Keep the dependency call outside the assertion macro. Syntax-only
        // indexing cannot inspect arbitrary Rust macro token trees, while a
        // direct call is evidence the graph can support without SCIP.
        let result = dispatch();
        assert!(result > 0);
    }
}
