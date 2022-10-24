use std::{
    mem::MaybeUninit,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{self, AsyncRead, ReadBuf},
    net::TcpStream,
    time::{sleep, Duration},
};

struct RepeatZero;

// Don't use `io::repeat(0)` as I wan't something faster and non-iterative.
impl AsyncRead for RepeatZero {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        unsafe {
            buf.unfilled_mut().fill(MaybeUninit::new(0));
            buf.assume_init(buf.capacity());
            buf.set_filled(buf.capacity());
        }
        Poll::Ready(Ok(()))
    }
}

#[async_trait]
trait ConnectHooks {
    type Listener: io::AsyncWrite + Unpin;
    async fn connect(&self) -> io::Result<Self::Listener>;
    async fn wait(&self);
}

async fn client_socket_loop<Hooks: ConnectHooks>(hooks: Hooks) -> io::Result<()> {
    loop {
        match hooks.connect().await {
            Ok(mut listener) => loop {
                match io::copy(&mut RepeatZero, &mut listener).await {
                    Ok(_) => {}
                    Err(e) => match e.kind() {
                        io::ErrorKind::ConnectionReset => return Ok(()),
                        io::ErrorKind::ConnectionRefused => break,
                        io::ErrorKind::BrokenPipe => break,
                        io::ErrorKind::ConnectionAborted => break,
                        io::ErrorKind::TimedOut => break,
                        io::ErrorKind::WouldBlock => {}
                        io::ErrorKind::NotConnected => break,
                        _ => return Err(e),
                    },
                }
            },
            Err(e) => match e.kind() {
                io::ErrorKind::BrokenPipe => {}
                io::ErrorKind::ConnectionAborted => {}
                io::ErrorKind::TimedOut => {}
                io::ErrorKind::WouldBlock => {}
                io::ErrorKind::NotConnected => {}
                io::ErrorKind::AlreadyExists => {}
                _ => return Err(e),
            },
        };

        hooks.wait().await;
    }
}

// Split for clarity
struct NativeConnectHooks(std::net::SocketAddr);

#[async_trait]
impl ConnectHooks for NativeConnectHooks {
    type Listener = TcpStream;

    async fn connect(&self) -> io::Result<TcpStream> {
        TcpStream::connect(self.0).await
    }

    async fn wait(&self) {
        sleep(Duration::from_millis(250)).await
    }
}

