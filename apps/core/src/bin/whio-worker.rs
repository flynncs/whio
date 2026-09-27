#[tokio::main]
async fn main() {
    let Err(error) = whio_core::app::worker::run().await else {
        return;
    };

    eprintln!("whio-worker: {error}");

    let mut cause = error.source();
    while let Some(error) = cause {
        eprintln!("  caused by: {error}");
        cause = error.source();
    }

    std::process::exit(1);
}
