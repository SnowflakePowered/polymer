mod keygen;
mod pack;
mod record;
mod sign;
mod verify;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Packs, signs, and checks Polymer plugins for distribution.
#[derive(Parser)]
#[command(name = "polymer-pack", version, about)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates a minisign key pair for signing releases.
    Keygen(keygen::Keygen),
    /// Packs a plugin listed in Polymer.toml into an asar archive.
    Pack(pack::Pack),
    /// Appends a signature to a packed archive.
    Sign(sign::Sign),
    /// Records a signed archive as a release in Polymer.toml.
    Record(record::Record),
    /// Checks Polymer.toml and, optionally, every release it lists.
    Verify(verify::Verify),
}

fn main() -> ExitCode {
    let result = match Arguments::parse().command {
        Command::Keygen(command) => command.run(),
        Command::Pack(command) => command.run(),
        Command::Sign(command) => command.run(),
        Command::Record(command) => command.run(),
        Command::Verify(command) => command.run(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("polymer-pack: {error}");
            ExitCode::FAILURE
        }
    }
}
