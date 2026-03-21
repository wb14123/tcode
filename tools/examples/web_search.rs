use llm_rs::tool::{CancellationToken, ToolContext};
use std::env;
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        eprintln!("Usage: {} <query>", args[0]);
        std::process::exit(1);
    }

    let query = &args[1];
    let tool = tools::web_search_tool();

    let ctx = ToolContext {
        cancel_token: CancellationToken::new(),
        permission: llm_rs::permission::ScopedPermissionManager::always_allow("web_search"),
    };
    let json_params = serde_json::json!({ "query": query }).to_string();
    let mut stream = tool.execute(ctx, json_params);

    while let Some(output) = stream.next().await {
        println!("{}", output);
    }
}
