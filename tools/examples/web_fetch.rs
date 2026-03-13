use std::env;
use std::time::Instant;
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

    // Direct test with timing for each step
    eprintln!("[{:.1}s] Starting web_fetch for: {url}", 0.0);
    let start = Instant::now();

    let tool = tools::web_fetch_tool();
    let ctx = ToolContext { cancel_token: CancellationToken::new() };
    let json_params = serde_json::json!({ "url": url }).to_string();
    let mut stream = tool.execute(ctx, json_params);

    while let Some(output) = stream.next().await {
        eprintln!("[{:.1}s] Got output", start.elapsed().as_secs_f64());
        println!("{}", output);
    }
    eprintln!("[{:.1}s] Done", start.elapsed().as_secs_f64());
}
