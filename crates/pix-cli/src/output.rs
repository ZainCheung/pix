use std::io::{self, Write};
use std::{error::Error, fmt};

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;

pub(crate) const OUTPUT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub(crate) struct CliUsageError(String);

impl CliUsageError {
    #[must_use]
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CliUsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CliUsageError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CommandOutput {
    format: OutputFormat,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T> {
    schema_version: u32,
    ok: bool,
    command: &'a str,
    data: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: u32,
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl CommandOutput {
    #[must_use]
    pub(crate) const fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    #[must_use]
    pub(crate) const fn is_json(self) -> bool {
        matches!(self.format, OutputFormat::Json)
    }

    // Consuming `self` documents that a command answers exactly once.
    #[allow(clippy::unused_self)]
    pub(crate) fn success<T: Serialize>(self, command: &str, data: &T) -> Result<()> {
        let envelope = SuccessEnvelope {
            schema_version: OUTPUT_SCHEMA_VERSION,
            ok: true,
            command,
            data,
        };
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        serde_json::to_writer(&mut lock, &envelope).context("encoding Pix JSON output")?;
        writeln!(lock).context("writing Pix JSON output")
    }

    pub(crate) fn error(code: &str, message: &str) -> Result<()> {
        let envelope = ErrorEnvelope {
            schema_version: OUTPUT_SCHEMA_VERSION,
            ok: false,
            error: ErrorBody { code, message },
        };
        let stderr = io::stderr();
        let mut lock = stderr.lock();
        serde_json::to_writer(&mut lock, &envelope).context("encoding Pix JSON error")?;
        writeln!(lock).context("writing Pix JSON error")
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorBody, ErrorEnvelope, OUTPUT_SCHEMA_VERSION, SuccessEnvelope};

    #[test]
    fn machine_envelopes_are_versioned_and_explicit() {
        let data = serde_json::json!({"devices": []});
        let success = serde_json::to_value(SuccessEnvelope {
            schema_version: OUTPUT_SCHEMA_VERSION,
            ok: true,
            command: "device.list",
            data: &data,
        })
        .expect("serialize success envelope");
        assert_eq!(success["schema_version"], 1);
        assert_eq!(success["ok"], true);
        assert_eq!(success["command"], "device.list");

        let error = serde_json::to_value(ErrorEnvelope {
            schema_version: OUTPUT_SCHEMA_VERSION,
            ok: false,
            error: ErrorBody {
                code: "usage",
                message: "device command required",
            },
        })
        .expect("serialize error envelope");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "usage");
    }
}
