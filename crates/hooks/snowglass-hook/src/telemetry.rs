//! Tracing setup for code loaded into target processes.

use parking_lot::{Mutex, MutexGuard};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::OnceLock,
};
use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;

static INITIALIZED: OnceLock<()> = OnceLock::new();

struct LockedWriter(Mutex<File>);

struct LockedWriterGuard<'a>(MutexGuard<'a, File>);

impl<'a> MakeWriter<'a> for LockedWriter {
    type Writer = LockedWriterGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LockedWriterGuard(self.0.lock())
    }
}

impl Write for LockedWriterGuard<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Installs a tracing subscriber that appends events to the configured hook log.
///
/// Hook crates are loaded as separate dynamic libraries, so each library owns
/// its own tracing dispatcher and initializes it on the first emitted event.
pub fn init_file_tracing(path: Option<PathBuf>) {
    INITIALIZED.get_or_init(|| {
        let builder = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(Level::TRACE);
        let file =
            path.and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
        let result = match file {
            Some(file) => builder
                .with_writer(LockedWriter(Mutex::new(file)))
                .try_init(),
            None => builder.with_writer(std::io::stderr).try_init(),
        };
        let _ = result;
    });
}
