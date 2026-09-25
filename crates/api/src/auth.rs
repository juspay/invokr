//! Invokr's `authn_kit` integration.
//!
//! `authn_kit` deliberately knows nothing about four things, each supplied here:
//!
//! * [`principal`] — what an authenticated caller is, and how claims become one
//! * [`scope`] — whether a route requires a credential at all
//! * [`legacy`] — the pre-OIDC shared key, kept only while callers migrate
//! * [`session`] — confining the login redirect to the dashboard
//! * [`setup`] / [`routes`] — assembling the stack and rendering its endpoints
//!
//! **Authentication only.** Invokr has no authorization layer: once a caller is
//! authenticated it reaches every org and every workspace.

pub mod legacy;
pub mod principal;
pub mod routes;
pub mod scope;
pub mod session;
pub mod setup;

// Only what the binary actually names. The module types (`InvokrProfile`,
// `InvokrScopes`, `AuthRoutesState`, `InvokrAuthn`) stay reachable by path.
pub use principal::Caller;
pub use setup::build;
