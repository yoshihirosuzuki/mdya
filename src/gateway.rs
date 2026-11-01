//! The component that talks to the user: the command line, the MCP server,
//! stdout, and environment variables.
//!
//! `print!` and `println!` are allowed anywhere in this module.
//! `std::io::stdout` and the environment variable functions stay disallowed
//! here as well (clippy.toml); allow `clippy::disallowed_methods` on each
//! function that needs one of them.

#![allow(clippy::print_stdout)]
