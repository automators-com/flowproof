//! A host-terminal screen replica for the vision evals in `evals/terminal-grid`.
//!
//!   flowproof-terminal-replica <layout.yaml> [--state <out.json>]
//!                              [--jpeg <quality>] [--scale <fraction>] [--bits <16|8>]
//!
//! It draws a layout's character grid with GDI, takes keys the way a host
//! terminal does, and writes what is on screen to `--state` after every
//! change, which is what an eval scores against.

#![cfg_attr(not(windows), allow(dead_code))]

mod degrade;
mod screen;
#[cfg(windows)]
mod window;

use std::path::PathBuf;
use std::process::ExitCode;

pub struct Options {
    pub state: Option<PathBuf>,
    pub degrade: degrade::Degrade,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flowproof-terminal-replica: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut layout = None;
    let mut opts = Options {
        state: None,
        degrade: degrade::Degrade::default(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--state" => opts.state = Some(PathBuf::from(value()?)),
            "--jpeg" => opts.degrade.jpeg = Some(number(&value()?)?),
            "--scale" => opts.degrade.scale = Some(number(&value()?)?),
            "--bits" => opts.degrade.bits = Some(number(&value()?)?),
            _ if layout.is_none() && !arg.starts_with('-') => layout = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    let path = layout.ok_or("usage: flowproof-terminal-replica <layout.yaml> [options]")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let layout: screen::Layout =
        serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let screen = screen::Screen::new(layout)?;
    #[cfg(windows)]
    return window::run(screen, opts);
    #[cfg(not(windows))]
    {
        let _ = (screen, opts);
        Err("the replica draws with GDI, so it runs on Windows only".into())
    }
}

fn number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{text} is not a valid number here"))
}
