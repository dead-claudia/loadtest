# Simple network load tester

This just generates and receives streams of zeroes over TCP. There's two subcomponents, a client and a server, and they're all bundled into the same command. I've verified that under localhost, it can exceed 50 Gbps, so it should be able to flood about any connection.

Under the hood, it's all written in [Rust](https://www.rust-lang.org/) and uses [Tokio](https://tokio.rs/).

> Little bit of background: I wanted something that was simple and Just Works to do some rudimentary capacity testing in my local network, and everything I found fell into one or more of the following categories:
>
> - Not free
> - Too high level (like wrk and wrk2 which only do HTTP requests)
> - Overly complicated to set up (like Locust)
> - A GUI program only (like JMeter)
>
> My initial draft of this was a simple 3-file Rust program, but of course, that's grown a bit.

- [Platform support](#platform-support)
- [Building](#building)
- [Client](#client)
- [Server](#server)
- [Performance notes](#performance-notes)
- [Contributing](#contributing)
  - [Solicited contributions](#solicited-contributions)
- [License](#license)

## Platform support

I primarily developed this on Linux via WSL 2 and have run it on a pure Linux box, so that's where it's most stable.

Additionally, automated testing is performed on the following platforms, using GitHub Actions:

- Linux
- MacOS
- Windows (note: integration tests haven't been ported over yet)

## Building

1. Install [Rust and Cargo](https://www.rust-lang.org/), if you haven't already.
2. Run `cargo build --release`.
3. Your binary is now located in `target/release/loadtest`. You can copy that out and put it wherever you need it.

I don't currently provide pre-made binaries for this, so you have to build them yourself.

## Client

Usage: `loadtest client tcp://<ipaddr>:<port> <concurrent_conns>`

- `<ipaddr>`: Either `localhost` (alias for `127.0.0.1`) or a literal IP address. Both IPv4 and IPv6 addresses are accepted, though IPv6 addresses must be bracketed.
- `<port>`: A TCP port to target.
- `<concurrent_conns>`: The number of concurrent connections you want to establish, defaults to 1. Note that this is subject to system and protocol limitations - you can't use this to generate, say, a million connections to a server from a single client.

## Server

Usage: `loadtest server tcp <port> <no_data_timeout>`

- `<port>`: A TCP port to listen to.
- `<no_data_timeout>`: A decimal no-data connection timeout in seconds. Decimals are accepted and it defaults to 1.

Note: this does *not* limit the number of connections to a client.

## Performance notes

CPU time is mostly bound on connection count, even on localhost. I tested it with a single client connection to a single server, and it required almost no CPU time to process.

## Contributing

Contributions are accepted, though anything new as well as changes to any existing functionality needs to be tested.

### Solicited contributions

I do have a few types of contributions I'd specifically like to invite.

**Set up integration tests for Windows**

I currently am only using this on Linux (and am making a best effort to make it runnable on Mac), so it's low priority for me. But if someone wants to port the integration tests to Windows (ideally, using Powershell and its background jobs functionality), I'd love to include it provided tests pass.

Note: unit tests should already be running against Windows.

**Make the load test able to go in reverse**

Specifically, I want to be able to 1. connect and get flooded with zeroes and 2. connect and go both ways. This would allow for a number of things:

- On full duplex links, one could test the throughput of both directions in a single go.
- On half duplex links, one could test each direction individually.
- If it's unknown whether there exists a link that's half duplex, running it in one direction then both would give a very easy answer.
- In zero-trust setups with firewalls, it may be easier to have machines under test subscribe to get flooded than it would be for them to run the server on their end and reconfigure the firewall to forward the port.

The code would need quite a bit of revision to support this, though.

**DNS resolution**

DNS resolution for the client IP address would be particularly helpful, so it can accept general TCP URIs rather than just IP addresses.

**Client statistics**

A client loop similar to the server loop that prints how many bytes are sent out and how many actually active connections are present.

**UDP sockets**

A UDP tester would also be valuable due to the different properties it has (like no congestion avoidance mechanisms).

Design notes: don't accept either a client connection count or a server data timeout. Neither are useful in this context. And for the server statistics, skip the connections count for similar reasons. All that matters is bytes per second and packets per second (and packet size *does* need configurable).

Implementation note: aim for zero copy, and try to reuse what resources you can.

- The actual TCP receive loop does *not* copy any bytes, and simply uses a read buffer shared across all connections and threads as it's never read from. This same read buffer should be reused for received UDP messages.
- The client should use a pre-allocated packet of 65536 (theoretical max UDP packet length) zeroes, and it needs to have a configurable maximum packet length.
  - The packet input data itself can be statically allocated, but the end length cannot.
  - The default length must be the largest safe packet size: 576 for IPv4 and 1280 for IPv6.
- Consider on Linux using the [`sendmmsg`](https://www.man7.org/linux/man-pages/man2/sendmmsg.2.html) and [`recvmmsg`](https://www.man7.org/linux/man-pages/man2/recvmmsg.2.html) syscalls instead of Tokio's own high-level `send` and `recv` methods. (You can get this via `udp_socket.as_raw_fd()`) The send buffer can itself be shared for the lifetime of the program, and the recv buffer can be simply declared as a series of global variables.
  - Be careful to only do that within Tokio's `tokio::task::block_in_place` so Tokio can do the right thing here.
  - Consider supporting up to 1024 packets each direction. Doesn't need to be small, as this data structure isn't replicated.
  - Do use a connected socket, so the kernel can avoid some work on routing them.

**Reduce TCP work**

Some descriptor-based hackery could be used to reduce work significantly on Linux.

- Work for inbound data could be reduced significantly by instead [splicing (read: `splice`)](https://man7.org/linux/man-pages/man2/splice.2.html) from each inbound socket to a shared intermediate pipe that itself is spliced to a shared `/dev/zero` descriptor.
- Work for outbound data could likewise be reduced significantly by doing the same, but in reverse.
  - Note: the `/dev/zero` descriptor could be opened read/write and be reused across both.
- In both cases, `usize::MAX` should be used for the max length so it can do as much as possible, but it also must be done non-blocking to avoid clogging up Tokio's event loop (and making it effectively one thread per connection).
  - Note: return `Poll::Pending` from the related future if the syscall returns `EAGAIN`/`io::ErrorKind::WouldBlock`.
- Don't roll this directly - just use [`tokio_pipe`](https://docs.rs/tokio-pipe/latest/tokio_pipe/) and call it a day.
  - Note: reads and writes do *not* need to be atomic.
- As tantalizing as it might seem, [no, you can't `sendfile` from `/dev/zero`, and you also can't just `splice` the socket to `/dev/null`, because that's not how the data model is structured](https://yarchive.net/comp/linux/splice.html).
  - Pipes are literally just kernel byte queues linked to a pair of file descriptors referencing the read and write halves of it. The file descriptors are streams, but the resource they encapsulate is just a chunk of memory.
  - Zero copy is only supported in two cases: to and from those byte queues and the special case of from a physical file to a stream-oriented socket (but not vice versa).

This will *not* be able to carry over into UDP, as that's inherently message-based. (You'd have to use something like BPF or Linux's io\_uring to accelerate that.)

## License

This is free and unencumbered software released into the public domain.

Anyone is free to copy, modify, publish, use, compile, sell, or
distribute this software, either in source code form or as a compiled
binary, for any purpose, commercial or non-commercial, and by any
means.

In jurisdictions that recognize copyright laws, the author or authors
of this software dedicate any and all copyright interest in the
software to the public domain. We make this dedication for the benefit
of the public at large and to the detriment of our heirs and
successors. We intend this dedication to be an overt act of
relinquishment in perpetuity of all present and future rights to this
software under copyright law.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.

For more information, please refer to <https://unlicense.org>
