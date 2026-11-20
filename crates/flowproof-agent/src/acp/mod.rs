//! The user's own coding agent as the model: Claude Code or Codex, spoken to
//! over the Agent Client Protocol (ACP, agentclientprotocol.com) on its
//! stdin/stdout.
//!
//! This is how a Claude or ChatGPT subscription will drive flowproof without
//! an API key: flowproof starts the agent's ACP adapter, the adapter runs the
//! agent under the login the user already has, and flowproof never holds a
//! credential. The subset of ACP spoken here is small enough to write by
//! hand, which keeps the engine synchronous: the official SDK needs tokio.

pub mod wire;
