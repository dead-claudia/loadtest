use std::{
    collections::VecDeque,
    future::{Future, IntoFuture},
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use crossbeam_utils::atomic::AtomicCell;

pub fn assert_panics<F: FnOnce() -> R + std::panic::UnwindSafe, R>(f: F, reason: &str) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(f);
    std::panic::set_hook(prev);

    match result {
        Ok(_) => panic!("assertion failed: expected panic with reason '{}'", reason),
        Err(e) => match e.downcast_ref::<&str>() {
            Some(&actual) if actual == reason => {}
            Some(&actual) => panic!(
                "assertion failed: expected panic with reason '{}', but found reason '{}'",
                reason, actual
            ),
            None => match e.downcast_ref::<String>() {
                Some(actual) if actual == reason => {}
                Some(actual) => panic!(
                    "assertion failed: expected panic with reason '{}', but found reason '{}'",
                    reason, actual
                ),
                None => std::panic::resume_unwind(e),
            },
        },
    }
}

// Nasty, but works.
pub struct StateSpy<S: Copy + Default, R, V: Clone> {
    _updates: Counter,
    _value: AtomicCell<S>,
    _transition: fn(S, V) -> (S, R),
}

impl<S: Copy + Eq + Default, R, V: Clone> StateSpy<S, R, V> {
    pub fn new(transition: fn(S, V) -> (S, R)) -> StateSpy<S, R, V> {
        StateSpy {
            _updates: Counter::new(),
            _value: AtomicCell::default(),
            _transition: transition,
        }
    }

    pub fn updates(&self) -> usize {
        self._updates.current()
    }

    pub fn current(&self) -> S {
        self._value.load()
    }

    pub fn advance(&self, value: V) -> R {
        let mut prev = self._value.load();
        let result = loop {
            let (next, result) = (self._transition)(prev, value.clone());
            match self._value.compare_exchange(prev, next) {
                Ok(_) => break result,
                Err(value) if value == next => break result,
                Err(value) => prev = value,
            };
        };
        self._updates.tick();
        result
    }
}

pub fn new_waker() -> Waker {
    unsafe { Waker::from_raw(clone(std::ptr::null())) }
}

pub fn new_context(waker: &Waker) -> Context {
    Context::from_waker(waker)
}

unsafe fn clone(ptr: *const ()) -> RawWaker {
    RawWaker::new(ptr, &VTABLE)
}

unsafe fn ignore(_: *const ()) {}

const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, ignore, ignore, ignore);

#[derive(Debug, Clone, Default)]
pub struct Counter(Arc<AtomicUsize>);

impl Counter {
    pub fn new() -> Counter {
        Counter::default()
    }

