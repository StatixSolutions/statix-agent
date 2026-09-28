//! The agent's WebSocket transport: wire types (`protocol`), the
//! connect/auth/reconnect session loop (`session`), and turning an incoming
//! message into a log line plus whatever reply it needs (`dispatch`).
//!
//! See `plans/controlplane-agent-communicationsystem.md` §9 for the target
//! module layout this is the first slice of, and §3 for the v2 envelope this
//! is expected to grow into once `apps/node-controller`'s gateway actually
//! terminates connections.

pub mod dispatch;
pub mod intent;
pub mod protocol;
pub mod session;
