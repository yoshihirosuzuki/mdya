# Fonts for `xtask/generate-test-pdfs`

The xtask reads **Noto Sans CJK JP Regular** from this directory to render
the Japanese fixture PDF used by `tests/smoke_ingest.rs`. The
font file (~16 MB) is `.gitignore`d to keep the mdya repository small —
download it before running the xtask:

```sh
curl --proto "=https" --tlsv1.2 -fLsS \
  -o xtask/generate-test-pdfs/fonts/NotoSansCJKjp-Regular.otf \
  https://github.com/notofonts/noto-cjk/raw/Sans2.004/Sans/OTF/Japanese/NotoSansCJKjp-Regular.otf
```

The file is read at run time rather than compiled into the xtask, so
building, linting and testing the workspace never need it. The fixture PDFs
themselves (`tests/fixtures/pdfs/*.pdf`) **are** committed, so a fresh clone
can run `cargo test` without ever touching this directory.

## License

Noto Sans CJK JP is licensed under the
[SIL Open Font License v1.1](https://openfontlicense.org/documents/OFL.txt),
compatible with mdya's `MIT OR Apache-2.0` dual licence. Embedding a subset
of the font in a generated PDF is permitted, and doing so does not place the
document itself under the OFL — see questions 1.12 and 1.13 of the
[OFL FAQ](https://openfontlicense.org/documents/OFL-FAQ.txt).
