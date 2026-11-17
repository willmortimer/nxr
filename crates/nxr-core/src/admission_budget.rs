//! Local host CPU/memory admission budget helpers.

/// Capacity available for local reservations on this host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostBudget {
    pub cpu: u32,
    pub memory_mib: u64,
}

/// One pending or active resource reservation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub cpu: u32,
    pub memory_mib: u64,
    pub reason: String,
}

/// Resolve the host budget from optional env overrides and parallelism.
///
/// A parsed env value wins. Unset or unparseable CPU uses `parallelism.max(1)`.
/// Unset or unparseable memory uses `2048 * cpu` (saturating). Never returns
/// [`u32::MAX`] or [`u64::MAX`]. Reservation inputs are not consulted.
#[must_use]
pub fn host_budget(
    cpu_env: Option<&str>,
    memory_env: Option<&str>,
    parallelism: u32,
) -> HostBudget {
    let cpu = parse_cpu(cpu_env).unwrap_or_else(|| clamp_cpu(parallelism.max(1)));
    let memory_mib = parse_memory(memory_env)
        .unwrap_or_else(|| clamp_memory(2048u64.saturating_mul(u64::from(cpu))));
    HostBudget { cpu, memory_mib }
}

/// Build a reservation from an explicit request, applying defaults for a zero request.
///
/// A zero request (`cpu == 0` and `memory_mib == 0`) becomes 1 CPU / 512 MiB, or
/// 2 CPU / 2048 MiB when `heavy`. Nonzero requests are kept as-is. A large
/// request does not change any host budget.
#[must_use]
pub fn reservation_for(requested_cpu: u32, requested_memory_mib: u64, heavy: bool) -> Reservation {
    if requested_cpu == 0 && requested_memory_mib == 0 {
        if heavy {
            Reservation {
                cpu: 2,
                memory_mib: 2048,
                reason: "default heavy reservation (2 cpu, 2048 MiB)".into(),
            }
        } else {
            Reservation {
                cpu: 1,
                memory_mib: 512,
                reason: "default reservation (1 cpu, 512 MiB)".into(),
            }
        }
    } else {
        Reservation {
            cpu: requested_cpu,
            memory_mib: requested_memory_mib,
            reason: format!("explicit request ({requested_cpu} cpu, {requested_memory_mib} MiB)"),
        }
    }
}

/// Admit `reservation` when `used + reservation` still fits in `host`.
///
/// # Errors
///
/// Returns an error containing `local budget exceeded` when the reservation
/// would not fit.
pub fn admit_reservation(
    used_cpu: u32,
    used_memory_mib: u64,
    reservation: &Reservation,
    host: HostBudget,
) -> Result<(), String> {
    let next_cpu = used_cpu.saturating_add(reservation.cpu);
    let next_memory = used_memory_mib.saturating_add(reservation.memory_mib);
    if next_cpu <= host.cpu && next_memory <= host.memory_mib {
        Ok(())
    } else {
        Err("local budget exceeded".into())
    }
}

fn parse_cpu(cpu_env: Option<&str>) -> Option<u32> {
    let value = cpu_env?.parse::<u32>().ok()?;
    if value == u32::MAX { None } else { Some(value) }
}

fn parse_memory(memory_env: Option<&str>) -> Option<u64> {
    let value = memory_env?.parse::<u64>().ok()?;
    if value == u64::MAX { None } else { Some(value) }
}

fn clamp_cpu(cpu: u32) -> u32 {
    if cpu == u32::MAX { u32::MAX - 1 } else { cpu }
}

fn clamp_memory(memory_mib: u64) -> u64 {
    if memory_mib == u64::MAX {
        u64::MAX - 1
    } else {
        memory_mib
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_env_uses_parallelism_and_is_not_max() {
        let budget = host_budget(None, None, 4);
        assert_eq!(budget.cpu, 4);
        assert_eq!(budget.memory_mib, 2048 * 4);
        assert_ne!(budget.cpu, u32::MAX);
        assert_ne!(budget.memory_mib, u64::MAX);

        let zero_parallelism = host_budget(None, None, 0);
        assert_eq!(zero_parallelism.cpu, 1);
        assert_eq!(zero_parallelism.memory_mib, 2048);
        assert_ne!(zero_parallelism.cpu, u32::MAX);
        assert_ne!(zero_parallelism.memory_mib, u64::MAX);
    }

    #[test]
    fn caller_request_of_99_is_not_copied_into_host_budget() {
        let _reservation = reservation_for(99, 99_000, false);
        let budget = host_budget(None, None, 4);
        assert_ne!(budget.cpu, 99);
        assert_ne!(budget.memory_mib, 99_000);
        assert_eq!(budget.cpu, 4);
        assert_eq!(budget.memory_mib, 2048 * 4);
    }

    #[test]
    fn zero_request_reserves_one_cpu_and_512_mib() {
        let reservation = reservation_for(0, 0, false);
        assert_eq!(reservation.cpu, 1);
        assert_eq!(reservation.memory_mib, 512);
        assert!(reservation.reason.contains("default"));
    }

    #[test]
    fn two_reservations_of_one_cpu_fit_in_host_of_two_and_third_exceeds() {
        let host = HostBudget {
            cpu: 2,
            memory_mib: 4096,
        };
        let unit = reservation_for(1, 512, false);

        assert!(admit_reservation(0, 0, &unit, host).is_ok());
        assert!(admit_reservation(1, 512, &unit, host).is_ok());
        let third = admit_reservation(2, 1024, &unit, host);
        assert!(third.is_err());
        assert!(third.unwrap_err().contains("local budget exceeded"));
    }
}
