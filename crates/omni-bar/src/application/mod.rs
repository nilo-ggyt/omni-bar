//! Pure application decisions: catalog actions and exclusive foreground state.
//!
//! These modules exchange static identities and owned action values. Native adapters
//! perform the effects and report their outcome after each application-state borrow ends.

pub(crate) mod foreground;
pub(crate) mod launcher;

// The termination coordinator is used by AppKit; portable unit tests also exercise its ordering.
#[cfg(any(target_os = "macos", test))]
pub(crate) mod termination;
