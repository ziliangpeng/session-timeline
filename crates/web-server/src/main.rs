//! thin binary: library holds all logic (see lib.rs)

#[tokio::main]
async fn main() {
    web_server::serve().await
}
