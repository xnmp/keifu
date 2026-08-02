use std::time::{Duration, Instant};

use keifu::network::{
    CancellationReason, NetworkManager, NetworkOperation, NetworkPhase, NetworkProgress,
    NetworkStatus, NETWORK_INACTIVITY_TIMEOUT,
};

#[test]
fn measurable_progress_refreshes_the_sixty_second_inactivity_window() {
    let started = Instant::now();
    let mut network = NetworkManager::active_for_test(NetworkOperation::Fetch, started);

    assert!(!network.check_inactivity_at(started + Duration::from_secs(59)));
    network.record_progress_at(
        NetworkProgress {
            bytes: 1_024,
            objects: 2,
            refs: 1,
        },
        started + Duration::from_secs(59),
    );

    assert!(!network.check_inactivity_at(
        started + Duration::from_secs(59) + NETWORK_INACTIVITY_TIMEOUT - Duration::from_millis(1)
    ));
    assert!(
        network.check_inactivity_at(started + Duration::from_secs(59) + NETWORK_INACTIVITY_TIMEOUT)
    );
    assert_eq!(
        network.status(),
        Some(NetworkStatus {
            operation: NetworkOperation::Fetch,
            phase: NetworkPhase::Cancelling(CancellationReason::InactivityTimeout),
        })
    );
    assert!(
        network.is_busy(),
        "cancelling remains busy until the worker exits"
    );
}

#[test]
fn repeated_progress_snapshot_does_not_hide_a_stalled_operation() {
    let started = Instant::now();
    let mut network = NetworkManager::active_for_test(NetworkOperation::Pull, started);
    let snapshot = NetworkProgress {
        bytes: 512,
        objects: 1,
        refs: 0,
    };

    network.record_progress_at(snapshot, started + Duration::from_secs(20));
    network.record_progress_at(snapshot, started + Duration::from_secs(70));

    assert!(network.check_inactivity_at(started + Duration::from_secs(80)));
    assert_eq!(
        network.status().unwrap().phase,
        NetworkPhase::Cancelling(CancellationReason::InactivityTimeout)
    );
}

#[test]
fn manual_cancellation_transitions_each_network_operation_once() {
    for operation in [
        NetworkOperation::Fetch,
        NetworkOperation::Pull,
        NetworkOperation::Push,
    ] {
        let mut network = NetworkManager::active_for_test(operation, Instant::now());

        assert!(network.cancel_active(CancellationReason::User));
        assert!(!network.cancel_active(CancellationReason::User));
        assert_eq!(
            network.status(),
            Some(NetworkStatus {
                operation,
                phase: NetworkPhase::Cancelling(CancellationReason::User),
            })
        );
    }
}
