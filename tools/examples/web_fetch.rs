use std::env;
use llm_rs::tool::{CancellationToken, ToolContext};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        eprintln!("Usage: {} <url>", args[0]);
        std::process::exit(1);
    }

    let url = &args[1];
    let tool = tools::web_fetch_tool();

    let ctx = ToolContext { cancel_token: CancellationToken::new() };
    let json_params = serde_json::json!({ "url": url }).to_string();
    let mut stream = tool.execute(ctx, json_params);

    while let Some(output) = stream.next().await {
        println!("{}", output);
    }
}
