//! A crate for easily cloneable [`Stream`]s, similar to
//! `[FutureExt::shared](https://docs.rs/futures/latest/futures/future/trait.FutureExt.html#method.shared)`.
//!
//! # Examples
//! ```
//! # futures::executor::block_on(async {
//! use futures::stream::{self, StreamExt};
//! use shared_stream::Share;
//!
//! let shared = stream::iter(1..=3).shared();
//! assert_eq!(shared.clone().take(1).collect::<Vec<_>>().await, [1]);
//! assert_eq!(shared.clone().skip(1).take(1).collect::<Vec<_>>().await, [2]);
//! assert_eq!(shared.collect::<Vec<_>>().await, [1, 2, 3]);
//! # });
//! ```
//!
//! If you need to share streams across thread boundaries, use `ashared`:
//!
//! ```
//! use futures::stream::{self, StreamExt};
//! use shared_stream::Share;
//! use std::thread;
//!
//! let shared1 = stream::iter(1..=3).ashared();
//! let shared2 = shared1.clone();
//! let shared3 = shared1.clone();
//! thread::spawn(move || futures::executor::block_on(async {
//!     assert_eq!(shared1.take(1).collect::<Vec<_>>().await, [1]);
//! })).join().unwrap();
//! thread::spawn(move || futures::executor::block_on(async {
//!     assert_eq!(shared2.skip(1).take(1).collect::<Vec<_>>().await, [2]);
//! })).join().unwrap();
//! thread::spawn(move || futures::executor::block_on(async {
//!     assert_eq!(shared3.collect::<Vec<_>>().await, [1, 2, 3]);
//! })).join().unwrap();
//! ```
#![warn(
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    future_incompatible,
    nonstandard_style,
    rust_2018_compatibility,
    rust_2018_idioms,
    rustdoc::all,
    unused,
    absolute_paths_not_starting_with_crate,
    anonymous_parameters,
    box_pointers,
    elided_lifetimes_in_paths,
    explicit_outlives_requirements,
    keyword_idents,
    macro_use_extern_crate,
    meta_variable_misuse,
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    non_ascii_idents,
    pointer_structural_match,
    single_use_lifetimes,
    trivial_casts,
    trivial_numeric_casts,
    unaligned_references,
    unreachable_pub,
    unstable_features,
    unused_crate_dependencies,
    unused_extern_crates,
    unused_import_braces,
    unused_lifetimes,
    unused_qualifications,
    unused_results,
    variant_size_differences
)]

use core::pin::Pin;
use core::task::Context;
use core::task::Poll;
use futures_core::{FusedStream, Stream};
use futures_util::task::{waker_ref, ArcWake};
use pin_project_lite::pin_project;
use std::cell::RefCell;
use std::fmt;
use std::mem;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use std::task::Waker;

#[derive(Debug)]
struct SharedWaker(Mutex<Vec<Waker>>);

impl SharedWaker {
    fn add_waker(&self, cx: &mut Context<'_>) {
        self.0.lock().unwrap().push(cx.waker().clone());
    }
}

impl ArcWake for SharedWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        for waker in mem::take(&mut *arc_self.0.lock().unwrap()) {
            waker.wake();
        }
    }
}

pin_project! {
    #[project = InnerStateProj]
    #[derive(Debug)]
    struct InnerState<S: Stream> {
        values: Vec<S::Item>,
        #[pin]
        stream: Option<S>,
        waker: Arc<SharedWaker>,
    }
}

impl<S: Stream> InnerState<S> {
    fn new(stream: S) -> Self {
        Self {
            stream: Some(stream),
            values: vec![],
            waker: Arc::new(SharedWaker(Mutex::new(vec![]))),
        }
    }
}

impl<S: Stream> InnerState<S>
where
    S::Item: Clone,
{
    fn get_item(
        mut self: Pin<&mut Self>,
        idx: usize,
        cx: &mut Context<'_>,
    ) -> Poll<Option<S::Item>> {
        loop {
            let this = self.as_mut().project();
            let value = this.values.get(idx).cloned();
            if value.is_none() {
                if let Some(stream) = this.stream.as_pin_mut() {
                    let waker = waker_ref(this.waker);
                    let mut up_cx = Context::from_waker(&waker);
                    match stream.poll_next(&mut up_cx) {
                        Poll::Ready(Some(v)) => {
                            this.values.push(v);
                            continue;
                        }
                        Poll::Ready(None) => {
                            self.as_mut().project().stream.set(None);
                        }
                        Poll::Pending => {
                            this.waker.add_waker(cx);
                            return Poll::Pending;
                        }
                    }
                }
            }
            return Poll::Ready(value);
        }
    }

    fn size_hint(&self, offset: usize) -> (usize, Option<usize>) {
        let upstream_cached = self.values.len() - offset;
        self.stream
            .as_ref()
            .map_or((upstream_cached, Some(upstream_cached)), |stream| {
                let upstream = stream.size_hint();
                (
                    upstream.0 + upstream_cached,
                    upstream.1.map(|v| v + upstream_cached),
                )
            })
    }

    fn is_terminated(&self, offset: usize) -> bool {
        self.stream.is_none() && self.values.len() <= offset
    }
}

