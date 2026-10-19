//! Operations usable through native blocking waits or asynchronous polling.

use std::{
    future::{Future, IntoFuture},
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use nusb::MaybeFuture;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::marker::Send as NonWasmSend;

#[cfg(target_arch = "wasm32")]
pub trait NonWasmSend {}
#[cfg(target_arch = "wasm32")]
impl<T> NonWasmSend for T {}

pub(crate) fn ready<T: NonWasmSend>(value: T) -> impl MaybeFuture<Output = T> {
    Ready(value)
}

struct Ready<T>(T);

impl<T> IntoFuture for Ready<T> {
    type Output = T;
    type IntoFuture = std::future::Ready<T>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.0)
    }
}

impl<T: NonWasmSend> MaybeFuture for Ready<T> {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output {
        self.0
    }
}

pub(crate) trait MaybeFutureExt: MaybeFuture + Sized {
    fn and_then<C, T, U, E, N>(self, continuation: C) -> impl MaybeFuture<Output = Result<U, E>>
    where
        Self: MaybeFuture<Output = Result<T, E>>,
        C: FnOnce(T) -> N + NonWasmSend,
        N: MaybeFuture<Output = Result<U, E>>,
    {
        AndThen {
            wrapped: self,
            continuation,
            next: PhantomData,
        }
    }

    fn continue_with<C, U, N>(self, continuation: C) -> ContinueWith<Self, C, N>
    where
        C: FnOnce(Self::Output) -> N + NonWasmSend,
        N: MaybeFuture<Output = U>,
    {
        ContinueWith {
            wrapped: self,
            continuation,
            next: PhantomData,
        }
    }
}

impl<F: MaybeFuture> MaybeFutureExt for F {}

pub(crate) struct ContinueWith<F, C, N> {
    wrapped: F,
    continuation: C,
    next: PhantomData<fn() -> N>,
}

impl<F, C, N, U> IntoFuture for ContinueWith<F, C, N>
where
    F: MaybeFuture,
    C: FnOnce(F::Output) -> N + NonWasmSend,
    N: MaybeFuture<Output = U>,
{
    type Output = U;
    type IntoFuture = ContinueWithFuture<F::IntoFuture, C, N::IntoFuture>;

    fn into_future(self) -> Self::IntoFuture {
        ContinueWithFuture {
            first: Some(Box::pin(self.wrapped.into_future())),
            continuation: Some(self.continuation),
            second: None,
        }
    }
}

impl<F, C, N, U> MaybeFuture for ContinueWith<F, C, N>
where
    F: MaybeFuture,
    C: FnOnce(F::Output) -> N + NonWasmSend,
    N: MaybeFuture<Output = U>,
{
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output {
        (self.continuation)(self.wrapped.wait()).wait()
    }
}

pub(crate) struct ContinueWithFuture<F, C, N> {
    first: Option<Pin<Box<F>>>,
    continuation: Option<C>,
    second: Option<Pin<Box<N>>>,
}

impl<F, C, N> Unpin for ContinueWithFuture<F, C, N> {}

impl<F, C, N, Next, U> Future for ContinueWithFuture<F, C, N>
where
    F: Future,
    C: FnOnce(F::Output) -> Next,
    Next: IntoFuture<Output = U, IntoFuture = N>,
    N: Future<Output = U>,
{
    type Output = U;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            if let Some(second) = self.second.as_mut() {
                return second.as_mut().poll(cx);
            }

            let first = self.first.as_mut().expect("polled after completion");
            match first.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(value) => {
                    self.first = None;
                    let continuation = self
                        .continuation
                        .take()
                        .expect("continuation missing after first operation completed");
                    self.second = Some(Box::pin(continuation(value).into_future()));
                }
            }
        }
    }
}

struct AndThen<F, C, N> {
    wrapped: F,
    continuation: C,
    next: PhantomData<fn() -> N>,
}

impl<F, C, N, T, U, E> IntoFuture for AndThen<F, C, N>
where
    F: MaybeFuture<Output = Result<T, E>>,
    C: FnOnce(T) -> N + NonWasmSend,
    N: MaybeFuture<Output = Result<U, E>>,
{
    type Output = Result<U, E>;
    type IntoFuture = AndThenFuture<F::IntoFuture, C, N::IntoFuture>;

    fn into_future(self) -> Self::IntoFuture {
        AndThenFuture {
            first: Some(Box::pin(self.wrapped.into_future())),
            continuation: Some(self.continuation),
            second: None,
        }
    }
}

