//! One monitoring iteration. Network, clock and persistence are injectable; no terminal I/O.
use crate::config::{Config, ConfigError, SaveOutcome};
use crate::monitor::{Action, Monitor};
use crate::network::{ProbeReport, Reachability, RequestError};
use crate::portal_auth::PortalLoginOutcome;
use crate::unified_auth::CredentialVerification;
use crate::wifi::CampusWifi;
use std::time::Duration;

pub trait Network {
    fn connection(&mut self, config: &Config) -> Result<CampusWifi, &'static str>;
    fn probe(&mut self, config: &Config) -> ProbeReport;
    fn authenticate(&mut self, config: &Config) -> Result<PortalLoginOutcome, RequestError>;
    fn verify(&mut self, config: &Config) -> Result<CredentialVerification, RequestError>;
}

pub trait CredentialStore {
    fn save(&mut self, config: &Config) -> Result<SaveOutcome, ConfigError>;
}

pub enum Event {
    Disconnected(&'static str),
    Healthy,
    Partial(ProbeReport),
    Waiting,
    Recovered,
    AcceptedButUnreachable,
    BalanceInsufficient(CampusWifi),
    AuthInconclusive,
    AuthFailed(RequestError),
    VerificationInconclusive,
    VerificationFailed(RequestError),
    CredentialsSaved,
    DurabilityUnconfirmed(std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Wait(Duration),
    RequestCredentials,
    ExitCredentialsRejected,
}

pub struct Cycle {
    pub events: Vec<Event>,
    pub next: Next,
}

pub struct Runtime {
    monitor: Monitor,
    interval: Duration,
    next_verification: Duration,
    verified: bool,
    pending_credentials: bool,
    interactive: bool,
}

impl Runtime {
    pub fn new(interval: Duration, pending_credentials: bool, interactive: bool) -> Self {
        Self {
            monitor: Monitor::new(interval),
            interval,
            next_verification: Duration::ZERO,
            verified: false,
            pending_credentials,
            interactive,
        }
    }

    pub fn credentials_changed(&mut self) {
        self.verified = false;
        self.pending_credentials = true;
        self.next_verification = Duration::ZERO;
    }

    fn rejected(&self, cycle: &mut Cycle) {
        cycle.next = if self.interactive {
            Next::RequestCredentials
        } else {
            Next::ExitCredentialsRejected
        };
    }

    pub fn step(
        &mut self,
        config: &Config,
        network: &mut impl Network,
        store: &mut impl CredentialStore,
        now: impl Fn() -> Duration,
    ) -> Result<Cycle, ConfigError> {
        let mut cycle = Cycle {
            events: Vec::new(),
            next: Next::Wait(self.interval),
        };
        let connection = match network.connection(config) {
            Ok(connection) => connection,
            Err(reason) => {
                self.monitor.disconnected();
                cycle.events.push(Event::Disconnected(reason));
                return Ok(cycle);
            }
        };
        let report = network.probe(config);
        let mut reachability = report.reachability();
        match self.monitor.observe(reachability, now()) {
            Action::Healthy => cycle.events.push(Event::Healthy),
            Action::Partial => cycle.events.push(Event::Partial(report)),
            Action::Wait => cycle.events.push(Event::Waiting),
            Action::Authenticate => {
                self.monitor.attempted(now());
                match network.authenticate(config) {
                    Ok(PortalLoginOutcome::Accepted) => {
                        reachability = network.probe(config).reachability();
                        // Record partial recovery too, so it interrupts consecutive offline rounds.
                        self.monitor.observe(reachability, now());
                        cycle.events.push(if reachability == Reachability::Online {
                            Event::Recovered
                        } else {
                            Event::AcceptedButUnreachable
                        });
                    }
                    Ok(PortalLoginOutcome::CredentialsRejected) => {
                        self.rejected(&mut cycle);
                        return Ok(cycle);
                    }
                    Ok(PortalLoginOutcome::BalanceInsufficient) => {
                        cycle.events.push(Event::BalanceInsufficient(connection))
                    }
                    Ok(PortalLoginOutcome::Inconclusive) => {
                        cycle.events.push(Event::AuthInconclusive)
                    }
                    Err(error) => cycle.events.push(Event::AuthFailed(error)),
                }
            }
        }
        if reachability == Reachability::Online && !self.verified && now() >= self.next_verification
        {
            let verification = network.verify(config);
            self.next_verification =
                now().saturating_add(self.interval.max(Duration::from_secs(30)));
            match verification {
                Ok(CredentialVerification::Valid) => {
                    self.verified = true;
                    if self.pending_credentials {
                        if let SaveOutcome::DurabilityUnconfirmed(error) = store.save(config)? {
                            cycle.events.push(Event::DurabilityUnconfirmed(error));
                        }
                        self.pending_credentials = false;
                        cycle.events.push(Event::CredentialsSaved);
                    }
                }
                Ok(CredentialVerification::Invalid) => self.rejected(&mut cycle),
                Ok(CredentialVerification::Inconclusive) => {
                    cycle.events.push(Event::VerificationInconclusive)
                }
                Err(error) => cycle.events.push(Event::VerificationFailed(error)),
            }
        }
        Ok(cycle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Credentials;
    use crate::network::ProbeFailure;
    use std::cell::Cell;
    use std::collections::VecDeque;

    struct FakeNetwork {
        probes: VecDeque<Reachability>,
        logins: VecDeque<Result<PortalLoginOutcome, RequestError>>,
        verifications: VecDeque<Result<CredentialVerification, RequestError>>,
        calls: Vec<&'static str>,
    }
    impl Network for FakeNetwork {
        fn connection(&mut self, _: &Config) -> Result<CampusWifi, &'static str> {
            Ok(CampusWifi::Dorm)
        }
        fn probe(&mut self, _: &Config) -> ProbeReport {
            self.calls.push("probe");
            let reachability = self.probes.pop_front().expect("unexpected probe");
            ProbeReport {
                http: if reachability == Reachability::Offline {
                    Err(ProbeFailure::Transport)
                } else {
                    Ok(())
                },
                https: if reachability == Reachability::Online {
                    Ok(())
                } else {
                    Err(ProbeFailure::Transport)
                },
            }
        }
        fn authenticate(&mut self, _: &Config) -> Result<PortalLoginOutcome, RequestError> {
            self.calls.push("login");
            self.logins.pop_front().expect("unexpected login")
        }
        fn verify(&mut self, _: &Config) -> Result<CredentialVerification, RequestError> {
            self.calls.push("verify");
            self.verifications
                .pop_front()
                .expect("unexpected verification")
        }
    }
    #[derive(Default)]
    struct Store {
        saved: usize,
        fail: bool,
    }
    impl CredentialStore for Store {
        fn save(&mut self, _: &Config) -> Result<SaveOutcome, ConfigError> {
            if self.fail {
                return Err(ConfigError::Io(std::io::Error::other(
                    "injected write failure",
                )));
            }
            self.saved += 1;
            Ok(SaveOutcome::Saved)
        }
    }
    fn config() -> Config {
        Config::new(Credentials {
            username: "test".into(),
            password: "secret".into(),
        })
    }
    fn network(probes: Vec<Reachability>) -> FakeNetwork {
        FakeNetwork {
            probes: probes.into(),
            logins: VecDeque::new(),
            verifications: VecDeque::new(),
            calls: vec![],
        }
    }
    fn runtime() -> Runtime {
        Runtime::new(Duration::from_secs(1), true, false)
    }

    #[test]
    fn accepted_then_partial_never_reports_recovery_or_saves_unverified_credentials() {
        use Reachability::*;
        let mut network = network(vec![Offline, Offline, Partial, Partial]);
        network.logins.push_back(Ok(PortalLoginOutcome::Accepted));
        let (mut runtime, mut store) = (runtime(), Store::default());
        let clock = Cell::new(Duration::ZERO);
        for seconds in [0, 1, 31] {
            clock.set(Duration::from_secs(seconds));
            let cycle = runtime
                .step(&config(), &mut network, &mut store, || clock.get())
                .unwrap();
            assert!(
                !cycle
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Recovered | Event::CredentialsSaved))
            );
            assert_eq!(cycle.next, Next::Wait(Duration::from_secs(1)));
        }
        assert_eq!(network.calls, ["probe", "probe", "login", "probe", "probe"]);
        assert_eq!(store.saved, 0);
    }

    #[test]
    fn complete_recovery_verifies_and_saves_once() {
        use Reachability::*;
        let mut network = network(vec![Offline, Offline, Online, Online]);
        network.logins.push_back(Ok(PortalLoginOutcome::Accepted));
        network
            .verifications
            .push_back(Ok(CredentialVerification::Valid));
        let (mut runtime, mut store) = (runtime(), Store::default());
        runtime
            .step(&config(), &mut network, &mut store, || Duration::ZERO)
            .unwrap();
        let recovered = runtime
            .step(&config(), &mut network, &mut store, || {
                Duration::from_secs(1)
            })
            .unwrap();
        assert!(
            recovered
                .events
                .iter()
                .any(|event| matches!(event, Event::Recovered))
        );
        runtime
            .step(&config(), &mut network, &mut store, || {
                Duration::from_secs(2)
            })
            .unwrap();
        assert_eq!(
            network.calls,
            ["probe", "probe", "login", "probe", "verify", "probe"]
        );
        assert_eq!(store.saved, 1);
    }

    #[test]
    fn authentication_timeout_keeps_probing_and_throttles_retries() {
        let mut network = network(vec![Reachability::Offline; 5]);
        network
            .logins
            .extend([Err(RequestError::Timeout), Err(RequestError::Timeout)]);
        let (mut runtime, mut store) = (runtime(), Store::default());
        for seconds in [0, 1, 2, 30, 31] {
            runtime
                .step(&config(), &mut network, &mut store, || {
                    Duration::from_secs(seconds)
                })
                .unwrap();
            let count = network
                .calls
                .iter()
                .filter(|&&call| call == "login")
                .count();
            assert_eq!(
                count,
                if seconds == 0 {
                    0
                } else if seconds < 31 {
                    1
                } else {
                    2
                }
            );
        }
        assert_eq!(
            network
                .calls
                .iter()
                .filter(|&&call| call == "probe")
                .count(),
            5
        );
    }

    #[test]
    fn unified_timeout_does_not_suspend_probes_and_retries_after_delay() {
        let mut network = network(vec![Reachability::Online; 4]);
        network.verifications.extend([
            Err(RequestError::Timeout),
            Ok(CredentialVerification::Valid),
        ]);
        let (mut runtime, mut store) = (runtime(), Store::default());
        for seconds in [0, 1, 29, 30] {
            runtime
                .step(&config(), &mut network, &mut store, || {
                    Duration::from_secs(seconds)
                })
                .unwrap();
            assert_eq!(store.saved, usize::from(seconds == 30));
        }
        assert_eq!(
            network.calls,
            ["probe", "verify", "probe", "probe", "probe", "verify"]
        );
    }

    #[test]
    fn background_rejection_exits_from_either_authenticator_without_save_or_input() {
        for portal in [true, false] {
            let mut network = network(if portal {
                vec![Reachability::Offline; 2]
            } else {
                vec![Reachability::Online]
            });
            network
                .logins
                .push_back(Ok(PortalLoginOutcome::CredentialsRejected));
            network
                .verifications
                .push_back(Ok(CredentialVerification::Invalid));
            let (mut runtime, mut store) = (runtime(), Store::default());
            if portal {
                runtime
                    .step(&config(), &mut network, &mut store, || Duration::ZERO)
                    .unwrap();
            }
            let cycle = runtime
                .step(&config(), &mut network, &mut store, || {
                    Duration::from_secs(1)
                })
                .unwrap();
            assert_eq!(cycle.next, Next::ExitCredentialsRejected);
            assert_eq!(store.saved, 0);
        }
    }

    #[test]
    fn interactive_replacement_requires_new_validation_and_save_failure_propagates() {
        let mut network = network(vec![Reachability::Online; 2]);
        network.verifications.extend([
            Ok(CredentialVerification::Invalid),
            Ok(CredentialVerification::Valid),
        ]);
        let mut runtime = Runtime::new(Duration::from_secs(30), false, true);
        let mut store = Store {
            fail: true,
            ..Default::default()
        };
        let mut config = config();
        assert_eq!(
            runtime
                .step(&config, &mut network, &mut store, || Duration::ZERO)
                .unwrap()
                .next,
            Next::RequestCredentials
        );
        config.update_credentials(Credentials {
            username: "corrected".into(),
            password: "new".into(),
        });
        runtime.credentials_changed();
        assert!(
            runtime
                .step(&config, &mut network, &mut store, || Duration::ZERO)
                .is_err()
        );
        assert_eq!(store.saved, 0);
    }
}
