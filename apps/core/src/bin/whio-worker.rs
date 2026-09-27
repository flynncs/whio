#[tokio::main]
async fn main() {
    if let Err(error) = whio_core::app::worker::run().await {
        eprintln!("whio-worker: {error}");
        std::process::exit(1);
    }
}
