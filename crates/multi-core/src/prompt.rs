//! Minimal interactive prompts on stdin/stdout.

use std::io::{BufRead, Write};

use crate::error::{Error, Result};

/// Ask a free-text question; returns the trimmed answer (possibly empty).
pub fn prompt(question: &str) -> Result<String> {
    let mut stdout = std::io::stdout();
    write!(stdout, "{question}: ")?;
    stdout.flush()?;
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line)?;
    if read == 0 {
        return Err(Error::Usage("Aborted: no input available.".to_string()));
    }
    Ok(line.trim().to_string())
}

/// Yes/no confirmation. `default` is used for an empty answer.
pub fn confirm(question: &str, default: bool) -> Result<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        let answer = prompt(&format!("{question} {hint}"))?.to_lowercase();
        match answer.as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Error: invalid input"),
        }
    }
}
