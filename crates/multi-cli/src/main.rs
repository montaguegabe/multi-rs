//! `multi` command-line interface.

mod cli;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use multi_core::error::Error;

fn main() -> ExitCode {
    let args = cli::Cli::parse();
    multi_core::logging::init(args.verbose);

    match cli::run(&args) {
        Ok(()) => {}
        Err(Error::Usage(message)) => {
            eprintln!("Error: {message}");
            return ExitCode::from(2);
        }
        Err(err) => {
            log::error!("{err}");
            return ExitCode::from(1);
        }
    }

    // After commands, check that all sub-repos are on their expected branch.
    // Some commands (like doctor) intentionally run outside a workspace.
    if args.command.runs_post_branch_check() {
        if let Err(err) = cli::post_command_branch_check(args.install_set()) {
            let message = err.to_string();
            if std::io::stderr().is_terminal() {
                eprintln!("\x1b[31m{message}\x1b[0m");
            } else {
                eprintln!("{message}");
            }
        }
    }
    ExitCode::SUCCESS
}