impl<F, C, N, T, U, E> MaybeFuture for AndThen<F, C, N>
where
    F: MaybeFuture<Output = Result<T, E>>,
    C: FnOnce(T) -> N + NonWasmSend,
    N: MaybeFuture<Output = Result<U, E>>,
{
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output {
        let value = self.wrapped.wait()?;
        (self.continuation)(value).wait()
    }
}

struct AndThenFuture<F, C, N> {
    first: Option<Pin<Box<F>>>,
    continuation: Option<C>,
    second: Option<Pin<Box<N>>>,
}

impl<F, C, N> Unpin for AndThenFuture<F, C, N> {}

impl<F, C, N, Next, T, U, E> Future for AndThenFuture<F, C, N>
where
    F: Future<Output = Result<T, E>>,
    C: FnOnce(T) -> Next,
    Next: IntoFuture<Output = Result<U, E>, IntoFuture = N>,
    N: Future<Output = Result<U, E>>,
{
    type Output = Result<U, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            if let Some(second) = self.second.as_mut() {
                return second.as_mut().poll(cx);
            }

            let first = self.first.as_mut().expect("polled after completion");
            match first.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    self.first = None;
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(value)) => {
                    self.first = None;
                    let continuation = self
                        .continuation
                        .take()
                        .expect("continuation missing after first operation completed");
                    self.second = Some(Box::pin(continuation(value).into_future()));
                }
            }
        }
    }
}

pub(crate) enum Either<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> Either<L, R> {
    pub(crate) fn left(value: L) -> Self {
        Self::Left(value)
    }

    pub(crate) fn right(value: R) -> Self {
        Self::Right(value)
    }
}

impl<L, R> IntoFuture for Either<L, R>
where
    L: MaybeFuture,
    R: MaybeFuture<Output = L::Output>,
{
    type Output = L::Output;
    type IntoFuture = EitherFuture<L::IntoFuture, R::IntoFuture>;

    fn into_future(self) -> Self::IntoFuture {
        match self {
            Self::Left(future) => EitherFuture::Left(Box::pin(future.into_future())),
            Self::Right(future) => EitherFuture::Right(Box::pin(future.into_future())),
        }
    }
}

impl<L, R> MaybeFuture for Either<L, R>
where
    L: MaybeFuture,
    R: MaybeFuture<Output = L::Output>,
{
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output {
        match self {
            Self::Left(future) => future.wait(),
            Self::Right(future) => future.wait(),
        }
    }
}

pub(crate) enum EitherFuture<L, R> {
    Left(Pin<Box<L>>),
    Right(Pin<Box<R>>),
}

impl<L, R> Unpin for EitherFuture<L, R> {}

impl<L, R> Future for EitherFuture<L, R>
where
    L: Future,
    R: Future<Output = L::Output>,
{
    type Output = L::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut *self {
            Self::Left(future) => future.as_mut().poll(cx),
            Self::Right(future) => future.as_mut().poll(cx),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type OperationFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
#[cfg(target_arch = "wasm32")]
pub(crate) type OperationFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Delay construction of an operation that may submit IO eagerly.
pub(crate) fn defer<F, M>(operation: F) -> impl MaybeFuture<Output = M::Output>
where
    F: FnOnce() -> M + NonWasmSend,
    M: MaybeFuture,
{
    ready(()).continue_with(move |_| operation())
}

/// Control algorithms await only runtime-independent USB transfers. Blocking
/// execution parks this thread while nusb's USB event loop drives completions.
/// Discovery and clear-halt keep their native nusb `.wait()` paths separately.
pub(crate) fn operation<F: Future + NonWasmSend>(
    future: F,
) -> impl MaybeFuture<Output = F::Output> {
    ControlOperation(future)
}
struct ControlOperation<F>(F);
impl<F: Future> IntoFuture for ControlOperation<F> {
    type Output = F::Output;
    type IntoFuture = F;
    fn into_future(self) -> F {
        self.0
    }
}
impl<F: Future + NonWasmSend> MaybeFuture for ControlOperation<F> {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> F::Output {
        futures_lite::future::block_on(self.0)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn eager_operations_are_constructed_only_when_waited_or_polled() {
        for mode in 0..4 {
            let count = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&count);
            let operation = defer(move || {
                observed.fetch_add(1, Ordering::SeqCst);
                ready(42)
            });
            assert_eq!(count.load(Ordering::SeqCst), 0);
            match mode {
                0 => drop(operation),
                1 => {
                    let future = operation.into_future();
                    assert_eq!(count.load(Ordering::SeqCst), 0);
                    drop(future);
                }
                2 => assert_eq!(operation.wait(), 42),
                _ => assert_eq!(futures_lite::future::block_on(operation.into_future()), 42),
            }
            assert_eq!(count.load(Ordering::SeqCst), usize::from(mode >= 2));
        }
    }
}
