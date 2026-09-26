#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    whio_core::app::run().await
}
