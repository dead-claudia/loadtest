use std::{net::SocketAddr, num::NonZeroU32, time::Duration};

use clap::Parser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpClientOptions {
    pub target: SocketAddr,
    pub connections: NonZeroU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpServerOptions {
    pub location: SocketAddr,
    pub conn_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    TcpClient(TcpClientOptions),
    TcpServer(TcpServerOptions),
}

// Note: the doc comments are also used as part of the help string readouts.

#[derive(clap::Parser)]
#[command(version, author, about)]
enum ClapArgs {
    /// Start a load testing client.
    ///
    /// For TCP connections, connections are opened continuously to a specified target, and each
    /// connection is continuously sending zero bytes to the specified server.
    Client(ClapClient),

    /// Start a load testing server.
    ///
    /// For TCP servers, connections are continuously accepted and their data read but dropped.
    Server(ClapServer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClapLocation {
    Tcp(SocketAddr),
}

fn get_socket_addr(arg: &str) -> anyhow::Result<SocketAddr> {
    let addr = match arg.strip_prefix("localhost:") {
        None => arg.parse()?,
        Some(port_str) => SocketAddr::from(([127, 0, 0, 1], port_str.parse()?)),
    };

    if addr.port() != 0 {
        Ok(addr)
    } else {
        Err(anyhow!("port cannot be zero"))
    }
}

impl std::str::FromStr for ClapLocation {
    type Err = anyhow::Error;

    fn from_str(arg: &str) -> anyhow::Result<Self> {
        match arg.strip_prefix("tcp://") {
            None => match arg.split_once(':') {
                None => Err(anyhow!("invalid URI")),
                Some((protocol, _)) => Err(anyhow!("unknown protocol '{protocol}:'")),
            },
            Some(arg) => get_socket_addr(arg).map(ClapLocation::Tcp),
        }
    }
}

fn parse_connections(s: &str) -> anyhow::Result<NonZeroU32> {
    NonZeroU32::new(s.parse()?).ok_or_else(|| anyhow!("cannot be zero"))
}

#[derive(clap::Args)]
struct ClapClient {
    /// The target to connect to.
    ///
    /// The format is `tcp://<ipaddr>:<port>`, where `<ipaddr>` is either `localhost` or an IP
    /// address, and `<port>` is a port number. The IP address can be any valid IPv4 or IPv6
    /// address, but `<port>` must be non-zero.
    target: ClapLocation,
    /// The number of concurrent tasks.
    ///
    /// Exactly one connection is made per task for TCP, though tasks may not correlate to actual
    /// threads or processes. Note that system and protocol limitations do apply - you can't use
    /// this to generate, say, a million connections to a server from a single client.
    #[arg(short, long, default_value = "1", value_parser = parse_connections)]
    concurrency: NonZeroU32,
}

#[derive(Debug, Clone, Copy)]
struct ServerConnTimeout(Duration);

impl std::str::FromStr for ServerConnTimeout {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.parse()? {
            v if v > 0.0 => Ok(ServerConnTimeout(Duration::from_secs_f64(v))),
            v if v < 0.0 => Err(anyhow!("cannot be negative")),
            _ => Err(anyhow!("cannot be zero")),
        }
    }
}

impl std::fmt::Display for ServerConnTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

#[derive(clap::Args)]
struct ClapServer {
    /// The address and port to listen on.
    ///
    /// The format is `tcp://<ipaddr>:<port>`, where `<ipaddr>` is either `localhost` or an IP
    /// address, and `<port>` is a port number. The IP address can be any valid IPv4 or IPv6
    /// address, but `<port>` must be non-zero.
    location: ClapLocation,
    /// The no-data timeout in seconds.
    ///
    /// Decimals are accepted.
    #[arg(short = 't', long, default_value = "1")]
    connection_timeout: ServerConnTimeout,
}

pub fn parse_cli_clap<I>(args: I) -> clap::error::Result<CliAction>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    match ClapArgs::try_parse_from(args)? {
        ClapArgs::Client(ClapClient {
            target: ClapLocation::Tcp(target),
            concurrency: connections,
        }) => Ok(CliAction::TcpClient(TcpClientOptions {
            target,
            connections,
        })),

        ClapArgs::Server(ClapServer {
            location: ClapLocation::Tcp(location),
            connection_timeout: ServerConnTimeout(conn_timeout),
        }) => Ok(CliAction::TcpServer(TcpServerOptions {
            location,
            conn_timeout,
        })),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use clap::CommandFactory;

    #[test]
    fn cli_is_set_up_correctly() {
        ClapArgs::command().debug_assert();
    }

    fn s(v: &'static str) -> std::ffi::OsString {
        v.into()
    }

    // Simplify all the testing and make it a lot more readable.
    macro_rules! expect_result {
        (
            args: $args:literal,
            result: $result:expr $(,)?
        ) => {
            let value = parse_cli_clap(
                ["loadtest"]
                    .iter()
                    .cloned()
                    .chain($args.split(' '))
                    .map(s)
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| e.render().to_string())
            .unwrap();
            assert_eq!(value, $result);
        };
    }

    macro_rules! expect_error {
        (
            args: $args:literal,
            output: [ $( $line:literal )* ] $(,)?
        ) => {
            expect_error! {
                args: $args,
                raw_output: [
                    $( $line )*
                    ""
                    "For more information try '--help'"
                ],
            }
        };
        (
            args: $args:literal,
            raw_output: [ $( $line:literal )* ] $(,)?
        ) => {
            let e = parse_cli_clap(
                ["loadtest"]
                    .iter()
                    .cloned()
                    .chain($args.split(' '))
                    .map(s)
                    .collect::<Vec<_>>(),
            ).unwrap_err();
            assert_eq!(
                e.render().to_string(),
                concat!($( $line, "\n" ),*),
            )
        };
    }

    #[test]
    fn parses_no_args() {
        let e = parse_cli_clap(vec![s("loadtest")]).unwrap_err();
        assert_eq!(
            e.render().to_string(),
            concat!(
                "A simple load tester utility.\n",
                "\n",
                "Usage: loadtest <COMMAND>\n",
                "\n",
                "Commands:\n",
                "  client  Start a load testing client\n",
                "  server  Start a load testing server\n",
                "  help    Print this message or the help of the given subcommand(s)\n",
                "\n",
                "Options:\n",
                "  -h, --help     Print help information\n",
                "  -V, --version  Print version information\n",
            )
        );
    }

    #[test]
    fn parses_help_summary() {
        expect_error! {
            args: "-h",
            raw_output: [
                "A simple load tester utility."
                ""
                "Usage: loadtest <COMMAND>"
                ""
                "Commands:"
                "  client  Start a load testing client"
                "  server  Start a load testing server"
                "  help    Print this message or the help of the given subcommand(s)"
                ""
                "Options:"
                "  -h, --help     Print help information"
                "  -V, --version  Print version information"
            ],
        }
    }

    #[test]
    fn parses_client_help_summary() {
        expect_error! {
            args: "client -h",
            raw_output: [
                "Start a load testing client"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
                ""
                "Arguments:"
                "  <TARGET>  The target to connect to"
                ""
                "Options:"
                "  -c, --concurrency <CONCURRENCY>  The number of concurrent tasks [default: 1]"
                "  -h, --help                       Print help information (use `--help` for more detail)"
            ],
        }
    }

    #[test]
    fn parses_server_help_summary() {
        expect_error! {
            args: "server -h",
            raw_output: [
                "Start a load testing server"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
                ""
                "Arguments:"
                "  <LOCATION>  The address and port to listen on"
                ""
                "Options:"
                "  -t, --connection-timeout <CONNECTION_TIMEOUT>"
                "          The no-data timeout in seconds [default: 1]"
                "  -h, --help"
                "          Print help information (use `--help` for more detail)"
            ],
        }
    }

    #[test]
    fn parses_bad_subcommand() {
        expect_error! {
            args: "what",
            output: [
                "error: The subcommand 'what' wasn't recognized"
                ""
                "Usage: loadtest <COMMAND>"
            ],
        }
    }

    //
    //  #####
    // #     # #      # ###### #    # #####
    // #       #      # #      ##   #   #
    // #       #      # #####  # #  #   #
    // #       #      # #      #  # #   #
    // #     # #      # #      #   ##   #
    //  #####  ###### # ###### #    #   #
    //

    #[test]
    fn parses_client_no_target() {
        expect_error! {
            args: "client",
            output: [
                "error: The following required arguments were not provided:"
                "  <TARGET>"
                ""
                "Usage: loadtest client <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_target_no_conns() {
        expect_error! {
            args: "client 123",
            output: [
                "error: Invalid value '123' for '<TARGET>': invalid URI"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_target_with_extra() {
        expect_error! {
            args: "client 123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_target_with_conns() {
        expect_error! {
            args: "client 123 -c 456",
            output: [
                "error: Invalid value '123' for '<TARGET>': invalid URI"
            ],
        }
    }

    #[test]
    fn parses_client_unknown_protocol_no_conns() {
        expect_error! {
            args: "client wss://123",
            output: [
                "error: Invalid value 'wss://123' for '<TARGET>': unknown protocol 'wss:'"
            ],
        }
    }

    #[test]
    fn parses_client_unknown_protocol_with_extra() {
        expect_error! {
            args: "client wss://123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_unknown_protocol_with_zero_conns() {
        expect_error! {
            args: "client wss://123 -c 0",
            output: [
                "error: Invalid value 'wss://123' for '<TARGET>': unknown protocol 'wss:'"
            ],
        }
    }

    #[test]
    fn parses_client_udp_protocol_no_conns() {
        expect_error! {
            args: "client udp://123",
            output: [
                "error: Invalid value 'udp://123' for '<TARGET>': unknown protocol 'udp:'"
            ],
        }
    }

    #[test]
    fn parses_client_udp_protocol_with_extra() {
        expect_error! {
            args: "client udp://123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_udp_protocol_with_zero_conns() {
        expect_error! {
            args: "client udp://123 -c 0",
            output: [
                "error: Invalid value 'udp://123' for '<TARGET>': unknown protocol 'udp:'"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_ip_no_conns() {
        expect_error! {
            args: "client tcp://123",
            output: [
                "error: Invalid value 'tcp://123' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_ip_with_extra() {
        expect_error! {
            args: "client tcp://123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_ip_with_zero_conns() {
        expect_error! {
            args: "client tcp://123 -c 0",
            output: [
                "error: Invalid value 'tcp://123' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_invalid_ip_with_conns() {
        expect_error! {
            args: "client tcp://123 -c 456",
            output: [
                "error: Invalid value 'tcp://123' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_without_port_no_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4",
            output: [
                "error: Invalid value 'tcp://1.2.3.4' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_without_port_with_extra() {
        expect_error! {
            args: "client tcp://1.2.3.4 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_without_port_with_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4 -c 456",
            output: [
                "error: Invalid value 'tcp://1.2.3.4' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_without_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4 -c 0",
            output: [
                "error: Invalid value 'tcp://1.2.3.4' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_without_port_no_conns() {
        expect_error! {
            args: "client tcp://localhost",
            output: [
                "error: Invalid value 'tcp://localhost' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_without_port_with_extra() {
        expect_error! {
            args: "client tcp://localhost 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_without_port_with_conns() {
        expect_error! {
            args: "client tcp://localhost -c 456",
            output: [
                "error: Invalid value 'tcp://localhost' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_without_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://localhost -c 0",
            output: [
                "error: Invalid value 'tcp://localhost' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_without_port_no_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]",
            output: [
                "error: Invalid value 'tcp://[1234::5678]' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_without_port_with_extra() {
        expect_error! {
            args: "client tcp://[1234::5678] 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_without_port_with_conns() {
        expect_error! {
            args: "client tcp://[1234::5678] -c 456",
            output: [
                "error: Invalid value 'tcp://[1234::5678]' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_without_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://[1234::5678] -c 0",
            output: [
                "error: Invalid value 'tcp://[1234::5678]' for '<TARGET>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_zero_port_no_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:0",
            output: [
                "error: Invalid value 'tcp://1.2.3.4:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_zero_port_with_extra() {
        expect_error! {
            args: "client tcp://1.2.3.4:0 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_zero_port_with_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:0 -c 456",
            output: [
                "error: Invalid value 'tcp://1.2.3.4:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_zero_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:0 -c 0",
            output: [
                "error: Invalid value 'tcp://1.2.3.4:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_zero_port_no_conns() {
        expect_error! {
            args: "client tcp://localhost:0",
            output: [
                "error: Invalid value 'tcp://localhost:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_zero_port_with_extra() {
        expect_error! {
            args: "client tcp://localhost:0 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_zero_port_with_conns() {
        expect_error! {
            args: "client tcp://localhost:0 -c 456",
            output: [
                "error: Invalid value 'tcp://localhost:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_zero_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://localhost:0 -c 0",
            output: [
                "error: Invalid value 'tcp://localhost:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_zero_port_no_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:0",
            output: [
                "error: Invalid value 'tcp://[1234::5678]:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_zero_port_with_extra() {
        expect_error! {
            args: "client tcp://[1234::5678]:0 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_zero_port_with_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:0 -c 456",
            output: [
                "error: Invalid value 'tcp://[1234::5678]:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_zero_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:0 -c 0",
            output: [
                "error: Invalid value 'tcp://[1234::5678]:0' for '<TARGET>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_no_conns() {
        expect_result! {
            args: "client tcp://1.2.3.4:9876",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([1, 2, 3, 4], 9876)),
                connections: NonZeroU32::new(1).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_extra() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_conns() {
        expect_result! {
            args: "client tcp://1.2.3.4:9876 -c 456",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([1, 2, 3, 4], 9876)),
                connections: NonZeroU32::new(456).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_long_conns_single_arg() {
        expect_result! {
            args: "client tcp://1.2.3.4:9876 --concurrency=456",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([1, 2, 3, 4], 9876)),
                connections: NonZeroU32::new(456).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_long_conns_two_arg() {
        expect_result! {
            args: "client tcp://1.2.3.4:9876 -c 456",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([1, 2, 3, 4], 9876)),
                connections: NonZeroU32::new(456).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 -c 0",
            output: [
                "error: Invalid value '0' for '--concurrency <CONCURRENCY>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_zero_decimal_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 -c 0.0",
            output: [
                "error: Invalid value '0.0' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_non_zero_decimal_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 -c 0.5",
            output: [
                "error: Invalid value '0.5' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_negative_zero_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 --concurrency=-0",
            output: [
                "error: Invalid value '-0' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_negative_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 --concurrency=-1",
            output: [
                "error: Invalid value '-1' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_target_ok_port_with_non_numeric_conns() {
        expect_error! {
            args: "client tcp://1.2.3.4:9876 -c abc",
            output: [
                "error: Invalid value 'abc' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_no_conns() {
        expect_result! {
            args: "client tcp://localhost:9876",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([127, 0, 0, 1], 9876)),
                connections: NonZeroU32::new(1).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_extra() {
        expect_error! {
            args: "client tcp://localhost:9876 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_conns() {
        expect_result! {
            args: "client tcp://localhost:9876 -c 456",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([127, 0, 0, 1], 9876)),
                connections: NonZeroU32::new(456).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://localhost:9876 -c 0",
            output: [
                "error: Invalid value '0' for '--concurrency <CONCURRENCY>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_zero_decimal_conns() {
        expect_error! {
            args: "client tcp://localhost:9876 -c 0.0",
            output: [
                "error: Invalid value '0.0' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_non_zero_decimal_conns() {
        expect_error! {
            args: "client tcp://localhost:9876 -c 0.5",
            output: [
                "error: Invalid value '0.5' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_negative_zero_conns() {
        expect_error! {
            args: "client tcp://localhost:9876 --concurrency=-0",
            output: [
                "error: Invalid value '-0' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_negative_conns() {
        expect_error! {
            args: "client tcp://localhost:9876 --concurrency=-1",
            output: [
                "error: Invalid value '-1' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv4_localhost_ok_port_with_non_numeric_conns() {
        expect_error! {
            args: "client tcp://localhost:9876 -c abc",
            output: [
                "error: Invalid value 'abc' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_no_conns() {
        expect_result! {
            args: "client tcp://[1234::5678]:9876",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([0x1234, 0, 0, 0, 0, 0, 0, 0x5678], 9876)),
                connections: NonZeroU32::new(1).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_extra() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest client [OPTIONS] <TARGET>"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_conns() {
        expect_result! {
            args: "client tcp://[1234::5678]:9876 -c 456",
            result: CliAction::TcpClient(TcpClientOptions {
                target: SocketAddr::from(([0x1234, 0, 0, 0, 0, 0, 0, 0x5678], 9876)),
                connections: NonZeroU32::new(456).unwrap(),
            }),
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_zero_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 -c 0",
            output: [
                "error: Invalid value '0' for '--concurrency <CONCURRENCY>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_zero_decimal_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 -c 0.0",
            output: [
                "error: Invalid value '0.0' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_non_zero_decimal_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 -c 0.5",
            output: [
                "error: Invalid value '0.5' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_negative_zero_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 --concurrency=-0",
            output: [
                "error: Invalid value '-0' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_negative_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 --concurrency=-1",
            output: [
                "error: Invalid value '-1' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    #[test]
    fn parses_client_ipv6_target_ok_port_with_non_numeric_conns() {
        expect_error! {
            args: "client tcp://[1234::5678]:9876 -c abc",
            output: [
                "error: Invalid value 'abc' for '--concurrency <CONCURRENCY>': invalid digit found in string"
            ],
        }
    }

    //
    //  #####
    // #     # ###### #####  #    # ###### #####
    // #       #      #    # #    # #      #    #
    //  #####  #####  #    # #    # #####  #    #
    //       # #      #####  #    # #      #####
    // #     # #      #   #   #  #  #      #   #
    //  #####  ###### #    #   ##   ###### #    #
    //

    #[test]
    fn parses_server_no_target() {
        expect_error! {
            args: "server",
            output: [
                "error: The following required arguments were not provided:"
                "  <LOCATION>"
                ""
                "Usage: loadtest server <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_target_no_conns() {
        expect_error! {
            args: "server 123",
            output: [
                "error: Invalid value '123' for '<LOCATION>': invalid URI"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_target_with_extra() {
        expect_error! {
            args: "server 123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_target_with_conns() {
        expect_error! {
            args: "server 123 -t 456",
            output: [
                "error: Invalid value '123' for '<LOCATION>': invalid URI"
            ],
        }
    }

    #[test]
    fn parses_server_unknown_protocol_no_conns() {
        expect_error! {
            args: "server wss://123",
            output: [
                "error: Invalid value 'wss://123' for '<LOCATION>': unknown protocol 'wss:'"
            ],
        }
    }

    #[test]
    fn parses_server_unknown_protocol_with_extra() {
        expect_error! {
            args: "server wss://123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_unknown_protocol_with_zero_timeout() {
        expect_error! {
            args: "server wss://123 -t 0",
            output: [
                "error: Invalid value 'wss://123' for '<LOCATION>': unknown protocol 'wss:'"
            ],
        }
    }

    #[test]
    fn parses_server_udp_protocol_no_timeout() {
        expect_error! {
            args: "server udp://123",
            output: [
                "error: Invalid value 'udp://123' for '<LOCATION>': unknown protocol 'udp:'"
            ],
        }
    }

    #[test]
    fn parses_server_udp_protocol_with_extra() {
        expect_error! {
            args: "server udp://123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_udp_protocol_with_zero_timeout() {
        expect_error! {
            args: "server udp://123 -t 0",
            output: [
                "error: Invalid value 'udp://123' for '<LOCATION>': unknown protocol 'udp:'"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_ip_no_timeout() {
        expect_error! {
            args: "server tcp://123",
            output: [
                "error: Invalid value 'tcp://123' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_ip_with_extra() {
        expect_error! {
            args: "server tcp://123 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_ip_with_zero_timeout() {
        expect_error! {
            args: "server tcp://123 -t 0",
            output: [
                "error: Invalid value 'tcp://123' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_invalid_ip_with_timeout() {
        expect_error! {
            args: "server tcp://123 -t 456",
            output: [
                "error: Invalid value 'tcp://123' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_without_port_no_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4",
            output: [
                "error: Invalid value 'tcp://1.2.3.4' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_without_port_with_extra() {
        expect_error! {
            args: "server tcp://1.2.3.4 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_without_port_with_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4 -t 456",
            output: [
                "error: Invalid value 'tcp://1.2.3.4' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_without_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4 -t 0",
            output: [
                "error: Invalid value 'tcp://1.2.3.4' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_without_port_no_timeout() {
        expect_error! {
            args: "server tcp://localhost",
            output: [
                "error: Invalid value 'tcp://localhost' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_without_port_with_extra() {
        expect_error! {
            args: "server tcp://localhost 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_without_port_with_timeout() {
        expect_error! {
            args: "server tcp://localhost -t 456",
            output: [
                "error: Invalid value 'tcp://localhost' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_without_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://localhost -t 0",
            output: [
                "error: Invalid value 'tcp://localhost' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_without_port_no_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]",
            output: [
                "error: Invalid value 'tcp://[1234::5678]' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_without_port_with_extra() {
        expect_error! {
            args: "server tcp://[1234::5678] 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_without_port_with_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678] -t 456",
            output: [
                "error: Invalid value 'tcp://[1234::5678]' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_without_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678] -t 0",
            output: [
                "error: Invalid value 'tcp://[1234::5678]' for '<LOCATION>': invalid socket address syntax"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_zero_port_no_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:0",
            output: [
                "error: Invalid value 'tcp://1.2.3.4:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_zero_port_with_extra() {
        expect_error! {
            args: "server tcp://1.2.3.4:0 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_zero_port_with_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:0 -t 456",
            output: [
                "error: Invalid value 'tcp://1.2.3.4:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_zero_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:0 -t 0",
            output: [
                "error: Invalid value 'tcp://1.2.3.4:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_zero_port_no_timeout() {
        expect_error! {
            args: "server tcp://localhost:0",
            output: [
                "error: Invalid value 'tcp://localhost:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_zero_port_with_extra() {
        expect_error! {
            args: "server tcp://localhost:0 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_zero_port_with_timeout() {
        expect_error! {
            args: "server tcp://localhost:0 -t 456",
            output: [
                "error: Invalid value 'tcp://localhost:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_zero_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://localhost:0 -t 0",
            output: [
                "error: Invalid value 'tcp://localhost:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_zero_port_no_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:0",
            output: [
                "error: Invalid value 'tcp://[1234::5678]:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_zero_port_with_extra() {
        expect_error! {
            args: "server tcp://[1234::5678]:0 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_zero_port_with_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:0 -t 456",
            output: [
                "error: Invalid value 'tcp://[1234::5678]:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_zero_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:0 -t 0",
            output: [
                "error: Invalid value 'tcp://[1234::5678]:0' for '<LOCATION>': port cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_no_timeout() {
        expect_result! {
            args: "server tcp://1.2.3.4:9876",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([1, 2, 3, 4], 9876)),
                conn_timeout: Duration::from_secs(1),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_extra() {
        expect_error! {
            args: "server tcp://1.2.3.4:9876 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_timeout() {
        expect_result! {
            args: "server tcp://1.2.3.4:9876 -t 456",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([1, 2, 3, 4], 9876)),
                conn_timeout: Duration::from_secs(456),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_long_timeout_single_arg() {
        expect_result! {
            args: "server tcp://1.2.3.4:9876 --connection-timeout=456",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([1, 2, 3, 4], 9876)),
                conn_timeout: Duration::from_secs(456),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_long_timeout_two_arg() {
        expect_result! {
            args: "server tcp://1.2.3.4:9876 -t 456",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([1, 2, 3, 4], 9876)),
                conn_timeout: Duration::from_secs(456),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:9876 -t 0",
            output: [
                "error: Invalid value '0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_zero_decimal_conns() {
        expect_error! {
            args: "server tcp://1.2.3.4:9876 -t 0.0",
            output: [
                "error: Invalid value '0.0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_non_zero_decimal_conns() {
        expect_result! {
            args: "server tcp://1.2.3.4:9876 -t 0.5",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([1, 2, 3, 4], 9876)),
                conn_timeout: Duration::from_millis(500),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_negative_zero_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:9876 --connection-timeout=-0",
            output: [
                "error: Invalid value '-0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_negative_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:9876 --connection-timeout=-1",
            output: [
                "error: Invalid value '-1' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be negative"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_target_ok_port_with_non_numeric_timeout() {
        expect_error! {
            args: "server tcp://1.2.3.4:9876 -t abc",
            output: [
                "error: Invalid value 'abc' for '--connection-timeout <CONNECTION_TIMEOUT>': invalid float literal"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_no_timeout() {
        expect_result! {
            args: "server tcp://localhost:9876",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([127, 0, 0, 1], 9876)),
                conn_timeout: Duration::from_secs(1),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_extra() {
        expect_error! {
            args: "server tcp://localhost:9876 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_timeout() {
        expect_result! {
            args: "server tcp://localhost:9876 -t 456",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([127, 0, 0, 1], 9876)),
                conn_timeout: Duration::from_secs(456),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://localhost:9876 -t 0",
            output: [
                "error: Invalid value '0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_zero_decimal_conns() {
        expect_error! {
            args: "server tcp://localhost:9876 -t 0.0",
            output: [
                "error: Invalid value '0.0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_non_zero_decimal_conns() {
        expect_result! {
            args: "server tcp://localhost:9876 -t 0.5",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([127, 0, 0, 1], 9876)),
                conn_timeout: Duration::from_millis(500),
            }),
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_negative_zero_timeout() {
        expect_error! {
            args: "server tcp://localhost:9876 --connection-timeout=-0",
            output: [
                "error: Invalid value '-0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_negative_timeout() {
        expect_error! {
            args: "server tcp://localhost:9876 --connection-timeout=-1",
            output: [
                "error: Invalid value '-1' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be negative"
            ],
        }
    }

    #[test]
    fn parses_server_ipv4_localhost_ok_port_with_non_numeric_timeout() {
        expect_error! {
            args: "server tcp://localhost:9876 -t abc",
            output: [
                "error: Invalid value 'abc' for '--connection-timeout <CONNECTION_TIMEOUT>': invalid float literal"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_no_timeout() {
        expect_result! {
            args: "server tcp://[1234::5678]:9876",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([0x1234, 0, 0, 0, 0, 0, 0, 0x5678], 9876)),
                conn_timeout: Duration::from_secs(1),
            }),
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_extra() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 456",
            output: [
                "error: Found argument '456' which wasn't expected, or isn't valid in this context"
                ""
                "Usage: loadtest server [OPTIONS] <LOCATION>"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_timeout() {
        expect_result! {
            args: "server tcp://[1234::5678]:9876 -t 456",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([0x1234, 0, 0, 0, 0, 0, 0, 0x5678], 9876)),
                conn_timeout: Duration::from_secs(456),
            }),
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_zero_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 -t 0",
            output: [
                "error: Invalid value '0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_zero_decimal_conns() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 -t 0.0",
            output: [
                "error: Invalid value '0.0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_non_zero_decimal_conns() {
        expect_result! {
            args: "server tcp://[1234::5678]:9876 -t 0.5",
            result: CliAction::TcpServer(TcpServerOptions {
                location: SocketAddr::from(([0x1234, 0, 0, 0, 0, 0, 0, 0x5678], 9876)),
                conn_timeout: Duration::from_millis(500),
            }),
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_negative_zero_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 --connection-timeout=-0",
            output: [
                "error: Invalid value '-0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_negative_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 --connection-timeout=-1",
            output: [
                "error: Invalid value '-1' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be negative"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_negative_decimal_zero_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 --connection-timeout=-0.0",
            output: [
                "error: Invalid value '-0.0' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be zero"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_negative_half_second_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 --connection-timeout=-0.5",
            output: [
                "error: Invalid value '-0.5' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be negative"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_negative_1_5_second_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 --connection-timeout=-1.5",
            output: [
                "error: Invalid value '-1.5' for '--connection-timeout <CONNECTION_TIMEOUT>': cannot be negative"
            ],
        }
    }

    #[test]
    fn parses_server_ipv6_target_ok_port_with_non_numeric_timeout() {
        expect_error! {
            args: "server tcp://[1234::5678]:9876 -t abc",
            output: [
                "error: Invalid value 'abc' for '--connection-timeout <CONNECTION_TIMEOUT>': invalid float literal"
            ],
        }
    }
}
