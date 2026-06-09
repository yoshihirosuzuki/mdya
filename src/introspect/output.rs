//! Render introspection reports for `mdya collection list` and `mdya
//! status` in the four `--format` shapes, mirroring `search::output`.
//! `json` is the machine-faithful shared shape the MCP tools mirror;
//! `human` / `md` / `xml` are CLI presentations.

use std::io::{self, Write};

use crate::introspect::{CollectionListReport, StatusReport};
use crate::search::output::xml_escape;

const NO_DESCRIPTION_HUMAN: &str = "(none)";

pub fn print_collections_human<W: Write>(
    writer: &mut W,
    report: &CollectionListReport,
) -> io::Result<()> {
    for c in &report.collections {
        let description = c.description.as_deref().unwrap_or(NO_DESCRIPTION_HUMAN);
        writeln!(
            writer,
            "{}\t{}\t{} docs\t{description}",
            c.name, c.path, c.document_count
        )?;
    }
    writeln!(writer, "{} collections", report.collections.len())
}

/// Compact JSON + trailing newline, matching `search::output::print_json`.
pub fn print_collections_json<W: Write>(
    writer: &mut W,
    report: &CollectionListReport,
) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, report)?;
    writeln!(writer)
}

pub fn print_collections_md<W: Write>(
    writer: &mut W,
    report: &CollectionListReport,
) -> io::Result<()> {
    writeln!(writer, "# Collections")?;
    writeln!(writer)?;
    writeln!(writer, "- {} collections", report.collections.len())?;
    for c in &report.collections {
        let description = c.description.as_deref().unwrap_or(NO_DESCRIPTION_HUMAN);
        writeln!(writer)?;
        writeln!(writer, "## {}", c.name)?;
        writeln!(writer)?;
        writeln!(writer, "- path: `{}`", c.path)?;
        writeln!(writer, "- document_count: {}", c.document_count)?;
        writeln!(writer, "- description: {description}")?;
    }
    Ok(())
}

pub fn print_collections_xml<W: Write>(
    writer: &mut W,
    report: &CollectionListReport,
) -> io::Result<()> {
    writeln!(writer, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    if report.collections.is_empty() {
        return writeln!(writer, "<collections/>");
    }
    writeln!(writer, "<collections>")?;
    for c in &report.collections {
        writeln!(writer, "  <collection>")?;
        writeln!(writer, "    <name>{}</name>", xml_escape(&c.name))?;
        writeln!(writer, "    <path>{}</path>", xml_escape(&c.path))?;
        write_optional_element(writer, "description", c.description.as_deref())?;
        writeln!(
            writer,
            "    <document_count>{}</document_count>",
            c.document_count
        )?;
        writeln!(writer, "  </collection>")?;
    }
    writeln!(writer, "</collections>")
}

pub fn print_status_human<W: Write>(writer: &mut W, report: &StatusReport) -> io::Result<()> {
    writeln!(writer, "version:         {}", report.version)?;
    writeln!(writer, "embedding_model: {}", report.embedding_model)?;
    writeln!(writer, "vector_dim:      {}", report.vector_dim)?;
    writeln!(writer, "collections:     {}", report.collections)?;
    writeln!(writer, "chunks:          {}", report.chunks)?;
    writeln!(writer, "sources:         {}", report.sources)
}

pub fn print_status_json<W: Write>(writer: &mut W, report: &StatusReport) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, report)?;
    writeln!(writer)
}

pub fn print_status_md<W: Write>(writer: &mut W, report: &StatusReport) -> io::Result<()> {
    writeln!(writer, "# Status")?;
    writeln!(writer)?;
    writeln!(writer, "- version: {}", report.version)?;
    writeln!(writer, "- embedding_model: `{}`", report.embedding_model)?;
    writeln!(writer, "- vector_dim: {}", report.vector_dim)?;
    writeln!(writer, "- collections: {}", report.collections)?;
    writeln!(writer, "- chunks: {}", report.chunks)?;
    writeln!(writer, "- sources: {}", report.sources)
}

