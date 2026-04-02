fn main() {
    if let Err(err) = headless::app::run_from_env() {
        eprintln!("{err}");
        std::process::exit(err.exit_code().code());
    }
}
