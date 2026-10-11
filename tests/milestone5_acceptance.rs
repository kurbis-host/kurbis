//! Milestone 5 acceptance/regression coverage for the complete CI preview path.
//!
//! The test uses a disposable PostgreSQL database and deterministic source,
//! remote-builder, object-store, and worker-runtime fixtures.  The fixtures
//! intentionally preserve the same boundaries as production:
//!
//! * source observation creates one immutable PR build key;
//! * build work is leased/coalesced in PostgreSQL;
//! * a remote-build receipt is checked against the artifact digest;
//! * runtime generations and capabilities are fenced by PostgreSQL; and
//! * expiry and failed starts must roll back rather than leaving a live route.
//!
//! Real Linux/Dokploy execution is deliberately not faked here.  The opt-in
//! hook and its required environment are documented in
//! `verification/milestone5/README.md` and exercised by
//! `tools/run_milestone5_acceptance.sh`.

#[path = "milestone5_acceptance/harness.rs"]
mod harness;
#[path = "milestone5_acceptance/migration.rs"]
mod migration;
