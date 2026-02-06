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

    let json_params = serde_json::json!({ "query": query }).to_string();
    let mut stream = tool.execute(json_params);

    while let Some(output) = stream.next().await {
        println!("{}", output);
    }
}
