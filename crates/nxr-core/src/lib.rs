//! Shared models, schema versions, diagnostics, and policy types.

pub mod config;
pub mod diagnostics;
pub mod model;

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_smoke() {
        assert_eq!(2 + 2, 4);
    }
}
