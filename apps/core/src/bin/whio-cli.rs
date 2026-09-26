#[tokio::main]
async fn main() -> Result<(), whio_core::app::cli::CliError> {
    whio_core::app::cli::run().await
}
