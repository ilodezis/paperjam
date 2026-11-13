//! Log output: `tracing` events to stderr.
//!
//! `RUST_LOG` picks what is shown (default `info`); `PAPERJAM_LOG_FORMAT=json`
//! switches from readable lines to one JSON object per line. Events carry ids
//! and error codes only, never document text, field values or tokens.

use anyhow::Context;
use tracing_subscriber::EnvFilter;

const FORMAT_VAR: &str = "PAPERJAM_LOG_FORMAT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

impl Format {
    /// Parses the value of `PAPERJAM_LOG_FORMAT`; unset or empty is `text`.
    pub fn parse(value: Option<&str>) -> anyhow::Result<Format> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Format::Text),
            Some(v) if v.eq_ignore_ascii_case("text") => Ok(Format::Text),
            Some(v) if v.eq_ignore_ascii_case("json") => Ok(Format::Json),
            Some(v) => anyhow::bail!("{FORMAT_VAR} must be `text` or `json`, not `{v}`"),
        }
    }
}

/// Installs the global subscriber. Call once, before the first event.
pub fn init() -> anyhow::Result<()> {
    // A `.env` file may carry the logging variables too.
    let _ = dotenvy::dotenv();
    let format = Format::parse(std::env::var(FORMAT_VAR).ok().as_deref())?;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match format {
        Format::Text => builder.try_init(),
        Format::Json => builder.json().try_init(),
    }
    .map_err(|e| anyhow::anyhow!(e))
    .context("could not set up logging")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_defaults_to_text() {
        assert_eq!(Format::parse(None).unwrap(), Format::Text);
        assert_eq!(Format::parse(Some("  ")).unwrap(), Format::Text);
    }

    #[test]
    fn format_accepts_json_in_any_case() {
        assert_eq!(Format::parse(Some("JSON")).unwrap(), Format::Json);
    }

    #[test]
    fn format_rejects_unknown_values_naming_the_variable() {
        let err = Format::parse(Some("yaml")).unwrap_err().to_string();
        assert!(err.contains("PAPERJAM_LOG_FORMAT"), "{err}");
    }
}
