use crate::Error;
use anyhow::anyhow;
use std::fmt;
use std::io::{self, Stderr, Write};

pub(crate) struct Log {
    page: usize,
    stderr: Stderr,
}

impl Log {
    pub fn new() -> Self {
        Log {
            page: 0,
            stderr: io::stderr(),
        }
    }

    pub fn tick(&mut self) {
        let _ = write!(self.stderr, ".");
        let _ = self.stderr.flush();
        self.page += 1;
    }

    pub fn note(&mut self, msg: &str) {
        let _ = write!(self.stderr, "[{}]", msg);
        let _ = self.stderr.flush();
        self.page += 1;
    }

    pub fn error(&mut self, err: Error) {
        let prefix = match err {
            Error::GitHub(_) => "", // already starts with "Error"
            _ => "Error: ",
        };
        writeln!(self, "{}{:?}", prefix, anyhow!(err));
    }

    pub fn write_fmt(&mut self, args: fmt::Arguments) {
        if self.page > 0 {
            let _ = writeln!(self.stderr);
            self.page = 0;
        }
        let _ = self.stderr.write_fmt(args);
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        if self.page > 0 {
            let _ = writeln!(self.stderr);
        }
    }
}
