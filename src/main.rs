fn main() {
    if let Err(error) = orbi::run() {
        eprintln!("orbi: {error:#}");
        std::process::exit(1);
    }
}
