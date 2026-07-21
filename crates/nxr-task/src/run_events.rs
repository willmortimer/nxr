//! Run-scoped enrichment of execution events (`run_id`, `seq`, timestamps).

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::events::{Event, EventSink, RunOutcome};

/// Wraps an [`EventSink`] and fills additive run metadata on every event.
pub struct RunEventDecorator<S> {
    inner: S,
    run_id: String,
    seq: u64,
    run_started: Instant,
    run_started_at: String,
    node_wall_start: BTreeMap<String, (Instant, String)>,
}

impl<S> RunEventDecorator<S> {
    /// Begin a new run, generating a unique `run_id`.
    #[must_use]
    pub fn new(inner: S) -> Self {
        let now = SystemTime::now();
        let nanos = now
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            inner,
            run_id: format!("run-{nanos:x}"),
            seq: 0,
            run_started: Instant::now(),
            run_started_at: format_rfc3339_utc(now),
            node_wall_start: BTreeMap::new(),
        }
    }

    /// Borrow the assigned run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Recover the wrapped sink.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    fn enrich(&mut self, event: Event) -> Event {
        match event {
            Event::PlanCreated {
                root,
                roots,
                node_count,
                run_id: _,
            } => Event::PlanCreated {
                root,
                roots,
                node_count,
                run_id: Some(self.run_id.clone()),
            },
            Event::NodeQueued { node, seq: _ } => Event::NodeQueued {
                node,
                seq: Some(self.next_seq()),
            },
            Event::NodeStarted {
                node,
                started_at: _,
                seq: _,
            } => {
                let wall = SystemTime::now();
                let stamp = format_rfc3339_utc(wall);
                self.node_wall_start
                    .insert(node.clone(), (Instant::now(), stamp.clone()));
                Event::NodeStarted {
                    node,
                    started_at: Some(stamp),
                    seq: Some(self.next_seq()),
                }
            }
            Event::NodeExited {
                node,
                code,
                status,
                duration_ms,
                started_at: _,
                finished_at: _,
                reason,
                seq: _,
            } => {
                let finished_at = format_rfc3339_utc(SystemTime::now());
                let (started_at, measured_ms) =
                    if let Some((instant, stamp)) = self.node_wall_start.remove(&node) {
                        (Some(stamp), Some(duration_ms_from(instant.elapsed())))
                    } else {
                        (None, duration_ms)
                    };
                Event::NodeExited {
                    node,
                    code,
                    status,
                    duration_ms: measured_ms.or(duration_ms),
                    started_at,
                    finished_at: Some(finished_at),
                    reason,
                    seq: Some(self.next_seq()),
                }
            }
            Event::RunCompleted {
                success,
                run_id: _,
                status,
                duration_ms: _,
                started_at: _,
                finished_at: _,
            } => {
                let finished_at = format_rfc3339_utc(SystemTime::now());
                Event::RunCompleted {
                    success,
                    run_id: Some(self.run_id.clone()),
                    status: status.or(Some(if success {
                        RunOutcome::Succeeded
                    } else {
                        RunOutcome::Failed
                    })),
                    duration_ms: Some(duration_ms_from(self.run_started.elapsed())),
                    started_at: Some(self.run_started_at.clone()),
                    finished_at: Some(finished_at),
                }
            }
            other => other,
        }
    }
}

impl<S: EventSink> EventSink for RunEventDecorator<S> {
    fn emit(&mut self, event: Event) {
        let enriched = self.enrich(event);
        self.inner.emit(enriched);
    }
}

fn duration_ms_from(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Format `system` as an RFC 3339 UTC timestamp with millisecond precision.
#[must_use]
pub fn format_rfc3339_utc(system: SystemTime) -> String {
    let duration = system.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    let (year, month, day, hour, minute, second) = civil_utc_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Civil UTC date/time from Unix seconds (Howard Hinnant algorithm).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn civil_utc_from_unix(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let hour = (tod / 3600) as u32;
    let minute = ((tod % 3600) / 60) as u32;
    let second = (tod % 60) as u32;

    // days since 1970-01-01 → civil (y, m, d)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + i64::from(m <= 2)) as i32;
    (year, m, d, hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::{RunEventDecorator, format_rfc3339_utc};
    use crate::events::{Event, EventSink, RecordingSink};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn decorator_fills_run_id_seq_and_timestamps() {
        let mut sink = RunEventDecorator::new(RecordingSink::new());
        sink.emit(Event::plan_created("root", None, 1));
        sink.emit(Event::node_started("a"));
        sink.emit(Event::node_exited("a", Some(0)));
        sink.emit(Event::run_completed(true));

        let events = sink.into_inner().into_events();
        match &events[0] {
            Event::PlanCreated {
                run_id: Some(id), ..
            } => assert!(id.starts_with("run-")),
            other => panic!("unexpected {other:?}"),
        }
        match &events[1] {
            Event::NodeStarted {
                started_at: Some(ts),
                seq: Some(1),
                ..
            } => assert!(ts.ends_with('Z')),
            other => panic!("unexpected {other:?}"),
        }
        match &events[2] {
            Event::NodeExited {
                duration_ms: Some(_),
                started_at: Some(_),
                finished_at: Some(_),
                seq: Some(2),
                ..
            } => {}
            other => panic!("unexpected {other:?}"),
        }
        match &events[3] {
            Event::RunCompleted {
                run_id: Some(_),
                duration_ms: Some(_),
                started_at: Some(_),
                finished_at: Some(_),
                ..
            } => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rfc3339_epoch() {
        let stamp = format_rfc3339_utc(UNIX_EPOCH + Duration::from_secs(0));
        assert_eq!(stamp, "1970-01-01T00:00:00.000Z");
    }
}
