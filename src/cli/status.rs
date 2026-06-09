//! `mdya status` implementation: gather index status via
//! [`introspect::status`] and render it in the requested `--format`.

use std::io;
use std::path::Path;

use crate::cli::OutputFormat;
use crate::introspect::{self, output};

pub async fn run(config_dir_flag: Option<&Path>, format: OutputFormat) -> anyhow::Result<()> {
    let report = introspect::status(config_dir_flag).await?;
    let mut stdout = io::stdout().lock();
    match format {
        OutputFormat::Human => output::print_status_human(&mut stdout, &report)?,
        OutputFormat::Json => output::print_status_json(&mut stdout, &report)?,
        OutputFormat::Md => output::print_status_md(&mut stdout, &report)?,
        OutputFormat::Xml => output::print_status_xml(&mut stdout, &report)?,
    }
    Ok(())
}
