use std::sync::Arc;
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{timeout, Duration, MissedTickBehavior},
};

use crate::stats::TcpServerStats;

const BUFFER_SIZE: usize = 1024 * 1024;
static mut READ_BUFFER: [u8; BUFFER_SIZE] = [0; BUFFER_SIZE];

struct IgnoreReadBuffer;

unsafe impl bytes::BufMut for IgnoreReadBuffer {
    fn remaining_mut(&self) -> usize {
        BUFFER_SIZE
    }

    unsafe fn advance_mut(&mut self, _cnt: usize) {}

    fn chunk_mut(&mut self) -> &mut bytes::buf::UninitSlice {
        unsafe {
            bytes::buf::UninitSlice::from_raw_parts_mut(READ_BUFFER.as_mut_ptr(), BUFFER_SIZE)
        }
    }
}

#[async_trait]
trait SocketHooks {
    type Elapsed;
    async fn read(&mut self) -> Result<io::Result<usize>, Self::Elapsed>;
    async fn write(&mut self) -> io::Result<()>;
    async fn shutdown(&mut self) -> io::Result<()>;
    fn stats(&self) -> &TcpServerStats;
    fn emit_warning(&self, e: io::Error);
}

async fn socket_handler<H: SocketHooks>(mut socket_hooks: H) -> io::Result<()> {
    socket_hooks.stats().add_connection();

    enum Status {
        Open,
        Closed,
        Err(io::Error),
    }

    let result = 'read: loop {
        match socket_hooks.read().await {
            Ok(Ok(0)) => break 'read Status::Open,
            Ok(Ok(bytes)) => {
                socket_hooks.stats().push_received_bytes(bytes);
                'write: loop {
                    match socket_hooks.write().await {
                        Ok(()) => break 'write,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue 'write,
                        Err(e) => break 'read Status::Err(e),
                    }
                }
            }
            Ok(Err(e)) => match e.kind() {
                io::ErrorKind::ConnectionReset => break 'read Status::Closed,
                io::ErrorKind::ConnectionAborted => break 'read Status::Closed,
                io::ErrorKind::BrokenPipe => break 'read Status::Closed,
                io::ErrorKind::WriteZero => break 'read Status::Closed,
                io::ErrorKind::WouldBlock => continue 'read,
                _ => break 'read Status::Err(e),
            },
            Err(_) => break 'read Status::Open,
        }
    };

    socket_hooks.stats().remove_connection();

    match result {
        Status::Closed => Ok(()),
        Status::Open => loop {
            match socket_hooks.shutdown().await {
                Ok(()) => break Ok(()),
                Err(e) => match e.kind() {
                    io::ErrorKind::ConnectionReset => break Ok(()),
                    io::ErrorKind::ConnectionAborted => break Ok(()),
                    io::ErrorKind::BrokenPipe => break Ok(()),
                    io::ErrorKind::WriteZero => break Ok(()),
                    io::ErrorKind::WouldBlock => continue,
                    _ => break Err(e),
                },
            }
        },
        Status::Err(e) => {
            socket_hooks.emit_warning(e);
            Ok(())
        }
    }
}

#[async_trait]
trait ServerHooks {
    type Socket;
    fn conn_timeout(&self) -> Duration;
    fn stats(&self) -> &TcpServerStats;
    fn emit_snapshot(&self, snapshot: String);
    async fn accept(&self) -> io::Result<Self::Socket>;
}

async fn accept_once<H: ServerHooks>(server_hooks: &H) -> io::Result<H::Socket> {
    loop {
        match server_hooks.accept().await {
            Ok(socket_hooks) => return Ok(socket_hooks),
            Err(e) => match e.kind() {
                io::ErrorKind::ConnectionReset => continue,
                io::ErrorKind::ConnectionAborted => continue,
                io::ErrorKind::WouldBlock => continue,
                _ => return Err(e),
            },
        };
    }
}

