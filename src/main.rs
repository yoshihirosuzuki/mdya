//! mdya binary entry point. Installs the panic hook before anything else
//! (panic hook prints a bug-report hint and exits 101), then redirects
//! Lance's FTS tokenizer to `<config_dir>/lance-models/` via
//! `LANCE_LANGUAGE_MODEL_HOME` *before* the tokio runtime starts so the
//! `env::set_var` happens single-threaded (Rust 2024 marks `set_var`
//! `unsafe` because env state is process-global), and finally hands off
//! to the async dispatch inside a manually-constructed tokio runtime.
//! `anyhow` carries the lib-layer error chain to the main return, where
//! its `Debug` impl prints the chain.

use anyhow::Result;
use clap::Parser;

use mdya::cli::{Cli, TracingOptions, init_tracing};
use mdya::store::lance_lm::{LANCE_LANGUAGE_MODEL_HOME_ENV_KEY, lance_models_dir};

fn main() -> Result<()> {
    install_panic_hook();
    let cli = Cli::parse();
    redirect_lance_language_model_home(&cli);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        init_tracing(TracingOptions {
            level_flag: cli.log_level.as_deref(),
            format: cli.log_format,
            no_color: cli.no_color,
        })
        .map_err(|e| anyhow::anyhow!("invalid log level directive: {e}"))?;
        cli.run().await
    })
}

/// Point Lance at `<config_dir>/lance-models/` for the rest of the
/// process. Best-effort by design:
///
/// - If `--config-dir` resolution fails (e.g. no `$HOME`), the subcommand
///   below will surface the same error against a richer context, so we
///   skip the env redirect rather than racing two error paths.
/// - If `create_dir_all` fails (e.g. `--config-dir` points at a read-only
///   path), we still set the env var so Lance can produce a clear
///   "Invalid directory path" error *only* when an FTS code path is
///   actually exercised. Non-Lance subcommands (`mdya version`,
///   `mdya stress`) continue to work under a non-writable `--config-dir`
///   — the integration tests in `tests/runtime_memory_guard.rs` depend
///   on that.
fn redirect_lance_language_model_home(cli: &Cli) {
    let Ok(base) = mdya::config::resolve_config_dir(cli.config_dir.as_deref()) else {
        return;
    };
    let lance_home = lance_models_dir(&base);
    let _ = std::fs::create_dir_all(&lance_home);
    // SAFETY: called before the tokio runtime starts and before any
    // other thread spawns, so the process is observably single-threaded
    // at this point and no data race on the env state is possible. This
    // is the documented safe usage pattern for `env::set_var` under the
    // Rust 2024 edition's `unsafe` reclassification.
    unsafe {
        std::env::set_var(LANCE_LANGUAGE_MODEL_HOME_ENV_KEY, &lance_home);
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        eprintln!(
            "\nthis is a bug in mdya — please report at \
             https://github.com/yoshihirosuzuki/mdya/issues"
        );
        std::process::exit(101);
    }));
}
