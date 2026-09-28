//! Protocol v2 client: the envelope and resource model (`protocol`), the
//! connect/handshake/reconcile session loop (`session`), per-message
//! handling (`dispatch`), and the pure "verify current against target" step
//! (`reconcile`).
//!
//! See `plans/controlplane-agent-communicationsystem.md` §3 for the wire
//! protocol this implements, and
//! `apps/node-controller/node_modules/@statix/node-controller-contract` for
//! the canonical schema it's ported from. Opt-in only via `--protocol v2`
//! (see `src/main.rs`'s `Protocol` enum) — production routing still sends
//! v1 traffic to `apps/api`, not `apps/node-controller`.

pub mod dispatch;
pub mod protocol;
pub mod reconcile;
pub mod session;
