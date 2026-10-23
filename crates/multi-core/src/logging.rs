//! Emoji-prefixed logger writing to stderr, matching the original CLI output.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct EmojiLogger;

impl Log for EmojiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let prefix = match record.level() {
            Level::Warn => "⚠️  ",
            Level::Error => "❌ ",
            _ => "",
        };
        let stderr = std::io::stderr();
        let mut handle = stderr.lock();
        let _ = writeln!(handle, "{prefix}{}", record.args());
    }

    fn flush(&self) {}
}

static LOGGER: EmojiLogger = EmojiLogger;

/// Install the logger. Safe to call more than once; later calls only adjust
/// the verbosity.
pub fn init(verbose: bool) {
    let level = if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}
