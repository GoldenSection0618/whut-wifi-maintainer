use crate::network::Reachability;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Healthy,
    Partial,
    Wait,
    Authenticate,
}

pub struct Monitor {
    offline_rounds: u8,
    next_auth: Duration,
    retry_delay: Duration,
}

impl Monitor {
    pub fn new(interval: Duration) -> Self {
        Self {
            offline_rounds: 0,
            next_auth: Duration::ZERO,
            retry_delay: interval.max(Duration::from_secs(30)),
        }
    }

    pub fn disconnected(&mut self) {
        self.offline_rounds = 0;
    }

    pub fn observe(&mut self, reachability: Reachability, now: Duration) -> Action {
        match reachability {
            Reachability::Online => {
                self.offline_rounds = 0;
                Action::Healthy
            }
            Reachability::Partial => {
                self.offline_rounds = 0;
                Action::Partial
            }
            Reachability::Offline => {
                self.offline_rounds = self.offline_rounds.saturating_add(1);
                if self.offline_rounds >= 2 && now >= self.next_auth {
                    Action::Authenticate
                } else {
                    Action::Wait
                }
            }
        }
    }

    pub fn attempted(&mut self, now: Duration) {
        self.offline_rounds = 0;
        self.next_auth = now.saturating_add(self.retry_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_two_consecutive_failures_and_never_authenticates_partial_network() {
        let mut monitor = Monitor::new(Duration::from_secs(30));
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::ZERO),
            Action::Wait
        );
        assert_eq!(
            monitor.observe(Reachability::Partial, Duration::from_secs(30)),
            Action::Partial
        );
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::from_secs(60)),
            Action::Wait
        );
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::from_secs(90)),
            Action::Authenticate
        );
    }

    #[test]
    fn accepted_login_does_not_turn_failed_probes_into_online_state() {
        let mut monitor = Monitor::new(Duration::from_secs(1));
        monitor.attempted(Duration::ZERO);
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::from_secs(1)),
            Action::Wait
        );
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::from_secs(2)),
            Action::Wait
        );
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::from_secs(30)),
            Action::Authenticate
        );
        assert_eq!(
            monitor.observe(Reachability::Online, Duration::from_secs(31)),
            Action::Healthy
        );
    }

    #[test]
    fn reconnect_starts_new_failure_sequence() {
        let mut monitor = Monitor::new(Duration::from_secs(30));
        monitor.observe(Reachability::Offline, Duration::ZERO);
        monitor.disconnected();
        assert_eq!(
            monitor.observe(Reachability::Offline, Duration::from_secs(30)),
            Action::Wait
        );
    }
}
