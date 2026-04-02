use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn main() {
    let interrupted = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted))
        .expect("register SIGINT handler");
    if let Err(err) = headless::app::run_from_env(interrupted) {
        eprintln!("{err}");
        std::process::exit(err.exit_code().code());
    }
}
