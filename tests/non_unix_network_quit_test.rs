use std::time::Instant;

use keifu::action::Action;
use keifu::app::App;
use keifu::network::NetworkOperation;
#[cfg(not(unix))]
use keifu::network::NETWORK_CANCELLATION_SUPPORTED;

fn assert_quit_is_immediate(mut app: App, action: Action) {
    app.handle_action(action).unwrap();

    assert!(app.should_quit);
    assert!(!app.shutdown_after_network);
    assert!(app.is_network_busy());
}

#[test]
fn normal_and_forced_quit_do_not_wait_when_network_cancellation_is_rejected() {
    for action in [Action::Quit, Action::ForceQuit] {
        let mut app = App::test_fixture();
        app.network
            .activate_uncancellable_for_test(NetworkOperation::Fetch, Instant::now());

        assert_quit_is_immediate(app, action);
    }
}

#[cfg(not(unix))]
#[test]
fn force_quit_does_not_wait_for_a_non_unix_network_job() {
    assert!(!NETWORK_CANCELLATION_SUPPORTED);
    let mut app = App::test_fixture();
    app.network
        .activate_for_test(NetworkOperation::Fetch, Instant::now());

    assert_quit_is_immediate(app, Action::ForceQuit);
}
