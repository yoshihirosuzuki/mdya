fn main() -> std::process::ExitCode {
    // SAFETY: no other thread is running yet: `main` starts none before it.
    unsafe { mdya::gateway::run() }
}