    pub fn current(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    pub fn tick(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub struct CallSpy<T> {
    _calls: Counter,
    _queue: std::sync::Mutex<VecDeque<T>>,
    _error_msg: String,
}

impl<T> CallSpy<T> {
    pub fn new<I: Into<String>>(error_message: I) -> CallSpy<T> {
        CallSpy {
            _calls: Counter::new(),
            _queue: std::sync::Mutex::new(VecDeque::new()),
            _error_msg: error_message.into(),
        }
    }

    #[allow(unused)]
    pub fn calls(&self) -> usize {
        return self._calls.current();
    }

    pub fn remaining(&self) -> usize {
        return self._queue.lock().unwrap().len();
    }

    pub fn push(&self, value: T) {
        self._queue.lock().unwrap().push_back(value);
    }

    pub fn invoke(&self) -> T {
        let value = {
            let mut lock = self._queue.lock().unwrap();
            lock.pop_front().expect(&self._error_msg)
        };
        self._calls.tick();
        value
    }
}

#[derive(Debug)]
pub enum Push {
    Write(Poll<std::io::Result<usize>>),
    Flush(Poll<std::io::Result<()>>),
    Shutdown(Poll<std::io::Result<()>>),
}

pub struct TestStream {
    _polls: Counter,
    _result_write: VecDeque<Poll<std::io::Result<usize>>>,
    _result_flush: VecDeque<Poll<std::io::Result<()>>>,
    _result_shutdown: VecDeque<Poll<std::io::Result<()>>>,
}

impl TestStream {
    pub fn new(polls: Counter) -> TestStream {
        TestStream {
            _polls: polls,
            _result_write: VecDeque::new(),
            _result_flush: VecDeque::new(),
            _result_shutdown: VecDeque::new(),
        }
    }

    pub fn push(&mut self, value: Push) {
        match value {
            Push::Write(value) => self._result_write.push_back(value),
            Push::Flush(value) => self._result_flush.push_back(value),
            Push::Shutdown(value) => self._result_shutdown.push_back(value),
        }
    }
}

impl Extend<Push> for TestStream {
    fn extend<T: IntoIterator<Item = Push>>(&mut self, iter: T) {
        for item in iter {
            self.push(item);
        }
    }
}

impl tokio::io::AsyncWrite for TestStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self._polls.tick();
        self._result_write
            .pop_front()
            .expect("Unexpected `poll_write`")
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self._polls.tick();
        self._result_flush
            .pop_front()
            .expect("Unexpected `poll_flush`")
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self._polls.tick();
        self._result_shutdown
            .pop_front()
            .expect("Unexpected `poll_shutdown`")
    }
}

pub fn run_future<F: IntoFuture<Output = R>, R>(fut: F) -> R {
    let mut fut = Box::pin(fut.into_future());
    let waker = new_waker();
    let mut cx = new_context(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(result) => return result,
            Poll::Pending => {}
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use matches::*;
    use std::cell::RefCell;
    use tokio::io::AsyncWrite;

    #[test]
    fn assert_panics_does_not_panic_on_inner_matching_panic_literal() {
        assert_panics(|| panic!("test"), "test")
    }

    #[test]
    fn assert_panics_does_not_panic_on_inner_matching_panic_alloc() {
        let string = String::from("e");
        assert_panics(|| panic!("t{}st", string), "test")
    }

    #[test]
    #[should_panic]
    fn assert_panics_panics_on_inner_matching_panic_literal() {
        assert_panics(|| panic!("test"), "not test")
    }

    #[test]
    #[should_panic]
    fn assert_panics_panics_on_inner_matching_panic_alloc() {
        let string = String::from("e");
        assert_panics(|| panic!("t{}st", string), "not test")
    }

    #[test]
    #[should_panic]
    fn assert_panics_panics_on_no_panic() {
        assert_panics(|| (), "not test")
    }

    #[test]
    fn state_spy_counts_updates_correctly_with_cycling_state() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        enum State {
            #[default]
            State1,
            State2,
            State3,
        }

        let spy = StateSpy::new(|s, v| match s {
            State::State1 => (State::State2, 123 + v),
            State::State2 => (State::State3, 456 + v),
            State::State3 => (State::State1, 789 + v),
        });

        assert_eq!(spy.updates(), 0);
        assert_eq!(spy.current(), State::State1);

        assert_eq!(spy.advance(10), 133);
        assert_eq!(spy.updates(), 1);
        assert_eq!(spy.current(), State::State2);

        assert_eq!(spy.advance(20), 476);
        assert_eq!(spy.updates(), 2);
        assert_eq!(spy.current(), State::State3);

        assert_eq!(spy.advance(30), 819);
        assert_eq!(spy.updates(), 3);
        assert_eq!(spy.current(), State::State1);

        assert_eq!(spy.advance(40), 163);
        assert_eq!(spy.updates(), 4);
        assert_eq!(spy.current(), State::State2);
    }

    #[test]
    fn state_spy_counts_updates_correctly_with_repeating_end_state() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        enum State {
            #[default]
            State1,
            State2,
            State3,
        }

        let spy = StateSpy::new(|s, v| match s {
            State::State1 => (State::State2, 123 + v),
            State::State2 => (State::State3, 456 + v),
            State::State3 => (State::State3, 789 + v),
        });

        assert_eq!(spy.updates(), 0);
        assert_eq!(spy.current(), State::State1);

        assert_eq!(spy.advance(10), 133);
        assert_eq!(spy.updates(), 1);
        assert_eq!(spy.current(), State::State2);

        assert_eq!(spy.advance(20), 476);
        assert_eq!(spy.updates(), 2);
        assert_eq!(spy.current(), State::State3);

        assert_eq!(spy.advance(30), 819);
        assert_eq!(spy.updates(), 3);
        assert_eq!(spy.current(), State::State3);

        assert_eq!(spy.advance(40), 829);
        assert_eq!(spy.updates(), 4);
        assert_eq!(spy.current(), State::State3);
    }

