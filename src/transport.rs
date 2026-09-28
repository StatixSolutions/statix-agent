//! The agent's WebSocket transport: v1 wire types (`protocol`), the
//! connect/auth/reconnect session loop (`session`), and turning an incoming
//! message into a log line plus whatever reply it needs (`dispatch`). `v2`
//! is the real protocol v2 client (opt-in via `--protocol v2` — see
//! `src/main.rs`), speaking the contract `apps/node-controller` now
//! implements.
//!
//! See `plans/controlplane-agent-communicationsystem.md` §9 for the target
//! module layout this is a slice of.

pub mod dispatch;
pub mod intent;
pub mod protocol;
pub mod session;
pub mod v2;
