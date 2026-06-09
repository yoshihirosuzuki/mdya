//! Generate test fixture PDFs for `tests/smoke_ingest.rs`.
//!
//! Output:
//!
//! - `tests/fixtures/pdfs/english.pdf` — ASCII baseline, exercises the basic
//!   pdf-extract → chunker → search path.
//! - `tests/fixtures/pdfs/japanese.pdf` — CJK acceptance check that
//!   pdf-extract's adobe-cmap-parser handles ToUnicode / CIDFont mapping
//!   well enough for Japanese ingest. If the extracted text is garbled,
//!   swapping the extractor crate from `pdf-extract` to `unpdf` 0.7
//!   (CJK-explicit) is a one-function fallback.
//!
//! Run with `cargo run -p xtask-generate-test-pdfs` after placing
//! `NotoSansCJKjp-Regular.otf` in `xtask/generate-test-pdfs/fonts/` (see
//! `fonts/README.md` for the download URL). The generated PDFs are
//! committed so contributors do not need to rerun this xtask on a fresh
//! clone — this mirrors the `xtask/generate-tiny-bert` /
//! `xtask/generate-tiny-modernbert` pattern already in use for ML
//! fixtures.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use krilla::Document;
use krilla::geom::Point;
use krilla::page::PageSettings;
use krilla::text::{Font, TextDirection};

const NOTO_SANS_CJK_JP: &[u8] = include_bytes!("../fonts/NotoSansCJKjp-Regular.otf");

/// A4 portrait in PDF points (1 pt = 1/72 inch). 595 × 842 ≈ 210 × 297 mm.
const PAGE_WIDTH_PT: f32 = 595.0;
const PAGE_HEIGHT_PT: f32 = 842.0;

const FONT_SIZE: f32 = 12.0;
const LINE_HEIGHT: f32 = 18.0;
const LEFT_MARGIN: f32 = 60.0;
const TOP_MARGIN: f32 = 80.0;

fn main() -> Result<()> {
    let font = Font::new(Arc::new(NOTO_SANS_CJK_JP.to_vec()).into(), 0)
        .context("parse Noto Sans CJK JP font (Font::new returned None)")?;

    let out_dir = fixtures_dir()?;
    std::fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;

    write_pdf(
        out_dir.join("english.pdf"),
        &[
            "mdya PDF ingest smoke fixture",
            "",
            "This is an English-only test document used by",
            "tests/smoke_ingest.rs to exercise pdf-extract end-to-end.",
            "The text intentionally spans several short lines so the",
            "sliding-window chunker emits at least one chunk and search",
            "queries can hit a known keyword such as 'smoke fixture'.",
        ],
        &font,
    )?;

    write_pdf(
        out_dir.join("japanese.pdf"),
        &[
            "mdya PDF 取り込み日本語スモーク用フィクスチャ",
            "",
            "tests/smoke_ingest.rs から呼ばれます。",
            "pdf-extract が CMap / ToUnicode を",
            "正しく解釈できるかを確認するためのドキュメントです。",
            "短い段落を複数置いて chunker の挙動を観察します。",
            "検索キーワード: フィクスチャ / スモーク",
        ],
        &font,
    )?;

    Ok(())
}

/// Resolve `tests/fixtures/pdfs/` relative to the workspace root. `cargo
/// run -p xtask-generate-test-pdfs` sets `CARGO_MANIFEST_DIR` to the
/// xtask crate's directory (`<workspace>/xtask/generate-test-pdfs`), so
/// the workspace root is two parents up.
fn fixtures_dir() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .context("locate workspace root from CARGO_MANIFEST_DIR")?;
    Ok(workspace.join("tests").join("fixtures").join("pdfs"))
}

fn write_pdf(path: PathBuf, lines: &[&str], font: &Font) -> Result<()> {
    let mut document = Document::new();
    let mut page = document.start_page_with(
        PageSettings::from_wh(PAGE_WIDTH_PT, PAGE_HEIGHT_PT)
            .context("create page settings (from_wh returned None)")?,
    );
    let mut surface = page.surface();
    // Empty lines are skipped for the `draw_text` call but the `i`
    // index is intentionally still advanced via `enumerate()`, so an
    // empty entry in `lines` acts as a one-line gap of vertical
    // spacing between paragraphs.
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let y = TOP_MARGIN + (i as f32) * LINE_HEIGHT;
        surface.draw_text(
            Point::from_xy(LEFT_MARGIN, y),
            font.clone(),
            FONT_SIZE,
            line,
            false,
            TextDirection::Auto,
        );
    }
    surface.finish();
    page.finish();

    let pdf = document
        .finish()
        .map_err(|e| anyhow::anyhow!("finish PDF document: {e:?}"))?;
    std::fs::write(&path, &pdf).with_context(|| format!("write {}", path.display()))?;
    println!("wrote {} ({} bytes)", path.display(), pdf.len());
    Ok(())
}