async fn timer_loop<H: ServerHooks>(server_hooks: Arc<H>, period: Duration) {
    let stats = server_hooks.stats();
    let mut ref_time = tokio::time::Instant::now();
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // Scheduled interval is 1 second, but that isn't actually guaranteed.
        let next_instant = interval.tick().await;
        let duration = next_instant.saturating_duration_since(ref_time);
        if let Some(snapshot) = stats.get_snapshot(duration) {
            ref_time = next_instant;
            server_hooks.emit_snapshot(snapshot);
        }
    }
}

struct ServerWrap {
    _stats: TcpServerStats,
    _conn_timeout: Duration,
    _listener: TcpListener,
}

#[async_trait]
impl ServerHooks for ServerWrap {
    type Socket = TcpStream;

    fn conn_timeout(&self) -> Duration {
        self._conn_timeout
    }

    async fn accept(&self) -> io::Result<Self::Socket> {
        self._listener.accept().await.map(|s| s.0)
    }

    fn stats(&self) -> &TcpServerStats {
        &self._stats
    }

    fn emit_snapshot(&self, snapshot: String) {
        eprintln!("{}", snapshot)
    }
}

struct SocketWrap {
    _hooks: Arc<ServerWrap>,
    _socket: TcpStream,
}

#[async_trait]
impl SocketHooks for SocketWrap {
    type Elapsed = tokio::time::error::Elapsed;

    fn stats(&self) -> &TcpServerStats {
        &self._hooks._stats
    }

    fn emit_warning(&self, e: io::Error) {
        eprintln!("{}", e)
    }

    async fn read(&mut self) -> Result<io::Result<usize>, tokio::time::error::Elapsed> {
        timeout(
            self._hooks.conn_timeout(),
            self._socket.read_buf(&mut IgnoreReadBuffer),
        )
        .await
    }

    async fn write(&mut self) -> io::Result<()> {
        self._socket.write_u8(0).await
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        self._socket.shutdown().await
    }
}

