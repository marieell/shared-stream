//! A crate for easily cloneable [`Stream`]s, similar to `FutureExt::shared`.
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
#![warn(
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    future_incompatible,
    nonstandard_style,
    rust_2018_compatibility,
    rust_2018_idioms,
    rustdoc,
    unused,
    absolute_paths_not_starting_with_crate,
    anonymous_parameters,
    box_pointers,
    elided_lifetimes_in_paths,
    explicit_outlives_requirements,
    invalid_html_tags,
    keyword_idents,
    macro_use_extern_crate,
    meta_variable_misuse,
    missing_copy_implementations,
    missing_crate_level_docs,
    missing_debug_implementations,
    missing_doc_code_examples,
    missing_docs,
    non_ascii_idents,
    pointer_structural_match,
    private_doc_tests,
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
use futures_core::ready;
use futures_core::{FusedStream, Stream};
use pin_project_lite::pin_project;
use std::cell::RefCell;
use std::mem;
use std::rc::Rc;

pin_project! {
    #[project = InnerStateProj]
    #[derive(Debug)]
    enum InnerState<I, S> {
        Running { values: Vec<I>, #[pin] stream: S },
        Finished { values: Vec<I> },
    }
}
impl<I: Clone, S: Stream<Item = I>> InnerState<I, S> {
    fn get_item(mut self: Pin<&mut Self>, idx: usize, cx: &mut Context<'_>) -> Poll<Option<I>> {
        loop {
            let this = self.as_mut().project();
            return Poll::Ready(match this {
                InnerStateProj::Running { stream, values } => {
                    let value = values.get(idx).cloned();
                    if value.is_none() {
                        let result = ready!(stream.poll_next(cx));
                        if let Some(v) = result {
                            values.push(v);
                            continue;
                        } else {
                            let values = mem::take(values);
                            self.set(Self::Finished { values });
                        }
                    }
                    value
                }
                InnerStateProj::Finished { values } => values.get(idx).cloned(),
            });
        }
    }
}

/// Stream for the [`shared`](Share::shared) method.
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct Shared<I, S> {
    inner: Rc<RefCell<InnerState<I, S>>>,
    idx: usize,
}

impl<I: Clone, S: Stream<Item = I>> Shared<I, S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: Rc::new(RefCell::new(InnerState::Running {
                stream,
                values: vec![],
            })),
            idx: 0,
        }
    }
}

impl<I, S> Clone for Shared<I, S> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
            idx: self.idx,
        }
    }
}

impl<I: Clone, S: Stream<Item = I>> Stream for Shared<I, S> {
    type Item = I;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // pin project Pin<&mut Self> -> Pin<&mut InnerState<I, S>>
        // this is only safe because we don't do anything else with Self::inner except
        // cloning (the Rc) which doesn't move its content or make it accessible.
        let result = unsafe {
            let inner: &RefCell<InnerState<I, S>> =
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
        match &*self.inner.borrow() {
            InnerState::Running { values, stream } => {
                let upstream_cached = values.len();
                let upstream = stream.size_hint();
                (
                    upstream.0 + upstream_cached,
                    upstream.1.map(|v| v + upstream_cached),
                )
            }
            InnerState::Finished { values } => (values.len(), Some(values.len())),
        }
    }
}

impl<I: Clone, S: Stream<Item = I>> FusedStream for Shared<I, S> {
    fn is_terminated(&self) -> bool {
        match &*self.inner.borrow() {
            InnerState::Running { .. } => false,
            InnerState::Finished { .. } => true,
        }
    }
}

/// An extension trait implemented for [`Stream`]s that provides the [`shared`](Share::shared) method.
pub trait Share: Stream {
    /// Turns this stream into a cloneable stream. Polled items are cached and cloned.
    ///
    /// Note that this function consumes the stream passed into it and returns a wrapped version of it.
    fn shared(self) -> Shared<Self::Item, Self>
    where
        Self: Sized,
        Self::Item: Clone;
}

impl<T: Stream> Share for T
where
    T::Item: Clone,
{
    fn shared(self) -> Shared<Self::Item, Self> {
        Shared::new(self)
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

    fn run<V: Clone, S: Stream<Item = V>>(stream: S) -> Vec<V> {
        block_on(stream.collect::<Vec<_>>())
    }

    #[test]
    fn test_stuff() {
        let seen = RefCell::new(vec![]);
        let stream =
            stream::iter(vec!["a".to_string(), "b".to_string(), "c".to_string()].into_iter())
                .inspect(|v| {
                    seen.borrow_mut().push(v.to_string());
                })
                .shared();
        assert_eq!(*seen.borrow(), [] as [String; 0]);
        assert_eq!(stream.size_hint(), (3, Some(3)));
        assert_eq!(stream.is_terminated(), false);

        let result: Vec<_> = run(stream.clone().take(1));
        assert_eq!(result, ["a"]);
        assert_eq!(*seen.borrow(), ["a"]);
        assert_eq!(stream.size_hint(), (3, Some(3)));
        assert_eq!(stream.is_terminated(), false);

        let result: Vec<_> = run(stream.clone());
        assert_eq!(result, ["a", "b", "c"]);
        assert_eq!(*seen.borrow(), ["a", "b", "c"]);
        assert_eq!(stream.size_hint(), (3, Some(3)));
        assert_eq!(stream.is_terminated(), true);

        let (result, result2): (Vec<_>, Vec<_>) = block_on(future::join(
            stream.clone().skip(1).collect(),
            stream.clone().collect(),
        ));
        assert_eq!(result, ["b", "c"]);
        assert_eq!(result2, ["a", "b", "c"]);
        assert_eq!(*seen.borrow(), ["a", "b", "c"]);
        assert_eq!(stream.size_hint(), (3, Some(3)));
        assert_eq!(stream.is_terminated(), true);
    }
}
