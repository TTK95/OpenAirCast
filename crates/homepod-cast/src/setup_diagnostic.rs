//! Explicit two-receiver setup-only diagnostic.

use airplay_client::{AirPlayClient, SetupPhase};
use airplay_core::features::AuthMethod;
use anyhow::{anyhow, bail};
use std::time::Duration;

const DISCOVERY_DEADLINE: Duration = Duration::from_secs(3);
const SETUP_DEADLINE: Duration = Duration::from_secs(30);
const OBSERVATION_WINDOW: Duration = Duration::from_secs(3);
const DISCONNECT_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) fn requested_names(args: Vec<String>) -> Result<[String; 2], String> {
    let [first, second]: [String; 2] = args
        .try_into()
        .map_err(|_| "diagnostic requires exactly two receiver names".to_string())?;
    if first.trim().is_empty() || second.trim().is_empty() {
        return Err("receiver names must not be empty".to_string());
    }
    if first == second {
        return Err("receiver names must be distinct".to_string());
    }
    Ok([first, second])
}

fn select_indices(requested: &[String; 2], discovered: &[&str]) -> Result<[usize; 2], String> {
    let find_exact = |name: &str| {
        let matches = discovered
            .iter()
            .enumerate()
            .filter_map(|(index, discovered_name)| (*discovered_name == name).then_some(index))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => Ok(*index),
            [] => Err("requested receiver was not discovered".to_string()),
            _ => Err("requested receiver name is ambiguous".to_string()),
        }
    };

    Ok([find_exact(&requested[0])?, find_exact(&requested[1])?])
}

enum SetupObservation {
    Accepted {
        connected: usize,
        failures: usize,
        phases: Vec<SetupPhase>,
    },
    Failed,
    Cancelled,
}

impl SetupObservation {
    fn connected_count(&self) -> Option<usize> {
        match self {
            Self::Accepted { connected, .. } => Some(*connected),
            Self::Failed | Self::Cancelled => None,
        }
    }
}

fn inspection_result(
    _setup: &SetupObservation,
    _disconnect_api_returned_ok: bool,
) -> anyhow::Result<()> {
    bail!("inspection-only: per-member teardown and audio remain unverified")
}

