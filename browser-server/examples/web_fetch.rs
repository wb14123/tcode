use std::env;
use std::time::Instant;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "browser_server=info".parse().unwrap()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        eprintln!("Usage: {} <url>", args[0]);
        std::process::exit(1);
    }

    let url = &args[1];

    eprintln!("[{:.1}s] Starting web_fetch for: {url}", 0.0);
    let start = Instant::now();

    match browser_server::web_fetch::fetch_and_extract(url) {
        Ok(content) => {
            eprintln!("[{:.1}s] Got content", start.elapsed().as_secs_f64());
            println!("{content}");
        }
        Err(e) => {
            eprintln!("[{:.1}s] Error: {e}", start.elapsed().as_secs_f64());
            browser_server::browser::shutdown_browser();
            std::process::exit(1);
        }
    }

    eprintln!("[{:.1}s] Done", start.elapsed().as_secs_f64());
    browser_server::browser::shutdown_browser();
}
