//! Pure admission regressions exercise production policy without native objects or GUI access.

use super::*;
use crate::application::termination::{
    ApplicationTermination, TerminationReadiness, TerminationRequest,
};

/// Extract an actual scalar coordinator ticket without fabricating its private numeric identity.
fn prepare(coordinator: &mut ApplicationTermination) -> TerminationTicket {
    // An active fake foreground requires the same accepted preparation boundary as native close.
    let TerminationRequest::Prepare(ticket) = coordinator.request(TerminationReadiness::Active)
    else {
        // Failing to enter Preparing invalidates the test setup rather than authorizing a guessed ticket.
        panic!("coordinator did not enter preparation");
    };
    ticket
}

/// Both built-in intents preserve their first direction across idempotent repeats and authorized promotion.
#[test]
fn admitted_direction_and_scope_only_promote_under_actual_quit() {
    for intent in [
        SurfaceCloseIntent::DismissAndResume,
        SurfaceCloseIntent::ExitToLauncher,
    ] {
        // Each direction independently exercises the actual production admission function.
        let ordinary = Some((intent, PluginCloseScope::ForegroundTransition));
        let terminal = Some((intent, PluginCloseScope::ApplicationTermination));
        assert!(
            validate_close_scope(
                ordinary,
                intent,
                PluginCloseScope::ForegroundTransition,
                false
            )
            .is_ok()
        );
        assert!(
            validate_close_scope(
                ordinary,
                intent,
                PluginCloseScope::ApplicationTermination,
                false
            )
            .is_err()
        );
        assert!(
            validate_close_scope(
                ordinary,
                intent,
                PluginCloseScope::ApplicationTermination,
                true
            )
            .is_ok()
        );
        assert!(
            validate_close_scope(
                terminal,
                intent,
                PluginCloseScope::ApplicationTermination,
                true
            )
            .is_ok()
        );
        assert!(
            validate_close_scope(
                terminal,
                intent,
                PluginCloseScope::ForegroundTransition,
                true
            )
            .is_err()
        );
        let opposite = match intent {
            SurfaceCloseIntent::DismissAndResume => {
                // Back to Launcher cannot overwrite a dismissal already accepted by this session.
                SurfaceCloseIntent::ExitToLauncher
            }
            SurfaceCloseIntent::ExitToLauncher => {
                // Dismissal cannot erase a previously accepted Launcher successor during quit.
                SurfaceCloseIntent::DismissAndResume
            }
        };
        assert!(
            validate_close_scope(
                ordinary,
                opposite,
                PluginCloseScope::ForegroundTransition,
                false
            )
            .is_err()
        );
        assert!(
            validate_close_scope(
                ordinary,
                opposite,
                PluginCloseScope::ApplicationTermination,
                true
            )
            .is_err()
        );
    }
}

/// A queued activation gate or missing quit ticket never grants terminal request authority.
#[test]
fn fresh_terminal_request_requires_real_coordinator_identity() {
    // No accepted request exists, so initial terminal admission still checks actual pending quit explicitly.
    assert!(
        validate_close_scope(
            None,
            SurfaceCloseIntent::DismissAndResume,
            PluginCloseScope::ApplicationTermination,
            false
        )
        .is_err()
    );
    assert!(validate_close_ticket(None, PluginCloseScope::ApplicationTermination, None).is_err());
    let mut coordinator = ApplicationTermination::new();
    let ticket = prepare(&mut coordinator);
    assert_eq!(
        validate_close_ticket(None, PluginCloseScope::ApplicationTermination, Some(ticket)),
        Ok(Some(ticket))
    );
    assert_eq!(
        validate_close_ticket(None, PluginCloseScope::ForegroundTransition, Some(ticket)),
        Ok(None)
    );
}

/// An earlier cancelled quit's receipt cannot be consumed when another real quit later becomes pending.
#[test]
fn terminal_receipt_remains_bound_to_its_original_quit_ticket() {
    // Obtain distinct identities from one actual coordinator, including its explicit cancellation boundary.
    let mut coordinator = ApplicationTermination::new();
    let old = prepare(&mut coordinator);
    assert_eq!(
        coordinator.prepared(old, false, TerminationReadiness::Active),
        TerminationRequest::Cancel
    );
    let current = prepare(&mut coordinator);
    assert_ne!(old, current);
    assert!(
        validate_close_ticket(
            Some(old),
            PluginCloseScope::ApplicationTermination,
            Some(current)
        )
        .is_err()
    );
    assert!(
        validate_close_ticket(Some(old), PluginCloseScope::ApplicationTermination, None).is_err()
    );
    assert_eq!(
        validate_close_ticket(
            Some(current),
            PluginCloseScope::ApplicationTermination,
            Some(current)
        ),
        Ok(Some(current))
    );
}
