//! `flowproof` — record a flow with the AI agent, replay it deterministically,
//! or heal a broken trace with a reviewable diff.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "flowproof", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record a flow from a YAML spec: the agent performs it once and writes a trace.
    Record {
        /// Path to the YAML flow spec.
        spec: PathBuf,
        /// Output trace file (JSON-lines).
        #[arg(short, long, default_value = "flow.trace.jsonl")]
        out: PathBuf,
    },
    /// Deterministically replay a recorded trace (zero LLM calls).
    Run {
        /// Path to the trace file.
        trace: PathBuf,
    },
    /// Propose a reviewable fix for a trace that no longer replays.
    Heal {
        /// Path to the trace file.
        trace: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Record { spec, out } => {
            anyhow::bail!(
                "`flowproof record` is not implemented yet (spec: {}, out: {})",
                spec.display(),
                out.display()
            );
        }
        Command::Run { trace } => {
            anyhow::bail!(
                "`flowproof run` is not implemented yet (trace: {})",
                trace.display()
            );
        }
        Command::Heal { trace } => {
            anyhow::bail!(
                "`flowproof heal` is not implemented yet (trace: {})",
                trace.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_record_subcommand() {
        let cli = Cli::parse_from(["flowproof", "record", "flow.yaml", "--out", "t.jsonl"]);
        match cli.command {
            Command::Record { spec, out } => {
                assert_eq!(spec, PathBuf::from("flow.yaml"));
                assert_eq!(out, PathBuf::from("t.jsonl"));
            }
            _ => panic!("expected record subcommand"),
        }
    }
}