    #[test]
    fn waker_and_context_work() {
        // This is entirely a type-checking thing.
        use std::future::Future;

        let waker = new_waker();
        let mut context = new_context(&waker);

        let mut fut = std::future::ready(());

        match std::pin::Pin::new(&mut fut).poll(&mut context) {
            std::task::Poll::Ready(_) => { /* ignore */ }
            std::task::Poll::Pending => { /* ignore */ }
        }
    }

    // Simplifies the panic checks.
    struct State<'a>(std::sync::Mutex<RefCell<(TestStream, Context<'a>)>>);

    impl<'a> State<'a> {
        fn guard(stream: TestStream, context: Context<'a>) -> State<'a> {
            State(std::sync::Mutex::new(RefCell::new((stream, context))))
        }

        fn view<F, R>(&self, f: F) -> R
        where
            F: FnOnce(Pin<&mut TestStream>, &mut Context<'a>) -> R,
        {
            use std::ops::DerefMut;
            let lock = self.0.lock().unwrap();
            let mut borrowed = lock.borrow_mut();
            let s = borrowed.deref_mut();
            f(Pin::new(&mut s.0), &mut s.1)
        }
    }

    macro_rules! setup {
        ($stream:ident, $polls:ident, $context:ident) => {
            let waker = new_waker();
            #[allow(unused_mut)]
            let mut $context = new_context(&waker);
            #[allow(unused_mut)]
            let mut $polls = Counter::new();
            #[allow(unused_mut)]
            let mut $stream = TestStream::new($polls.clone());
        };
    }

    #[test]
    fn allows_pushing_pending_write() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Write(Poll::Pending));
        assert_eq!(polls.current(), 0);
        assert_matches!(
            Pin::new(&mut stream).poll_write(&mut context, &[0; 16]),
            Poll::Pending
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn allows_pushing_ok_write() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Write(Poll::Ready(Ok(123))));
        assert_eq!(polls.current(), 0);
        assert_matches!(
            Pin::new(&mut stream).poll_write(&mut context, &[0; 16]),
            Poll::Ready(Ok(123))
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn allows_pushing_error_write() {
        let expected = || Poll::Ready(Err(std::io::Error::from_raw_os_error(123)));

        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Write(expected()));
        assert_eq!(polls.current(), 0);
        match Pin::new(&mut stream).poll_write(&mut context, &[0; 16]) {
            Poll::Ready(Err(e)) if e.raw_os_error() == Some(123) => {}
            result => panic!(
                "assertion failed: `{:?}` does not match `{:?}`",
                result,
                expected()
            ),
        };
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn disallows_pushing_error_write_from_start() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        let state = State::guard(stream, context);
        assert_eq!(polls.current(), 0);
        assert_panics(
            || state.view(|stream, cx| stream.poll_write(cx, &[0; 16])),
            "Unexpected `poll_write`",
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn disallows_pushing_error_write_after_success() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Write(Poll::Pending));
        assert_eq!(polls.current(), 0);
        let _ = Pin::new(&mut stream).poll_write(&mut context, &[0; 16]);
        assert_eq!(polls.current(), 1);
        let state = State::guard(stream, context);
        assert_eq!(polls.current(), 1);
        assert_panics(
            || state.view(|stream, cx| stream.poll_write(cx, &[0; 16])),
            "Unexpected `poll_write`",
        );
        assert_eq!(polls.current(), 2);
    }

    #[test]
    fn allows_pushing_pending_flush() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Flush(Poll::Pending));
        assert_eq!(polls.current(), 0);
        assert_matches!(
            Pin::new(&mut stream).poll_flush(&mut context),
            Poll::Pending
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn allows_pushing_ok_flush() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Flush(Poll::Ready(Ok(()))));
        assert_eq!(polls.current(), 0);
        assert_matches!(
            Pin::new(&mut stream).poll_flush(&mut context),
            Poll::Ready(Ok(()))
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn allows_pushing_error_flush() {
        let expected = || Poll::Ready(Err(std::io::Error::from_raw_os_error(123)));

        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Flush(expected()));
        assert_eq!(polls.current(), 0);
        match Pin::new(&mut stream).poll_flush(&mut context) {
            Poll::Ready(Err(e)) if e.raw_os_error() == Some(123) => {}
            result => panic!(
                "assertion failed: `{:?}` does not match `{:?}`",
                result,
                expected()
            ),
        };
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn disallows_pushing_error_flush_from_start() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        let state = State::guard(stream, context);
        assert_eq!(polls.current(), 0);
        assert_panics(
            || state.view(|stream, cx| stream.poll_flush(cx)),
            "Unexpected `poll_flush`",
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn disallows_pushing_error_flush_after_success() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Flush(Poll::Pending));
        assert_eq!(polls.current(), 0);
        let _ = Pin::new(&mut stream).poll_flush(&mut context);
        assert_eq!(polls.current(), 1);
        let state = State::guard(stream, context);
        assert_eq!(polls.current(), 1);
        assert_panics(
            || state.view(|stream, cx| stream.poll_flush(cx)),
            "Unexpected `poll_flush`",
        );
        assert_eq!(polls.current(), 2);
    }

    #[test]
    fn allows_pushing_pending_shutdown() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Shutdown(Poll::Pending));
        assert_eq!(polls.current(), 0);
        assert_matches!(
            Pin::new(&mut stream).poll_shutdown(&mut context),
            Poll::Pending
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn allows_pushing_ok_shutdown() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Shutdown(Poll::Ready(Ok(()))));
        assert_eq!(polls.current(), 0);
        assert_matches!(
            Pin::new(&mut stream).poll_shutdown(&mut context),
            Poll::Ready(Ok(()))
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn allows_pushing_error_shutdown() {
        let expected = || Poll::Ready(Err(std::io::Error::from_raw_os_error(123)));

        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Shutdown(expected()));
        assert_eq!(polls.current(), 0);
        match Pin::new(&mut stream).poll_shutdown(&mut context) {
            Poll::Ready(Err(e)) if e.raw_os_error() == Some(123) => {}
            result => panic!(
                "assertion failed: `{:?}` does not match `{:?}`",
                result,
                expected()
            ),
        };
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn disallows_pushing_error_shutdown_from_start() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        let state = State::guard(stream, context);
        assert_eq!(polls.current(), 0);
        assert_panics(
            || state.view(|stream, cx| stream.poll_shutdown(cx)),
            "Unexpected `poll_shutdown`",
        );
        assert_eq!(polls.current(), 1);
    }

    #[test]
    fn disallows_pushing_error_shutdown_after_success() {
        setup!(stream, polls, context);
        assert_eq!(polls.current(), 0);
        stream.push(Push::Shutdown(Poll::Pending));
        assert_eq!(polls.current(), 0);
        let _ = Pin::new(&mut stream).poll_shutdown(&mut context);
        assert_eq!(polls.current(), 1);
        let state = State::guard(stream, context);
        assert_eq!(polls.current(), 1);
        assert_panics(
            || state.view(|stream, cx| stream.poll_shutdown(cx)),
            "Unexpected `poll_shutdown`",
        );
        assert_eq!(polls.current(), 2);
    }

    struct TestFut {
        wait: i32,
        value: i32,
    }

    impl Future for TestFut {
        type Output = i32;
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            if this.wait == 0 {
                Poll::Ready(this.value)
            } else {
                this.wait -= 1;
                Poll::Pending
            }
        }
    }

    #[test]
    fn run_future_can_immediately_return() {
        let fut = TestFut {
            wait: 0,
            value: 123,
        };

        assert_eq!(run_future(fut), 123)
    }

    #[test]
    fn run_future_can_return_after_one_pending() {
        let fut = TestFut {
            wait: 1,
            value: 123,
        };

        assert_eq!(run_future(fut), 123)
    }

    #[test]
    fn run_future_can_return_after_many_pendings() {
        let fut = TestFut {
            wait: 5,
            value: 123,
        };

        assert_eq!(run_future(fut), 123)
    }
}
