#![forbid(unsafe_code)]

#[tokio::main]
async fn main() {
    if let Err(error) = nono_approval::cli::run_cli().await {
        eprintln!(
            "error: {}",
            nono_approval::display::sanitize(&error.to_string())
        );
        std::process::exit(1);
    }
}
