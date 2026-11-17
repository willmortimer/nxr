//! Receipt binding checks for operation and workspace identity.

/// Returns `Ok(())` only when both stored ids are non-empty and equal the requested ids.
///
/// Any empty stored id, empty requested id, or disagreement returns `Err` whose text
/// contains `does not match`.
pub fn receipt_binding_matches(
    stored_operation_id: &str,
    stored_workspace_id: &str,
    requested_operation_id: &str,
    requested_workspace_id: &str,
) -> Result<(), String> {
    if stored_operation_id.is_empty()
        || stored_workspace_id.is_empty()
        || requested_operation_id.is_empty()
        || requested_workspace_id.is_empty()
        || stored_operation_id != requested_operation_id
        || stored_workspace_id != requested_workspace_id
    {
        return Err(
            "receipt binding does not match (stored operation_id/workspace_id vs requested)"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::receipt_binding_matches;

    #[test]
    fn matching_ids_succeed() {
        assert!(receipt_binding_matches("op-1", "ws-1", "op-1", "ws-1").is_ok());
    }

    #[test]
    fn different_operation_id_fails() {
        let err = receipt_binding_matches("op-1", "ws-1", "op-2", "ws-1").unwrap_err();
        assert!(err.contains("does not match"));
    }

    #[test]
    fn different_workspace_id_fails() {
        let err = receipt_binding_matches("op-1", "ws-1", "op-1", "ws-2").unwrap_err();
        assert!(err.contains("does not match"));
    }

    #[test]
    fn empty_stored_operation_id_fails() {
        let err = receipt_binding_matches("", "ws-1", "op-1", "ws-1").unwrap_err();
        assert!(err.contains("does not match"));
    }

    #[test]
    fn empty_stored_workspace_id_fails() {
        let err = receipt_binding_matches("op-1", "", "op-1", "ws-1").unwrap_err();
        assert!(err.contains("does not match"));
    }
}