pub async fn client_loop(options: crate::cli_args::TcpClientOptions) -> anyhow::Result<()> {
    let mut handles = vec![];

    for _ in 0..options.connections.into() {
        handles.push(tokio::spawn(client_socket_loop(NativeConnectHooks(
            options.target,
        ))));
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

#[cfg(test)]
mod test {
    // CAUTION: these are generally written as state machines driving a loop. There must be an end
    // state, or the loop will continue on forever.
    //
    // Also, for inner state in the client loop, use cells so the actual object being tested
    // doesn't need to accept a mutable reference (which gets annoying fast with the borrow
    // checker.)

    use super::*;
    use crate::test_utils::*;
    use std::mem::MaybeUninit;
    use tokio::io::AsyncRead;

    #[test]
    fn repeat_zero_works() {
        let mut repeat_zero = RepeatZero;

        let waker = new_waker();
        let mut context = new_context(&waker);

        let mut byte_buf = [MaybeUninit::uninit(); 16];
        let mut buf = tokio::io::ReadBuf::uninit(&mut byte_buf[..]);

        assert_eq!(buf.filled().len(), 0);
        assert_eq!(buf.initialized().len(), 0);
        assert_eq!(buf.capacity(), 16);

        let repeat_zero = Pin::new(&mut repeat_zero);

        assert_matches!(
            RepeatZero::poll_read(repeat_zero, &mut context, &mut buf),
            Poll::Ready(Ok(()))
        );

        assert_eq!(buf.filled().len(), buf.capacity());
        assert_eq!(buf.initialized().len(), buf.capacity());
    }

    // Get rid of this boilerplate.
    type ConnectResult = Result<Vec<Push>, io::ErrorKind>;

    struct RunLoopResult<S: Copy + Default + Send> {
        result: io::Result<()>,
        state_spy: StateSpy<S, ConnectResult, ()>,
        wait_calls: Counter,
        stream_polls: Counter,
    }

    fn run_loop<S>(transition: fn(S, ()) -> (S, ConnectResult)) -> RunLoopResult<S>
    where
        S: Copy + Eq + Default + Send,
    {
        struct TestHooks<S: Copy + Default + Send> {
            state_spy: StateSpy<S, ConnectResult, ()>,
            stream_polls: Counter,
            wait_calls: Counter,
        }

        #[async_trait]
        impl<S: Copy + Eq + Default + Send> ConnectHooks for &TestHooks<S> {
            type Listener = TestStream;

            async fn connect(&self) -> io::Result<TestStream> {
                match self.state_spy.advance(()) {
                    Ok(push) => Ok({
                        let mut stream = TestStream::new(self.stream_polls.clone());
                        stream.extend(push);
                        stream
                    }),
                    Err(kind) => Err(io::Error::from(kind)),
                }
            }

            async fn wait(&self) {
                self.wait_calls.tick();
            }
        }

        let hooks = TestHooks {
            state_spy: StateSpy::new(transition),
            stream_polls: Counter::new(),
            wait_calls: Counter::new(),
        };

        let result = run_future(client_socket_loop(&hooks));

        RunLoopResult {
            result,
            state_spy: hooks.state_spy,
            wait_calls: hooks.wait_calls,
            stream_polls: hooks.stream_polls,
        }
    }

    #[test]
    fn client_loop_processes_immediate_fatal_connect_failure() {
        let r = run_loop(|(), ()| ((), Err(io::ErrorKind::PermissionDenied)));

        assert_matches!(r.result, Err(e) if e.kind() == io::ErrorKind::PermissionDenied);

        assert_eq!(r.state_spy.updates(), 1);
        // assert_eq!(r.state_spy.current(), ());
        assert_eq!(r.wait_calls.current(), 0);
        assert_eq!(r.stream_polls.current(), 0);
    }

    #[test]
    fn client_loop_processes_tolerable_connect_failures() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        enum State {
            #[default]
            Delay1,
            Delay2,
            Delay3,
            Delay4,
            Delay5,
            Delay6,
            Deny,
        }

        let r = run_loop(|s, ()| match s {
            State::Delay1 => (State::Delay2, Err(io::ErrorKind::BrokenPipe)),
            State::Delay2 => (State::Delay3, Err(io::ErrorKind::ConnectionAborted)),
            State::Delay3 => (State::Delay4, Err(io::ErrorKind::TimedOut)),
            State::Delay4 => (State::Delay5, Err(io::ErrorKind::WouldBlock)),
            State::Delay5 => (State::Delay6, Err(io::ErrorKind::NotConnected)),
            State::Delay6 => (State::Deny, Err(io::ErrorKind::AlreadyExists)),
            State::Deny => (State::Deny, Err(io::ErrorKind::PermissionDenied)),
        });

        assert_matches!(r.result, Err(e) if e.kind() == io::ErrorKind::PermissionDenied);

        assert_eq!(r.state_spy.updates(), 7);
        assert_eq!(r.state_spy.current(), State::Deny);
        assert_eq!(r.wait_calls.current(), 6);
        assert_eq!(r.stream_polls.current(), 0);
    }

    #[test]
    fn client_loop_processes_single_connection_with_no_data() {
        let r = run_loop(|(), ()| {
            (
                (),
                Ok(vec![
                    Push::Write(Poll::Pending),
                    Push::Write(Poll::Ready(Err(io::Error::from(io::ErrorKind::WouldBlock)))),
                    Push::Write(Poll::Ready(Err(io::Error::from(
                        io::ErrorKind::ConnectionReset,
                    )))),
                ]),
            )
        });

        assert_matches!(r.result, Ok(()));

        assert_eq!(r.state_spy.updates(), 1);
        // assert_eq!(r.state_spy.current(), ());
        assert_eq!(r.wait_calls.current(), 0);
        assert_eq!(r.stream_polls.current(), 3);
    }

    #[test]
    fn client_loop_processes_single_connection_with_some_data() {
        let r = run_loop(|(), ()| {
            (
                (),
                Ok(vec![
                    Push::Write(Poll::Pending),
                    Push::Write(Poll::Ready(Err(io::Error::from(io::ErrorKind::WouldBlock)))),
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(
                        io::ErrorKind::ConnectionReset,
                    )))),
                ]),
            )
        });

        assert_matches!(r.result, Ok(()));

        assert_eq!(r.state_spy.updates(), 1);
        // assert_eq!(r.state_spy.current(), ());
        assert_eq!(r.wait_calls.current(), 0);
        assert_eq!(r.stream_polls.current(), 4);
    }

    #[test]
    fn client_loop_processes_single_connection_then_connection_refused() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        enum State {
            #[default]
            Connect,
            Deny,
        }

        let r = run_loop(|s, ()| match s {
            State::Connect => (
                State::Deny,
                Ok(vec![
                    Push::Write(Poll::Pending),
                    Push::Write(Poll::Ready(Err(io::Error::from(io::ErrorKind::WouldBlock)))),
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)))),
                ]),
            ),
            State::Deny => (State::Deny, Err(io::ErrorKind::ConnectionRefused)),
        });

        assert_matches!(r.result, Err(e) if e.kind() == io::ErrorKind::ConnectionRefused);

        assert_eq!(r.state_spy.updates(), 2);
        assert_eq!(r.state_spy.current(), State::Deny);
        assert_eq!(r.wait_calls.current(), 1);
        assert_eq!(r.stream_polls.current(), 4);
    }

    #[test]
    fn client_loop_processes_tolerable_mid_connection_failures() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        enum State {
            #[default]
            Delay1,
            Delay2,
            Delay3,
            Delay4,
            Delay5,
            Deny,
        }

        let r = run_loop(|s, ()| match s {
            State::Delay1 => (
                State::Delay2,
                Ok(vec![
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(
                        io::ErrorKind::ConnectionRefused,
                    )))),
                ]),
            ),
            State::Delay2 => (
                State::Delay3,
                Ok(vec![
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)))),
                ]),
            ),
            State::Delay3 => (
                State::Delay4,
                Ok(vec![
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(
                        io::ErrorKind::ConnectionAborted,
                    )))),
                ]),
            ),
            State::Delay4 => (
                State::Delay5,
                Ok(vec![
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(io::ErrorKind::TimedOut)))),
                ]),
            ),
            State::Delay5 => (
                State::Deny,
                Ok(vec![
                    Push::Write(Poll::Ready(Ok(123))),
                    Push::Write(Poll::Ready(Err(io::Error::from(
                        io::ErrorKind::NotConnected,
                    )))),
                ]),
            ),
            State::Deny => (State::Deny, Err(io::ErrorKind::PermissionDenied)),
        });

        assert_matches!(r.result, Err(e) if e.kind() == io::ErrorKind::PermissionDenied);

        assert_eq!(r.state_spy.updates(), 6);
        assert_eq!(r.state_spy.current(), State::Deny);
        assert_eq!(r.wait_calls.current(), 5);
        assert_eq!(r.stream_polls.current(), 10);
    }
}
