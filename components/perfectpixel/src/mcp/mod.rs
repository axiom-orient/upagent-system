mod params;
mod root;
mod server;

pub use server::{serve, startup, PerfectPixelMcp, Startup, StartupError, MCP_HELP};
