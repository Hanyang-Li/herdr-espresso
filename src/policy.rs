use crate::consts::{RENEW_INTERVAL, STOP_GRACE};
use std::time::Instant;

pub fn active(status: &str) -> bool {
    matches!(status, "working" | "blocked")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    RotateLease,
    KillEspresso,
}

#[derive(Debug, Default)]
pub struct Machine {
    active: bool,
    espresso_up: bool,
    pending_stop: Option<Instant>,
    next_renew: Option<Instant>,
}

impl Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_status(&mut self, active: bool, now: Instant) -> Vec<Action> {
        self.active = active;
        if active {
            self.pending_stop = None;
            if !self.espresso_up {
                self.espresso_up = true;
                self.next_renew = Some(now + RENEW_INTERVAL);
                return vec![Action::RotateLease];
            }
            return vec![];
        }
        if self.espresso_up && self.pending_stop.is_none() {
            self.pending_stop = Some(now + STOP_GRACE);
        }
        vec![]
    }

    pub fn on_timer(&mut self, now: Instant) -> Vec<Action> {
        if let Some(stop) = self.pending_stop {
            if now >= stop {
                self.pending_stop = None;
                self.next_renew = None;
                self.espresso_up = false;
                return vec![Action::KillEspresso];
            }
        }
        if self.active {
            if let Some(renew) = self.next_renew {
                if now >= renew {
                    self.next_renew = Some(now + RENEW_INTERVAL);
                    return vec![Action::RotateLease];
                }
            }
        }
        vec![]
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [self.pending_stop, self.next_renew]
            .into_iter()
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{RENEW_INTERVAL, STOP_GRACE};
    use std::time::Instant;

    #[test]
    fn active_only_for_working_and_blocked() {
        assert!(active("working"));
        assert!(active("blocked"));
        assert!(!active("idle"));
        assert!(!active("done"));
        assert!(!active("unknown"));
        assert!(!active(""));
    }

    #[test]
    fn activating_opens_espresso_immediately_and_schedules_renew() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        assert_eq!(m.on_status(true, t0), vec![Action::RotateLease]);
        assert_eq!(m.next_deadline(), Some(t0 + RENEW_INTERVAL));
    }

    #[test]
    fn repeat_active_status_does_nothing() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        assert_eq!(m.on_status(true, t0), vec![]);
    }

    #[test]
    fn renew_timer_rotates_and_reschedules() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        let t1 = t0 + RENEW_INTERVAL;
        assert_eq!(m.on_timer(t1), vec![Action::RotateLease]);
        assert_eq!(m.next_deadline(), Some(t1 + RENEW_INTERVAL));
    }

    #[test]
    fn inactive_arms_stop_grace_then_kills() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        assert_eq!(m.on_status(false, t0), vec![]); // no immediate kill
        assert_eq!(m.next_deadline(), Some(t0 + STOP_GRACE));
        let t1 = t0 + STOP_GRACE;
        assert_eq!(m.on_timer(t1), vec![Action::KillEspresso]);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn reactivating_before_grace_cancels_kill() {
        let t0 = Instant::now();
        let mut m = Machine::new();
        m.on_status(true, t0);
        m.on_status(false, t0);
        // back to active before grace fires: no new open (espresso still up), grace cancelled
        assert_eq!(m.on_status(true, t0), vec![]);
        assert_eq!(m.next_deadline(), Some(t0 + RENEW_INTERVAL));
    }
}
