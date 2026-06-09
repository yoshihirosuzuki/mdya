# Fonts for `xtask/generate-test-pdfs`

The xtask embeds **Noto Sans CJK JP Regular** to render the Japanese
fixture PDF used by `tests/smoke_ingest.rs`. The
font file (~16 MB) is `.gitignore`d to keep the mdya repository small —
download it before running the xtask:

```sh
curl -L -o xtask/generate-test-pdfs/fonts/NotoSansCJKjp-Regular.otf \
  https://github.com/notofonts/noto-cjk/raw/main/Sans/OTF/Japanese/NotoSansCJKjp-Regular.otf
```

The fixture PDFs themselves (`tests/fixtures/pdfs/*.pdf`) **are** committed,
so a fresh clone can run `cargo test` without ever touching this directory.

## License

Noto Sans JP is licensed under the
[SIL Open Font License v1.1](https://scripts.sil.org/OFL), compatible with
mdya's `MIT OR Apache-2.0` dual licence. Embedding the font in a generated
PDF (`subset_fonts: true` in `PdfSaveOptions`) is permitted; the embedded
subset inherits the same OFL terms.