/// Stream for the [`shared`](Share::shared) method.
#[must_use = "streams do nothing unless polled"]
pub struct Shared<S: Stream> {
    inner: Rc<RefCell<InnerState<S>>>,
    idx: usize,
}

impl<S> fmt::Debug for Shared<S>
where
    S: Stream + fmt::Debug,
    S::Item: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("inner", &self.inner)
            .field("idx", &self.idx)
            .finish()
    }
}

impl<S: Stream> Shared<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: Rc::new(RefCell::new(InnerState::new(stream))),
            idx: 0,
        }
    }
}

impl<S: Stream> Clone for Shared<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
            idx: self.idx,
        }
    }
}

impl<S: Stream> Stream for Shared<S>
where
    S::Item: Clone,
{
    type Item = S::Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // pin project Pin<&mut Self> -> Pin<&mut InnerState<I, S>>
        // this is only safe because we don't do anything else with Self::inner except
        // cloning (the Rc) which doesn't move its content or make it accessible.
        let result = unsafe {
            let inner: &RefCell<InnerState<S>> =
                Pin::into_inner_unchecked(self.as_ref()).inner.as_ref();
            Pin::new_unchecked(&mut *inner.borrow_mut()).get_item(self.idx, cx)
        };
        if let Poll::Ready(Some(_)) = result {
            // trivial safe pin projection
            unsafe { Pin::get_unchecked_mut(self).idx += 1 }
        }
        result
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.borrow().size_hint(self.idx)
    }
}

impl<S: Stream> FusedStream for Shared<S>
where
    S::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        self.inner.borrow().is_terminated(self.idx)
    }
}

/// Stream for the [`ashared`](Share::ashared) method.
#[must_use = "streams do nothing unless polled"]
pub struct Ashared<S: Stream> {
    inner: Arc<RwLock<InnerState<S>>>,
    idx: usize,
}

impl<S> fmt::Debug for Ashared<S>
where
    S: Stream + fmt::Debug,
    S::Item: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ashared")
            .field("inner", &self.inner)
            .field("idx", &self.idx)
            .finish()
    }
}

impl<S: Stream + Send> Ashared<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: Arc::new(RwLock::new(InnerState::new(stream))),
            idx: 0,
        }
    }
}

impl<S: Stream> Clone for Ashared<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            idx: self.idx,
        }
    }
}

impl<S: Stream> Stream for Ashared<S>
where
    S::Item: Clone,
{
    type Item = S::Item;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // pin project Pin<&mut Self> -> Pin<&mut InnerState<I, S>>
        // this is only safe because we don't do anything else with Self::inner except
        // cloning (the Arc) which doesn't move its content or make it accessible.
        let result = unsafe {
            let inner: &RwLock<InnerState<S>> =
                Pin::into_inner_unchecked(self.as_ref()).inner.as_ref();
            Pin::new_unchecked(&mut *inner.write().unwrap()).get_item(self.idx, cx)
        };
        if let Poll::Ready(Some(_)) = result {
            // trivial safe pin projection
            unsafe { Pin::get_unchecked_mut(self).idx += 1 }
        }
        result
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.read().unwrap().size_hint(self.idx)
    }
}

impl<S: Stream> FusedStream for Ashared<S>
where
    S::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        self.inner.read().unwrap().is_terminated(self.idx)
    }
}

/// An extension trait implemented for [`Stream`]s that provides the [`shared`](Share::shared) and [`ashared`](Share::ashared) methods.
pub trait Share: Stream {
    /// Turns this stream into a cloneable stream. Polled items are cached and cloned.
    ///
    /// Note that this function consumes the stream passed into it and returns a wrapped version of it.
    fn shared(self) -> Shared<Self>
    where
        Self: Sized,
        Self::Item: Clone;

    /// Turns this stream into a cloneable stream that can be shared across threads. Polled items are cached and cloned.
    ///
    /// Note that this function consumes the stream passed into it and returns a wrapped version of it.
    fn ashared(self) -> Ashared<Self>
    where
        Self: Sized + Send,
        Self::Item: Clone;
}

impl<T: Stream> Share for T
where
    T::Item: Clone,
{
    fn shared(self) -> Shared<Self> {
        Shared::new(self)
    }

    fn ashared(self) -> Ashared<Self>
    where
        T: Send,
    {
        Ashared::new(self)
    }
}

#[cfg(test)]
mod test {
    use super::Share;
    use core::cell::RefCell;
    use futures::executor::block_on;
    use futures::future;
    use futures::stream::{self, StreamExt};
    use futures_core::stream::{FusedStream, Stream};
    use std::sync::RwLock;

    fn collect<V: Clone, S: Stream<Item = V>>(stream: S) -> Vec<V> {
        block_on(stream.collect::<Vec<_>>())
    }

