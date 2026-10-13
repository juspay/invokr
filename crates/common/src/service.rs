//! Shared orchestration layer used by **both** deployment modes.
//!
//! The raw `db::*` helpers are single SQL statements; the *orchestration* above
//! them — transaction boundaries, pg_cron registration, guards, validation,
//! idempotency handling — used to be re-implemented separately in the REST
//! handlers (`crates/api`) and the library client (`crates/worker`), which let
//! the two drift (see juspay/invokr#55).
//!
//! This module is the single implementation both adapters call, so REST mode and
//! library mode cannot diverge on those semantics. Functions return [`crate::error::AppError`]
//! (already defined in this crate); the REST handler propagates it directly, the
//! library client converts it via `anyhow`.

pub mod endpoints;
pub mod jobs;
