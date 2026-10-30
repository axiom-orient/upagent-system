#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    match perfectpixel::mcp::startup(std::env::args().skip(1).collect()) {
        Ok(perfectpixel::mcp::Startup::Help) => {
            println!("{}", perfectpixel::mcp::MCP_HELP);
        }
        Ok(perfectpixel::mcp::Startup::Server(server)) => {
            if let Err(error) = perfectpixel::mcp::serve(server).await {
                eprintln!("perfectpixel-mcp: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("perfectpixel-mcp: {error}");
            std::process::exit(2);
        }
    }
}
