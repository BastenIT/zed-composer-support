use composer_language_server::run;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    run().await;
}
