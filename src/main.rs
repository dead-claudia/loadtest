#[macro_use]
extern crate async_trait;
#[macro_use]
extern crate anyhow;
mod cli_args;
mod client_loop;
mod server_loop;
mod stats;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Skip the first value as it's just the program name.
    match cli_args::parse_cli_clap(std::env::args_os()) {
        Ok(cli_args::CliAction::TcpClient(opts)) => client_loop::client_loop(opts).await?,
        Ok(cli_args::CliAction::TcpServer(opts)) => server_loop::server_loop(opts).await?,
        Err(e) => e.exit(),
    };

    Ok(())
}

// Test-only dependencies
#[cfg(test)]
#[macro_use]
extern crate matches;
#[cfg(test)]
mod test_utils;