pub async fn server_loop(options: crate::cli_args::TcpServerOptions) -> anyhow::Result<()> {
    let listener = TcpListener::bind(options.location).await?;
    let server_wrap = Arc::new(ServerWrap {
        _stats: TcpServerStats::new(),
        _conn_timeout: options.conn_timeout,
        _listener: listener,
    });

    tokio::spawn(timer_loop(server_wrap.clone(), Duration::from_secs(1)));

    loop {
        let hooks = server_wrap.clone();
        tokio::spawn(socket_handler(SocketWrap {
            _socket: accept_once(&*hooks).await?,
            _hooks: hooks,
        }));
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_utils::*;

    use bytes::BufMut;

    #[test]
    fn ignore_read_buffer_returns_right_slice_size() {
        let mut buf = IgnoreReadBuffer;
        assert_eq!(buf.chunk_mut().len(), buf.remaining_mut());
    }

    #[derive(Debug)]
    struct FakeHooksInner {
        stats: TcpServerStats,
        read: CallSpy<Result<std::io::Result<usize>, ()>>,
        write: CallSpy<std::io::Result<()>>,
        shutdown: CallSpy<std::io::Result<()>>,
        accept: CallSpy<std::io::Result<()>>,
        snapshots: std::sync::Mutex<Vec<String>>,
        warnings: std::sync::Mutex<Vec<io::ErrorKind>>,
    }

    impl FakeHooksInner {
        fn new() -> FakeHooksInner {
            FakeHooksInner {
                stats: TcpServerStats::new(),
                accept: CallSpy::new("unexpected `accept`"),
                read: CallSpy::new("unexpected `read`"),
                write: CallSpy::new("unexpected `write`"),
                shutdown: CallSpy::new("unexpected `shutdown`"),
                snapshots: std::sync::Mutex::new(Vec::new()),
                warnings: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn push_snapshot(&self) {
            self.snapshots.lock().unwrap().push(
                self.stats
                    .get_snapshot(Duration::from_secs(1))
                    .unwrap_or_default(),
            )
        }
    }

    trait FakeHooksMethods {
        fn __inner__(&self) -> &FakeHooksInner;

        fn snapshots(&self) -> Vec<String> {
            self.__inner__().snapshots.lock().unwrap().clone()
        }

        fn warnings(&self) -> Vec<io::ErrorKind> {
            self.__inner__().warnings.lock().unwrap().clone()
        }

        fn accepted(&self) -> usize {
            self.__inner__().accept.calls()
        }

        // Don't accept a full socket, as that can just be generated later.
        fn push_accept(&self, result: std::io::Result<()>) {
            self.__inner__().accept.push(result);
        }

        fn push_read(&self, result: Result<std::io::Result<usize>, ()>) {
            self.__inner__().read.push(result);
        }

        fn push_write(&self, result: std::io::Result<()>) {
            self.__inner__().write.push(result);
        }

        fn push_shutdown(&self, result: std::io::Result<()>) {
            self.__inner__().shutdown.push(result);
        }

        fn assert_empty(&self) {
            let inner = self.__inner__();
            assert_eq!(inner.accept.remaining(), 0, "`accept` queue is not empty.");
            assert_eq!(inner.read.remaining(), 0, "`read` queue is not empty.");
            assert_eq!(inner.write.remaining(), 0, "`write` queue is not empty.");
            assert_eq!(
                inner.shutdown.remaining(),
                0,
                "`shutdown` queue is not empty."
            );
        }
    }

    #[derive(Debug, Clone)]
    struct FakeSocketHooks {
        inner: Arc<FakeHooksInner>,
    }

    impl FakeSocketHooks {
        fn new() -> FakeSocketHooks {
            FakeSocketHooks {
                inner: Arc::new(FakeHooksInner::new()),
            }
        }
    }

    impl FakeHooksMethods for FakeSocketHooks {
        fn __inner__(&self) -> &FakeHooksInner {
            &self.inner
        }
    }

    #[async_trait]
    impl SocketHooks for FakeSocketHooks {
        type Elapsed = ();

        async fn read(&mut self) -> Result<std::io::Result<usize>, ()> {
            let value = self.inner.read.invoke();
            self.inner.push_snapshot();
            value
        }

        async fn write(&mut self) -> std::io::Result<()> {
            let value = self.inner.write.invoke();
            self.inner.push_snapshot();
            value
        }

        async fn shutdown(&mut self) -> std::io::Result<()> {
            let value = self.inner.shutdown.invoke();
            self.inner.push_snapshot();
            value
        }

        fn stats(&self) -> &TcpServerStats {
            &self.inner.stats
        }

        fn emit_warning(&self, e: io::Error) {
            self.inner.warnings.lock().unwrap().push(e.kind());
        }
    }

    #[derive(Debug, Clone)]
    struct FakeServerHooks {
        inner: Arc<FakeHooksInner>,
    }

    impl FakeServerHooks {
        fn new() -> FakeServerHooks {
            FakeServerHooks {
                inner: Arc::new(FakeHooksInner::new()),
            }
        }
    }

    impl FakeHooksMethods for FakeServerHooks {
        fn __inner__(&self) -> &FakeHooksInner {
            &self.inner
        }
    }

    #[async_trait]
    impl ServerHooks for FakeServerHooks {
        type Socket = FakeSocketHooks;

        fn conn_timeout(&self) -> Duration {
            Duration::from_secs(1)
        }

        async fn accept(&self) -> io::Result<Self::Socket> {
            self.inner.accept.invoke().map(|_| FakeSocketHooks {
                inner: self.inner.clone(),
            })
        }

        fn stats(&self) -> &TcpServerStats {
            &self.inner.stats
        }

        fn emit_snapshot(&self, snapshot: String) {
            self.inner.snapshots.lock().unwrap().push(snapshot)
        }
    }

    #[test]
    fn fake_socket_hooks_starts_with_empty_queues() {
        let socket_hooks = FakeSocketHooks::new();
        socket_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_socket_assert_empty_hooks_fails_with_non_empty_read() {
        let socket_hooks = FakeSocketHooks::new();
        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_socket_assert_empty_hooks_fails_with_non_empty_write() {
        let socket_hooks = FakeSocketHooks::new();
        socket_hooks.push_write(Ok(()));
        socket_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_socket_assert_empty_hooks_fails_with_non_empty_shutdown() {
        let socket_hooks = FakeSocketHooks::new();
        socket_hooks.push_shutdown(Ok(()));
        socket_hooks.assert_empty();
    }

    #[test]
    #[should_panic = "unexpected `read`"]
    fn fake_socket_hooks_requires_queued_read_to_invoke_read() {
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(FakeSocketHooks::new().read());
    }

    #[test]
    #[should_panic = "unexpected `write`"]
    fn fake_socket_hooks_requires_queued_write_to_invoke_write() {
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(FakeSocketHooks::new().write());
    }

    #[test]
    #[should_panic = "unexpected `shutdown`"]
    fn fake_socket_hooks_requires_queued_shutdown_to_invoke_shutdown() {
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(FakeSocketHooks::new().shutdown());
    }

    #[test]
    fn fake_socket_hooks_runs_hooks_correctly() {
        let mut socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_read(Ok(Ok(123)));
        socket_hooks.stats().add_connection();
        socket_hooks.stats().push_received_bytes(1000);
        assert_matches!(run_future(socket_hooks.read()), Ok(Ok(0)));

        socket_hooks.push_write(Ok(()));
        socket_hooks.push_write(Ok(()));
        socket_hooks.stats().push_received_bytes(1000);
        assert_matches!(run_future(socket_hooks.write()), Ok(()));

        socket_hooks.emit_warning(io::ErrorKind::InvalidInput.into());

        socket_hooks.push_shutdown(Ok(()));
        socket_hooks.push_shutdown(Ok(()));
        socket_hooks.stats().push_received_bytes(1000);
        socket_hooks.stats().remove_connection();
        assert_matches!(run_future(socket_hooks.shutdown()), Ok(()));

        socket_hooks.stats().add_connection();
        socket_hooks.stats().push_received_bytes(1000);
        assert_matches!(run_future(socket_hooks.read()), Ok(Ok(123)));
        assert_matches!(run_future(socket_hooks.write()), Ok(()));
        socket_hooks.stats().push_received_bytes(1000);
        socket_hooks.stats().remove_connection();
        assert_matches!(run_future(socket_hooks.shutdown()), Ok(()));
        socket_hooks.emit_warning(io::ErrorKind::Other.into());

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 8 Kb/s in"),
                String::from("1 conns, 8 Kb/s in"),
                String::from("0 conns, 8 Kb/s in"),
                String::from("1 conns, 8 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 8 Kb/s in"),
            ],
        );

        assert_eq!(
            socket_hooks.warnings(),
            vec![io::ErrorKind::InvalidInput, io::ErrorKind::Other]
        );

        socket_hooks.assert_empty();
    }

    #[test]
    fn fake_server_hooks_starts_with_empty_queues() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_server_assert_empty_hooks_fails_with_non_empty_accept() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_accept(Ok(()));
        server_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_server_assert_empty_hooks_fails_with_non_empty_read() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_read(Ok(Ok(0)));
        server_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_server_assert_empty_hooks_fails_with_non_empty_write() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_write(Ok(()));
        server_hooks.assert_empty();
    }

    #[test]
    #[should_panic]
    fn fake_server_assert_empty_hooks_fails_with_non_empty_shutdown() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_shutdown(Ok(()));
        server_hooks.assert_empty();
    }

    #[test]
    #[should_panic = "unexpected `accept`"]
    fn fake_server_hooks_requires_queued_accept_to_invoke_accept() {
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(FakeServerHooks::new().accept());
    }

    #[test]
    #[should_panic = "unexpected `read`"]
    fn fake_server_hooks_requires_queued_read_to_invoke_read() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_accept(Ok(()));
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(run_future(server_hooks.accept()).unwrap().read());
    }

    #[test]
    #[should_panic = "unexpected `write`"]
    fn fake_server_hooks_requires_queued_write_to_invoke_write() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_accept(Ok(()));
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(run_future(server_hooks.accept()).unwrap().write());
    }

    #[test]
    #[should_panic = "unexpected `shutdown`"]
    fn fake_server_hooks_requires_queued_shutdown_to_invoke_shutdown() {
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_accept(Ok(()));
        // This is where it should panic. The error result shouldn't matter, as it shouldn't be
        // returning in the first place.
        let _ = run_future(run_future(server_hooks.accept()).unwrap().shutdown());
    }

    #[test]
    fn fake_server_hooks_runs_hooks_correctly() {
        // Now, unwrap so I can use it a bit more easily.
        let server_hooks = FakeServerHooks::new();
        server_hooks.push_accept(Ok(()));

        {
            let mut socket_hooks = run_future(server_hooks.accept()).unwrap();

            socket_hooks.push_read(Ok(Ok(0)));
            socket_hooks.push_read(Ok(Ok(123)));
            socket_hooks.stats().add_connection();
            socket_hooks.stats().push_received_bytes(1000);
            assert_matches!(run_future(socket_hooks.read()), Ok(Ok(0)));

            socket_hooks.push_write(Ok(()));
            socket_hooks.push_write(Ok(()));
            socket_hooks.stats().push_received_bytes(1000);
            assert_matches!(run_future(socket_hooks.write()), Ok(()));

            socket_hooks.emit_warning(io::ErrorKind::InvalidInput.into());

            socket_hooks.push_shutdown(Ok(()));
            socket_hooks.push_shutdown(Ok(()));
            socket_hooks.stats().push_received_bytes(1000);
            socket_hooks.stats().remove_connection();
            assert_matches!(run_future(socket_hooks.shutdown()), Ok(()));

            socket_hooks.stats().add_connection();
            socket_hooks.stats().push_received_bytes(1000);
            assert_matches!(run_future(socket_hooks.read()), Ok(Ok(123)));
            assert_matches!(run_future(socket_hooks.write()), Ok(()));
            socket_hooks.stats().push_received_bytes(1000);
            socket_hooks.stats().remove_connection();
            assert_matches!(run_future(socket_hooks.shutdown()), Ok(()));
            socket_hooks.emit_warning(io::ErrorKind::Other.into());
        }

        assert_eq!(
            server_hooks.snapshots(),
            vec![
                String::from("1 conns, 8 Kb/s in"),
                String::from("1 conns, 8 Kb/s in"),
                String::from("0 conns, 8 Kb/s in"),
                String::from("1 conns, 8 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 8 Kb/s in"),
            ],
        );

        assert_eq!(
            server_hooks.warnings(),
            vec![io::ErrorKind::InvalidInput, io::ErrorKind::Other]
        );

        server_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_handles_immediate_end() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_shutdown(Ok(()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ]
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_handles_some_bytes_pushed() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_shutdown(Ok(()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_handles_tolerable_read_errors() {
        let tolerable_error_kinds = [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::WriteZero,
        ];

        for kind in tolerable_error_kinds {
            let socket_hooks = FakeSocketHooks::new();

            socket_hooks.push_read(Ok(Ok(1234)));
            socket_hooks.push_write(Ok(()));
            socket_hooks.push_read(Ok(Err(kind.into())));
            assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

            assert_eq!(
                socket_hooks.snapshots(),
                vec![
                    String::from("1 conns, 0 Kb/s in"),
                    String::from("1 conns, 9 Kb/s in"),
                    String::from("1 conns, 0 Kb/s in"),
                ],
            );

            assert_eq!(socket_hooks.warnings(), vec![]);
            socket_hooks.assert_empty();
        }
    }

    #[test]
    fn socket_handler_handles_timeout() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Err(()));
        socket_hooks.push_shutdown(Ok(()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_retries_read_on_would_block() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Err(io::ErrorKind::WouldBlock.into())));
        socket_hooks.push_read(Ok(Err(io::ErrorKind::WouldBlock.into())));
        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Ok(Err(io::ErrorKind::WouldBlock.into())));
        socket_hooks.push_read(Ok(Err(io::ErrorKind::WouldBlock.into())));
        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_shutdown(Ok(()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_retries_write_on_would_block() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Err(io::ErrorKind::WouldBlock.into()));
        socket_hooks.push_write(Err(io::ErrorKind::WouldBlock.into()));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_shutdown(Ok(()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_handles_fatal_read_errors() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Ok(Err(io::ErrorKind::Other.into())));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![io::ErrorKind::Other]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_handles_fatal_write_errors() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Err(io::ErrorKind::Other.into()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![io::ErrorKind::Other]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_ignores_closedish_shutdown_errors() {
        let tolerable_error_kinds = [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::WriteZero,
        ];

        for kind in tolerable_error_kinds {
            let socket_hooks = FakeSocketHooks::new();

            socket_hooks.push_read(Ok(Ok(1234)));
            socket_hooks.push_write(Ok(()));
            socket_hooks.push_read(Ok(Ok(0)));
            socket_hooks.push_shutdown(Err(kind.into()));
            assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

            assert_eq!(
                socket_hooks.snapshots(),
                vec![
                    String::from("1 conns, 0 Kb/s in"),
                    String::from("1 conns, 9 Kb/s in"),
                    String::from("1 conns, 0 Kb/s in"),
                    String::from("0 conns, 0 Kb/s in"),
                ],
            );

            assert_eq!(socket_hooks.warnings(), vec![]);
            socket_hooks.assert_empty();
        }
    }

    #[test]
    fn socket_handler_retries_shutdown_on_would_block() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_shutdown(Err(io::ErrorKind::WouldBlock.into()));
        socket_hooks.push_shutdown(Err(io::ErrorKind::WouldBlock.into()));
        socket_hooks.push_shutdown(Ok(()));
        assert_matches!(run_future(socket_handler(socket_hooks.clone())), Ok(()));

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn socket_handler_crashes_on_fatal_shutdown_errors() {
        let socket_hooks = FakeSocketHooks::new();

        socket_hooks.push_read(Ok(Ok(1234)));
        socket_hooks.push_write(Ok(()));
        socket_hooks.push_read(Ok(Ok(0)));
        socket_hooks.push_shutdown(Err(io::ErrorKind::Other.into()));
        assert_matches!(
            run_future(socket_handler(socket_hooks.clone())),
            Err(e) if e.kind() == io::ErrorKind::Other
        );

        assert_eq!(
            socket_hooks.snapshots(),
            vec![
                String::from("1 conns, 0 Kb/s in"),
                String::from("1 conns, 9 Kb/s in"),
                String::from("1 conns, 0 Kb/s in"),
                String::from("0 conns, 0 Kb/s in"),
            ],
        );

        assert_eq!(socket_hooks.warnings(), vec![]);
        socket_hooks.assert_empty();
    }

    #[test]
    fn assert_once_accepts_a_request_successfully() {
        let server_hooks = FakeServerHooks::new();

        server_hooks.push_accept(Ok(()));
        assert_matches!(run_future(accept_once(&server_hooks)), Ok(_));

        assert_eq!(server_hooks.snapshots(), vec![] as Vec<String>);
        assert_eq!(server_hooks.warnings(), vec![]);
        assert_eq!(
            server_hooks.stats().get_snapshot(Duration::from_millis(1)),
            Some(String::from("0 conns, 0 Kb/s in"))
        );
    }

    #[test]
    fn assert_once_retries_on_tolerable_error() {
        let server_hooks = FakeServerHooks::new();

        server_hooks.push_accept(Err(io::ErrorKind::ConnectionReset.into()));
        server_hooks.push_accept(Err(io::ErrorKind::ConnectionAborted.into()));
        server_hooks.push_accept(Err(io::ErrorKind::WouldBlock.into()));
        server_hooks.push_accept(Ok(()));
        assert_matches!(run_future(accept_once(&server_hooks)), Ok(_));

        assert_eq!(server_hooks.snapshots(), vec![] as Vec<String>);
        assert_eq!(server_hooks.warnings(), vec![]);
        assert_eq!(
            server_hooks.stats().get_snapshot(Duration::from_millis(1)),
            Some(String::from("0 conns, 0 Kb/s in"))
        );
        server_hooks.assert_empty();
    }

    #[test]
    fn assert_once_propagates_fatal_errors() {
        let server_hooks = FakeServerHooks::new();

        server_hooks.push_accept(Err(io::ErrorKind::Other.into()));
        assert_matches!(run_future(accept_once(&server_hooks)), Err(e) if e.kind() == io::ErrorKind::Other);

        assert_eq!(server_hooks.snapshots(), vec![] as Vec<String>);
        assert_eq!(server_hooks.warnings(), vec![]);
        assert_eq!(
            server_hooks.stats().get_snapshot(Duration::from_millis(1)),
            Some(String::from("0 conns, 0 Kb/s in"))
        );
        server_hooks.assert_empty();
    }

    macro_rules! cfg_monotonic_timer_exists {
        ( $( $item:item )* ) => {
            $(
                // Disable on macOS and Windows as they lack true monotonic clocks. See related
                // comment in `.github/workflows/ci.yml`.
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                $item
            )*
        };
    }

    cfg_monotonic_timer_exists! {
        // Try to avoid Mac's lack of a true monotonic timer. Offloading the actual number to the
        // workflow config, with the (relatively fast) preferred default here.
        fn timer_step_millis() -> Duration {
            Duration::from_millis(100)
        }

        fn timer_offset_millis() -> Duration {
            timer_step_millis() / 2
        }

        fn bps(n: usize) -> usize {
            n * (timer_step_millis().as_millis() as usize) / 1000
        }

        #[tokio::test]
        async fn timer_loop_prints_continuous_zeroes() {
            let server_hooks = Arc::new(FakeServerHooks::new());

            let task = tokio::spawn(timer_loop(server_hooks.clone(), timer_step_millis()));

            tokio::time::sleep(timer_step_millis() * 5 + timer_offset_millis()).await;
            task.abort();

            assert_eq!(
                server_hooks.snapshots(),
                vec![
                    String::from("0 conns, 0 Kb/s in"),
                    String::from("0 conns, 0 Kb/s in"),
                    String::from("0 conns, 0 Kb/s in"),
                    String::from("0 conns, 0 Kb/s in"),
                    String::from("0 conns, 0 Kb/s in"),
                ]
            );
            assert_eq!(server_hooks.warnings(), vec![]);
            assert_eq!(
                server_hooks.stats().get_snapshot(Duration::from_millis(1)),
                Some(String::from("0 conns, 0 Kb/s in"))
            );

            // FIXME: This doesn't type-check, due to the inner timer futures holding cells (which
            // make it not unwind-safe).
            // ```
            // with_attempts(3, || async {
            //     let server_hooks = Arc::new(FakeServerHooks::new());
            //
            //     let task = tokio::spawn(timer_loop(server_hooks.clone(), timer_step_millis()));
            //
            //     tokio::time::sleep(timer_step_millis() * 5 + timer_offset_millis()).await;
            //     task.abort();
            //
            //     assert_eq!(
            //         server_hooks.snapshots(),
            //         vec![
            //             String::from("0 conns, 0 Kb/s in"),
            //             String::from("0 conns, 0 Kb/s in"),
            //             String::from("0 conns, 0 Kb/s in"),
            //             String::from("0 conns, 0 Kb/s in"),
            //             String::from("0 conns, 0 Kb/s in"),
            //         ]
            //     );
            //     assert_eq!(server_hooks.warnings(), vec![]);
            //     assert_eq!(
            //         server_hooks.stats().get_snapshot(Duration::from_millis(1)),
            //         Some(String::from("0 conns, 0 Kb/s in"))
            //     );
            // })
            // .await
            // ```
        }

        #[tokio::test]
        async fn timer_loop_reacts_to_connection_changes() {
            let server_hooks = Arc::new(FakeServerHooks::new());

            let task = tokio::spawn(timer_loop(server_hooks.clone(), timer_step_millis()));

            tokio::time::sleep(timer_step_millis() + timer_offset_millis()).await;
            server_hooks.stats().add_connection();
            tokio::time::sleep(timer_step_millis()).await;
            server_hooks.stats().push_received_bytes(bps(2222));
            tokio::time::sleep(timer_step_millis()).await;
            server_hooks.stats().push_received_bytes(bps(1111));
            server_hooks.stats().remove_connection();
            tokio::time::sleep(timer_step_millis()).await;
            tokio::time::sleep(timer_step_millis()).await;
            task.abort();

            assert_eq!(
                server_hooks.snapshots(),
                vec![
                    String::from("0 conns, 0 Kb/s in"),
                    String::from("1 conns, 0 Kb/s in"),
                    String::from("1 conns, 17 Kb/s in"),
                    String::from("0 conns, 8 Kb/s in"),
                    String::from("0 conns, 0 Kb/s in"),
                ]
            );
            assert_eq!(server_hooks.warnings(), vec![]);
            assert_eq!(
                server_hooks.stats().get_snapshot(Duration::from_millis(1)),
                Some(String::from("0 conns, 0 Kb/s in"))
            );

            // FIXME: This doesn't type-check, due to the inner timer futures holding cells (which
            // make it not unwind-safe).
            // ```
            // with_attempts(3, || async {
            //     let server_hooks = Arc::new(FakeServerHooks::new());
            //
            //     let task = tokio::spawn(timer_loop(server_hooks.clone(), timer_step_millis()));
            //
            //     tokio::time::sleep(timer_step_millis() + timer_offset_millis()).await;
            //     server_hooks.stats().add_connection();
            //     tokio::time::sleep(timer_step_millis()).await;
            //     server_hooks.stats().push_received_bytes(bps(321));
            //     tokio::time::sleep(timer_step_millis()).await;
            //     server_hooks.stats().push_received_bytes(bps(123));
            //     server_hooks.stats().remove_connection();
            //     tokio::time::sleep(timer_step_millis()).await;
            //     tokio::time::sleep(timer_step_millis()).await;
            //     task.abort();
            //
            //     assert_eq!(
            //         server_hooks.snapshots(),
            //         vec![
            //             String::from("0 conns, 0 Kb/s in"),
            //             String::from("1 conns, 0 Kb/s in"),
            //             String::from("1 conns, 12 Kb/s in"),
            //             String::from("0 conns, 4 Kb/s in"),
            //             String::from("0 conns, 0 Kb/s in"),
            //         ]
            //     );
            //     assert_eq!(server_hooks.warnings(), vec![]);
            //     assert_eq!(
            //         server_hooks.stats().get_snapshot(Duration::from_millis(1)),
            //         Some(String::from("0 conns, 0 Kb/s in"))
            //     );
            // })
            // .await
            // ```
        }
    }
}
