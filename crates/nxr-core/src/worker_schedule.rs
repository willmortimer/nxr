//! Multi-worker scheduling skeleton (V4.5 / V5 scaffolding).
//!
//! Nix remote builders remain Nix-owned. This registry selects NXR workers for
//! mutable or interactive leaves ([`Operation::Remote`]) without storing NixPlane
//! fleet Assignments ([ADR-0174](../../../docs/adr/0174-nixplane-fleet-state-boundary.md)).
//!
//! [`WorkerScheduler::select_and_reserve`] on [`LocalWorkerRegistry`] is
//! **local and in-process only**: it does not coordinate across daemons or hosts.
//! Production remote scheduling must add an external lease or coordinator.

use std::collections::BTreeMap;

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

/// One worker held for a single in-flight remote attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerReservation {
    expires_at: u64,
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
    ///
    /// Workers with a non-expired reservation are skipped.
    fn select_worker(&self, requirements: &WorkerRequirements) -> Option<&WorkerDescriptor>;

    /// Hold `worker_id` until `now + ttl_secs`. At most one active reservation per worker.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker is unknown or already reserved.
    fn reserve_worker(&mut self, worker_id: &str, now: u64, ttl_secs: u64) -> Result<(), String>;

    /// Select one eligible worker and reserve it until `now + ttl_secs` in one step.
    ///
    /// Implementations such as [`LocalWorkerRegistry`] are in-process only; callers
    /// must not treat this as a distributed lock.
    ///
    /// # Errors
    ///
    /// Returns an error when the reservation cannot be recorded.
    fn select_and_reserve(
        &mut self,
        requirements: &WorkerRequirements,
        now: u64,
        ttl_secs: u64,
    ) -> Result<Option<WorkerDescriptor>, String>;
}

/// In-memory registry used in tests and future `worker.register` wiring.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalWorkerRegistry {
    workers: Vec<WorkerDescriptor>,
    reservations: BTreeMap<String, WorkerReservation>,
}

impl LocalWorkerRegistry {
    fn reservation_active(&self, worker_id: &str, now: u64) -> bool {
        self.reservations
            .get(worker_id)
            .is_some_and(|reservation| reservation.expires_at > now)
    }

    fn worker_satisfies(worker: &WorkerDescriptor, requirements: &WorkerRequirements) -> bool {
        requirements
            .tags
            .iter()
            .all(|tag| worker.tags.iter().any(|t| t == tag))
            && worker.cpu >= requirements.cpu
            && worker.memory_mib >= requirements.memory_mib
    }

    fn prune_expired(&mut self, now: u64) {
        self.reservations
            .retain(|_, reservation| reservation.expires_at > now);
    }

    /// Release a reservation early (for example after the attempt finishes).
    pub fn release_worker(&mut self, worker_id: &str) {
        self.reservations.remove(worker_id);
    }
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
        let now = unix_now_secs();
        self.workers.iter().find(|worker| {
            !self.reservation_active(&worker.id, now)
                && Self::worker_satisfies(worker, requirements)
        })
    }

    fn reserve_worker(&mut self, worker_id: &str, now: u64, ttl_secs: u64) -> Result<(), String> {
        self.prune_expired(now);
        if !self.workers.iter().any(|worker| worker.id == worker_id) {
            return Err(format!("unknown worker `{worker_id}`"));
        }
        if self.reservation_active(worker_id, now) {
            return Err(format!("worker `{worker_id}` is already reserved"));
        }
        self.reservations.insert(
            worker_id.to_owned(),
            WorkerReservation {
                expires_at: now.saturating_add(ttl_secs),
            },
        );
        Ok(())
    }

    fn select_and_reserve(
        &mut self,
        requirements: &WorkerRequirements,
        now: u64,
        ttl_secs: u64,
    ) -> Result<Option<WorkerDescriptor>, String> {
        self.prune_expired(now);
        let picked_id = self
            .workers
            .iter()
            .find(|worker| {
                !self.reservation_active(&worker.id, now)
                    && Self::worker_satisfies(worker, requirements)
            })
            .map(|worker| worker.id.clone());
        let Some(worker_id) = picked_id else {
            return Ok(None);
        };
        self.reserve_worker(&worker_id, now, ttl_secs)?;
        Ok(self
            .workers
            .iter()
            .find(|worker| worker.id == worker_id)
            .cloned())
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_workers(registry: &mut LocalWorkerRegistry) {
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
    }

    #[test]
    fn registry_selects_by_tags_and_capacity() {
        let mut registry = LocalWorkerRegistry::default();
        sample_workers(&mut registry);
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

    #[test]
    fn reserve_refuses_overcommit_on_one_worker() {
        let mut registry = LocalWorkerRegistry::default();
        sample_workers(&mut registry);
        let now = 1_000_u64;
        registry.reserve_worker("linux-a", now, 60).expect("first");
        let err = registry
            .reserve_worker("linux-a", now, 60)
            .expect_err("second");
        assert!(err.contains("already reserved"), "{err}");
    }

    #[test]
    fn select_skips_reserved_worker() {
        let mut registry = LocalWorkerRegistry::default();
        sample_workers(&mut registry);
        let requirements = WorkerRequirements {
            tags: vec!["winbuild".to_owned()],
            cpu: 2,
            memory_mib: 2048,
        };
        let now = unix_now_secs();
        registry.reserve_worker("linux-a", now, 3600).unwrap();
        let picked = registry.select_worker(&requirements);
        assert!(picked.is_none(), "only matching worker was reserved");
    }

    #[test]
    fn expired_reservation_allows_re_reserve() {
        let mut registry = LocalWorkerRegistry::default();
        sample_workers(&mut registry);
        registry.reserve_worker("linux-b", 100, 10).unwrap();
        registry
            .reserve_worker("linux-b", 200, 10)
            .expect("prior reservation expired");
    }

    #[test]
    fn select_and_reserve_prevents_double_overcommit() {
        let mut registry = LocalWorkerRegistry::default();
        sample_workers(&mut registry);
        let requirements = WorkerRequirements {
            tags: vec!["winbuild".to_owned()],
            cpu: 2,
            memory_mib: 2048,
        };
        let now = 5_000_u64;
        let first = registry
            .select_and_reserve(&requirements, now, 120)
            .unwrap()
            .expect("first reservation");
        assert_eq!(first.id, "linux-a");
        let second = registry
            .select_and_reserve(&requirements, now, 120)
            .unwrap();
        assert!(
            second.is_none(),
            "second select must not overcommit linux-a"
        );
    }

    #[test]
    fn split_select_then_reserve_allows_overcommit_race() {
        let mut registry = LocalWorkerRegistry::default();
        sample_workers(&mut registry);
        let requirements = WorkerRequirements {
            tags: vec!["linux".to_owned()],
            cpu: 1,
            memory_mib: 1024,
        };
        let now = 9_000_u64;
        let picked = registry.select_worker(&requirements).unwrap().id.clone();
        let also = registry.select_worker(&requirements).unwrap().id.clone();
        assert_eq!(picked, also, "non-atomic path may pick the same worker");
        registry.reserve_worker(&picked, now, 30).unwrap();
        let err = registry
            .reserve_worker(&also, now, 30)
            .expect_err("second reserve must fail");
        assert!(err.contains("already reserved"), "{err}");
    }
}
