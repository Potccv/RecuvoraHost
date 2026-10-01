use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    recuvora_host::boot::run_cli().await
}