pub fn print_status_xml<W: Write>(writer: &mut W, report: &StatusReport) -> io::Result<()> {
    writeln!(writer, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(writer, "<status>")?;
    writeln!(
        writer,
        "  <version>{}</version>",
        xml_escape(&report.version)
    )?;
    writeln!(
        writer,
        "  <embedding_model>{}</embedding_model>",
        xml_escape(&report.embedding_model)
    )?;
    writeln!(writer, "  <vector_dim>{}</vector_dim>", report.vector_dim)?;
    writeln!(
        writer,
        "  <collections>{}</collections>",
        report.collections
    )?;
    writeln!(writer, "  <chunks>{}</chunks>", report.chunks)?;
    writeln!(writer, "  <sources>{}</sources>", report.sources)?;
    writeln!(writer, "</status>")
}

/// Emit `<tag>escaped</tag>`, or a self-closing `<tag/>` when the value is
/// absent, so the element is always present in the XML mirror.
fn write_optional_element<W: Write>(
    writer: &mut W,
    tag: &'static str,
    value: Option<&str>,
) -> io::Result<()> {
    match value {
        Some(v) => writeln!(writer, "    <{tag}>{}</{tag}>", xml_escape(v)),
        None => writeln!(writer, "    <{tag}/>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspect::CollectionInfo;

    fn render<F>(render: F) -> String
    where
        F: FnOnce(&mut Vec<u8>) -> io::Result<()>,
    {
        let mut buf = Vec::new();
        render(&mut buf).expect("render");
        String::from_utf8(buf).expect("utf8")
    }

    fn two_collections() -> CollectionListReport {
        CollectionListReport {
            collections: vec![
                CollectionInfo {
                    name: "notes".to_string(),
                    path: "~/notes".to_string(),
                    description: Some("個人メモ".to_string()),
                    document_count: 42,
                },
                CollectionInfo {
                    name: "docs".to_string(),
                    path: "/abs/docs".to_string(),
                    description: None,
                    document_count: 0,
                },
            ],
        }
    }

    fn sample_status() -> StatusReport {
        StatusReport {
            version: "0.3.0".to_string(),
            embedding_model: "cl-nagoya/ruri-v3-30m".to_string(),
            vector_dim: 256,
            collections: 2,
            chunks: 12,
            sources: 4,
        }
    }

    #[test]
    fn collections_json_emits_null_description_and_trailing_newline() {
        let out = render(|w| print_collections_json(w, &two_collections()));
        assert!(out.ends_with('\n'), "missing trailing newline: {out:?}");
        let value: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(
            value["collections"][1]["description"],
            serde_json::Value::Null
        );
        assert_eq!(value["collections"][0]["document_count"], 42);
    }

    #[test]
    fn collections_xml_uses_self_closing_tag_for_absent_description() {
        let out = render(|w| print_collections_xml(w, &two_collections()));
        assert!(out.contains("<description/>"), "got: {out}");
        assert!(out.contains("<name>notes</name>"), "got: {out}");
    }

    #[test]
    fn empty_collections_xml_self_closes_the_root() {
        let report = CollectionListReport {
            collections: vec![],
        };
        let out = render(|w| print_collections_xml(w, &report));
        assert!(out.contains("<collections/>"), "got: {out}");
    }

    #[test]
    fn status_json_emits_six_fields_and_trailing_newline() {
        let out = render(|w| print_status_json(w, &sample_status()));
        assert!(out.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(value["vector_dim"], 256);
        assert_eq!(value["sources"], 4);
    }

    #[test]
    fn collections_human_emits_tab_fields_and_a_count_footer() {
        let out = render(|w| print_collections_human(w, &two_collections()));
        let lines: Vec<&str> = out.lines().collect();
        // One row per collection, then a summary footer.
        assert_eq!(lines[0], "notes\t~/notes\t42 docs\t個人メモ");
        // Absent description falls back to the placeholder, not an empty cell.
        assert_eq!(lines[1], "docs\t/abs/docs\t0 docs\t(none)");
        assert_eq!(lines[2], "2 collections");
    }

    #[test]
    fn collections_md_renders_a_section_per_collection() {
        let out = render(|w| print_collections_md(w, &two_collections()));
        assert!(out.starts_with("# Collections\n"), "got: {out}");
        assert!(out.contains("## notes"), "got: {out}");
        assert!(out.contains("- document_count: 42"), "got: {out}");
        assert!(out.contains("- description: (none)"), "got: {out}");
    }

    #[test]
    fn status_human_lists_all_six_fields() {
        let out = render(|w| print_status_human(w, &sample_status()));
        assert!(out.contains("version:         0.3.0"), "got: {out}");
        assert!(out.contains("vector_dim:      256"), "got: {out}");
        assert!(out.contains("sources:         4"), "got: {out}");
    }

    #[test]
    fn status_md_lists_all_six_fields() {
        let out = render(|w| print_status_md(w, &sample_status()));
        assert!(out.starts_with("# Status\n"), "got: {out}");
        assert!(out.contains("- version: 0.3.0"), "got: {out}");
        assert!(out.contains("- chunks: 12"), "got: {out}");
    }

    #[test]
    fn status_xml_mirrors_the_six_fields() {
        let out = render(|w| print_status_xml(w, &sample_status()));
        assert!(out.contains("<vector_dim>256</vector_dim>"), "got: {out}");
        assert!(out.contains("<sources>4</sources>"), "got: {out}");
        assert!(
            out.contains("<embedding_model>cl-nagoya/ruri-v3-30m</embedding_model>"),
            "got: {out}"
        );
    }
}