    fn test_everything<
        S: Clone + Stream<Item = String> + FusedStream + Unpin,
        F: Fn() -> Vec<String>,
    >(
        orig_stream: S,
        seen: F,
    ) {
        assert_eq!(seen(), [] as [String; 0]);
        assert_eq!(orig_stream.size_hint(), (3, Some(3)));
        assert!(!orig_stream.is_terminated());

        let stream = orig_stream.clone().take(1);
        assert_eq!(stream.size_hint(), (1, Some(1)));
        assert!(!stream.is_terminated());
        let result = collect(stream);
        assert_eq!(result, ["a"]);
        assert_eq!(seen(), ["a"]);
        assert_eq!(orig_stream.size_hint(), (3, Some(3)));
        assert!(!orig_stream.is_terminated());

        let stream = orig_stream.clone();
        assert_eq!(stream.size_hint(), (3, Some(3)));
        assert!(!stream.is_terminated());
        let result = collect(stream);
        assert_eq!(result, ["a", "b", "c"]);
        assert_eq!(seen(), ["a", "b", "c"]);
        assert_eq!(orig_stream.size_hint(), (3, Some(3)));
        assert!(!orig_stream.is_terminated());

        let stream1 = orig_stream.clone().skip(1);
        assert_eq!(stream1.size_hint(), (2, Some(2)));
        assert!(!stream1.is_terminated());
        let stream2 = orig_stream.clone();
        assert_eq!(stream2.size_hint(), (3, Some(3)));
        assert!(!stream2.is_terminated());
        let (result1, result2): (Vec<_>, Vec<_>) =
            block_on(future::join(stream1.collect(), stream2.collect()));
        assert_eq!(result1, ["b", "c"]);
        assert_eq!(result2, ["a", "b", "c"]);
        assert_eq!(seen(), ["a", "b", "c"]);
        assert_eq!(orig_stream.size_hint(), (3, Some(3)));
        assert!(!orig_stream.is_terminated());

        let mut stream1 = orig_stream.clone();
        assert_eq!(Some("a".to_string()), block_on(stream1.next()));
        assert_eq!(stream1.size_hint(), (2, Some(2)));
        assert!(!stream1.is_terminated());
        assert_eq!(Some("b".to_string()), block_on(stream1.next()));
        assert_eq!(stream1.size_hint(), (1, Some(1)));
        assert!(!stream1.is_terminated());
        assert_eq!(Some("c".to_string()), block_on(stream1.next()));
        assert_eq!(stream1.size_hint(), (0, Some(0)));
        assert!(stream1.is_terminated());
        assert_eq!(orig_stream.size_hint(), (3, Some(3)));
        assert!(!orig_stream.is_terminated());
    }

    #[test]
    fn test_everything_shared() {
        let seen = RefCell::new(vec![]);
        let orig_stream = stream::iter(["a", "b", "c"].iter().map(|v| (*v).to_string()))
            .inspect(|v| {
                seen.borrow_mut().push(v.clone());
            })
            .shared();
        test_everything(orig_stream, || seen.borrow().clone());
    }

    #[test]
    fn test_everything_ashared() {
        let seen = RwLock::new(vec![]);
        let orig_stream = stream::iter(["a", "b", "c"].iter().map(|v| (*v).to_string()))
            .inspect(|v| {
                seen.write().unwrap().push(v.clone());
            })
            .ashared();
        test_everything(orig_stream, || seen.read().unwrap().clone());
    }

    #[test]
    fn test_size_hint_for_unfinished() {
        let mut stream = stream::iter(["a", "b", "c"].iter().map(|v| (*v).to_string())).shared();
        assert_eq!(stream.size_hint(), (3, Some(3)));
        assert!(!stream.is_terminated());
        assert_eq!(block_on(stream.next()), Some("a".to_string()));
        assert_eq!(stream.size_hint(), (2, Some(2)));
        assert!(!stream.is_terminated());
    }

    #[test]
    fn ashared_is_send() {
        let _: &dyn Send = &stream::empty::<()>().ashared();
    }

    use futures::channel::mpsc::channel;
    use futures::executor::LocalPool;
    use futures::task::LocalSpawnExt;

    #[test]
    fn test_wake_multiple_wakers() {
        let mut pool = LocalPool::new();
        let spawner = pool.spawner();

        let (mut sender, receiver) = channel(1);
        let mut receiver1 = receiver.ashared();
        let mut receiver2 = receiver1.clone();

        spawner
            .spawn_local(async move {
                assert_eq!(receiver1.next().await, Some(1));
            })
            .unwrap();
        spawner
            .spawn_local(async move {
                assert_eq!(receiver2.next().await, Some(1));
            })
            .unwrap();

        // Run both tasks until they are stalled (since nothing has been sent through the channel
        // yet)
        pool.run_until_stalled();

        // Send something through the channel so that both receivers have something and the tasks
        // are not stalled anymore
        sender.try_send(1).unwrap();

        // Assert that both tasks were indeed woken up
        assert!(pool.try_run_one());
        assert!(pool.try_run_one());
    }
}
