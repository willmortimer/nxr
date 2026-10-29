//! Multi-worker scheduling skeleton (V4.5 / V5 scaffolding).
//!
//! Nix remote builders remain Nix-owned. This registry selects NXR workers for
//! mutable or interactive leaves ([`Operation::Remote`]) without storing NixPlane
//! fleet Assignments ([ADR-0174](../../../docs/adr/0174-nixplane-fleet-state-boundary.md)).

use serde::{Deserialize, Serialize};

/// Capability tags advertised by one worker enrollment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub cpu: u32,
    #[serde(default)]
    pub memory_mib: u64,
}

/// Requirements extracted from a remote operation or session start.
#[derive(Clone, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkerRequirements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub cpu: u32,
    #[serde(default)]
    pub memory_mib: u64,
}

/// Minimal scheduling surface for a future coordinator role.
pub trait WorkerScheduler {
    /// Register or replace one worker advertisement.
    ///
    /// # Errors
    ///
    /// Returns an error when the descriptor is invalid.
    fn register_worker(&mut self, worker: WorkerDescriptor) -> Result<(), String>;

    /// Pick a worker that satisfies `requirements`, if any.
    fn select_worker(&self, requirements: &WorkerRequirements) -> Option<&WorkerDescriptor>;
}

/// In-memory registry used in tests and future `worker.register` wiring.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalWorkerRegistry {
    workers: Vec<WorkerDescriptor>,
}

impl WorkerScheduler for LocalWorkerRegistry {
    fn register_worker(&mut self, worker: WorkerDescriptor) -> Result<(), String> {
        if worker.id.trim().is_empty() {
            return Err("worker id must not be empty".into());
        }
        if let Some(existing) = self.workers.iter_mut().find(|entry| entry.id == worker.id) {
            *existing = worker;
        } else {
            self.workers.push(worker);
        }
        Ok(())
    }

    fn select_worker(&self, requirements: &WorkerRequirements) -> Option<&WorkerDescriptor> {
        self.workers.iter().find(|worker| {
            requirements.tags.iter().all(|tag| worker.tags.iter().any(|t| t == tag))
                && worker.cpu >= requirements.cpu
                && worker.memory_mib >= requirements.memory_mib
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_selects_by_tags_and_capacity() {
        let mut registry = LocalWorkerRegistry::default();
        registry
            .register_worker(WorkerDescriptor {
                id: "linux-a".to_owned(),
                tags: vec!["linux".to_owned(), "winbuild".to_owned()],
                cpu: 4,
                memory_mib: 8192,
            })
            .unwrap();
        registry
            .register_worker(WorkerDescriptor {
                id: "linux-b".to_owned(),
                tags: vec!["linux".to_owned()],
                cpu: 2,
                memory_mib: 4096,
            })
            .unwrap();
        let requirements = WorkerRequirements {
            tags: vec!["winbuild".to_owned()],
            cpu: 2,
            memory_mib: 2048,
        };
        let picked = registry.select_worker(&requirements).expect("worker");
        assert_eq!(picked.id, "linux-a");
    }

    #[test]
    fn registry_rejects_empty_worker_id() {
        let mut registry = LocalWorkerRegistry::default();
        let error = registry
            .register_worker(WorkerDescriptor {
                id: "   ".to_owned(),
                tags: Vec::new(),
                cpu: 1,
                memory_mib: 1,
            })
            .unwrap_err();
        assert!(error.contains("empty"), "{error}");
    }
}