/// Run one bounded, setup-only diagnostic against two exact discovery names.
pub async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let requested = requested_names(args).map_err(|message| anyhow!(message))?;

    println!("Two-receiver setup-only diagnostic");
    println!("This sends SETUP/RECORD, attempts TEARDOWN, sends no audio, and changes no volume.");
    println!("Run it without another AirPlay session or OpenAirCast instance.");

    let mut client =
        AirPlayClient::new().map_err(|_| anyhow!("diagnostic client initialization failed"))?;
    let devices = client
        .discover(DISCOVERY_DEADLINE)
        .await
        .map_err(|_| anyhow!("diagnostic discovery failed"))?;
    let discovered_names = devices
        .iter()
        .map(|device| device.name.as_str())
        .collect::<Vec<_>>();
    let indices =
        select_indices(&requested, &discovered_names).map_err(|message| anyhow!(message))?;
    let selected = [devices[indices[0]].clone(), devices[indices[1]].clone()];

    let password_required = selected
        .iter()
        .filter(|device| device.requires_password)
        .count();
    let non_transient = selected
        .iter()
        .filter(|device| device.features.auth_method() != AuthMethod::HomeKitTransient)
        .count();
    let eligible = selected.len()
        - selected
            .iter()
            .filter(|device| {
                device.requires_password
                    || device.features.auth_method() != AuthMethod::HomeKitTransient
            })
            .count();
    println!(
        "eligibility summary: requested={} eligible={} password_required={} non_transient={}",
        selected.len(),
        eligible,
        password_required,
        non_transient
    );
    if password_required != 0 || non_transient != 0 {
        bail!("diagnostic receiver eligibility check failed");
    }

    let setup = match tokio::time::timeout(
        SETUP_DEADLINE,
        client.connect_group_best_effort(&selected, None),
    )
    .await
    {
        Ok(Ok(report)) => {
            let phases = report
                .failures
                .iter()
                .map(|failure| failure.phase)
                .collect::<Vec<SetupPhase>>();
            let observation = SetupObservation::Accepted {
                connected: report.connected.len(),
                failures: report.failures.len(),
                phases,
            };
            tokio::time::sleep(OBSERVATION_WINDOW).await;
            observation
        }
        Ok(Err(_)) => SetupObservation::Failed,
        Err(_) => SetupObservation::Cancelled,
    };
    let connected = setup
        .connected_count()
        .map_or_else(|| "unknown".to_string(), |count| count.to_string());
    match &setup {
        SetupObservation::Accepted {
            failures, phases, ..
        } => println!(
            "setup summary: requested={} connected={connected} failures={failures} phases={phases:?} outcome=accepted",
            selected.len()
        ),
        SetupObservation::Failed => println!(
            "setup summary: requested={} connected={connected} failures=unknown phases=unknown outcome=error",
            selected.len()
        ),
        SetupObservation::Cancelled => println!(
            "setup summary: requested={} connected={connected} failures=unknown phases=unknown outcome=timeout",
            selected.len()
        ),
    }

    let disconnect_api_returned_ok =
        match tokio::time::timeout(DISCONNECT_DEADLINE, client.disconnect()).await {
            Ok(Ok(())) => {
                println!("disconnect_api=returned_ok");
                true
            }
            Ok(Err(_)) => {
                println!("disconnect_api=returned_error");
                false
            }
            Err(_) => {
                println!("disconnect_api=timeout");
                false
            }
        };

    println!("verification summary: per_member_teardown=unverified audio=unverified");
    inspection_result(&setup, disconnect_api_returned_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod requested_names {
        use super::*;

        #[test]
        fn requires_exactly_two_names() {
            assert!(requested_names(vec![]).is_err());
            assert!(requested_names(vec!["Büro".into()]).is_err());
            assert!(requested_names(vec!["Büro".into(), "Bad".into(), "Küche".into()]).is_err());
        }

        #[test]
        fn rejects_empty_or_whitespace_only_names() {
            assert!(requested_names(vec!["".into(), "Bad".into()]).is_err());
            assert!(requested_names(vec!["Büro".into(), " \t ".into()]).is_err());
        }

        #[test]
        fn rejects_duplicate_exact_names() {
            assert!(requested_names(vec!["Büro".into(), "Büro".into()]).is_err());
        }

        #[test]
        fn retains_nonempty_spelling_and_order() {
            assert_eq!(
                requested_names(vec![" Büro ".into(), "Bad".into()]),
                Ok([" Büro ".into(), "Bad".into()])
            );
        }
    }

    mod select_indices {
        use super::*;

        #[test]
        fn preserves_reversed_requested_order() {
            let names = requested_names(vec!["Büro".into(), "Bad".into()]).unwrap();
            assert_eq!(select_indices(&names, &["Bad", "Büro"]), Ok([1, 0]));
        }

        #[test]
        fn rejects_ambiguous_discovery_name() {
            let names = requested_names(vec!["Büro".into(), "Bad".into()]).unwrap();
            assert!(select_indices(&names, &["Büro", "Büro", "Bad"]).is_err());
        }

        #[test]
        fn rejects_missing_discovery_name() {
            let names = requested_names(vec!["Büro".into(), "Bad".into()]).unwrap();
            assert!(select_indices(&names, &["Bad"]).is_err());
        }
    }

    mod outcome_classification {
        use super::*;

        #[test]
        fn accepted_setup_and_ok_disconnect_remain_unverified() {
            let setup = SetupObservation::Accepted {
                connected: 2,
                failures: 0,
                phases: vec![],
            };

            assert!(inspection_result(&setup, true).is_err());
        }

        #[test]
        fn failed_setup_has_unknown_connected_membership() {
            assert_eq!(SetupObservation::Failed.connected_count(), None);
        }

        #[test]
        fn cancelled_setup_has_unknown_connected_membership() {
            assert_eq!(SetupObservation::Cancelled.connected_count(), None);
        }
    }
}
