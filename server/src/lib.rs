mod backend;
mod composer;

use tower_lsp_server::{LspService, Server};

pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run() {
    let (service, socket) = LspService::new(backend::Backend::new);
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}
