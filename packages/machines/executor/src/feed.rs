//! Turning what happens on the chain into the streams a strategy awaits.
//!
//! A strategy never polls. It holds the venue's account stream and the vault's
//! open and waits, and this is what fills them: one watcher reads the chain's
//! logs as blocks arrive and pushes what concerns this vault into a channel,
//! and a producer hands those to the component as it reads them.
//!
//! The channel belongs to the vault, not to one run of its agent. The watcher
//! marks a block read once it has put that block's events in the channel,
//! not once the agent has read them, so an agent that stops before it has
//! (a fault, an allowance change, a dry tank) leaves them there, and the
//! agent started after it reads them first.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use wasmtime::StoreContextMut;
use wasmtime::component::{Destination, StreamProducer, StreamResult, VecBuffer};

/// The reading end of a vault's channel, shared by every agent the vault
/// runs, one after another.
pub type Reader<T> = Arc<Mutex<UnboundedReceiver<T>>>;

/// One of a vault's channels, kept for as long as the vault is served.
pub struct Feed<T> {
    sender: UnboundedSender<T>,
    reader: Reader<T>,
}

impl<T> Clone for Feed<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            reader: self.reader.clone(),
        }
    }
}

impl<T> Default for Feed<T> {
    fn default() -> Self {
        let (sender, reader) = tokio::sync::mpsc::unbounded_channel();
        Self {
            sender,
            reader: Arc::new(Mutex::new(reader)),
        }
    }
}

impl<T> Feed<T> {
    /// Where a watcher puts what it reads.
    pub fn sender(&self) -> UnboundedSender<T> {
        self.sender.clone()
    }

    /// Where an agent takes it from, starting with whatever an earlier one
    /// left unread.
    pub fn reader(&self) -> Reader<T> {
        self.reader.clone()
    }

    /// Put `first` ahead of whatever is waiting, dropping any waiting event
    /// that `same` says is the same news, so it is neither late nor said
    /// twice. Only while no agent and no watcher runs: the waiting events are
    /// taken out and put back behind it.
    pub fn put_first(&self, first: T, same: impl Fn(&T) -> bool) {
        let mut reader = self
            .reader
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut waiting = Vec::new();
        while let Ok(event) = reader.try_recv() {
            if !same(&event) {
                waiting.push(event);
            }
        }
        for event in std::iter::once(first).chain(waiting) {
            let _ = self.sender.send(event);
        }
    }
}

/// Told of each event a producer hands over.
type Note<T> = Box<dyn Fn(&T) + Send + Sync>;

/// The host's end of a `stream<T>`: whatever is put into the channel comes out
/// of the stream, in order, and the stream ends when the channel closes.
pub struct ChannelProducer<T> {
    events: Reader<T>,
    /// Told of each event as it is handed over.
    noting: Option<Note<T>>,
}

impl<T> ChannelProducer<T> {
    pub fn new(events: Reader<T>) -> Self {
        Self {
            events,
            noting: None,
        }
    }

    /// Have `note` told of each event as it is handed to the strategy.
    pub fn noting(mut self, note: impl Fn(&T) + Send + Sync + 'static) -> Self {
        self.noting = Some(Box::new(note));
        self
    }

    fn note(&self, batch: &[T]) {
        if let Some(note) = &self.noting {
            batch.iter().for_each(note);
        }
    }
}

impl<T, D> StreamProducer<D> for ChannelProducer<T>
where
    T: Send + Sync + Unpin + 'static,
{
    type Item = T;
    type Buffer = VecBuffer<T>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        mut destination: Destination<'a, Self::Item, Self::Buffer>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let wanted = destination.remaining(&mut store).unwrap_or(32);

        // A zero-length read asks "would you have something for me?". Saying
        // yes without producing anything is right here: the next read either
        // finds an event waiting or waits for one.
        if wanted == 0 {
            return Poll::Ready(Ok(StreamResult::Completed));
        }

        let mut batch = Vec::new();
        // Only one agent runs at a time, so this is never waited on; a
        // poisoned lock is still the same channel.
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if batch.len() >= wanted {
                break;
            }
            match events.poll_recv(cx) {
                Poll::Ready(Some(event)) => batch.push(event),
                // The watcher has stopped, so the stream is over.
                Poll::Ready(None) => {
                    if batch.is_empty() {
                        return Poll::Ready(Ok(StreamResult::Dropped));
                    }
                    self.note(&batch);
                    destination.set_buffer(batch.into());
                    return Poll::Ready(Ok(StreamResult::Dropped));
                }
                Poll::Pending => break,
            }
        }

        if batch.is_empty() {
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            // The waker is held by the channel; it fires when an event lands.
            return Poll::Pending;
        }

        self.note(&batch);
        destination.set_buffer(batch.into());
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

/// The host's end of a `stream<T>` fed from any stream of the host's own:
/// whatever it yields comes out of the component's stream, in order, and the
/// component's stream ends when it does. Whatever the stream holds (a
/// connection's slot in a quota, say) is let go when the component drops its
/// end.
pub struct FromStream<S>(pub S);

impl<S, D> StreamProducer<D> for FromStream<S>
where
    S: futures::Stream + Send + Unpin + 'static,
    S::Item: Send + Sync + Unpin + 'static,
{
    type Item = S::Item;
    type Buffer = VecBuffer<S::Item>;

    fn poll_produce<'a>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut store: StoreContextMut<'a, D>,
        mut destination: Destination<'a, Self::Item, Self::Buffer>,
        finish: bool,
    ) -> Poll<wasmtime::Result<StreamResult>> {
        let wanted = destination.remaining(&mut store).unwrap_or(32);
        if wanted == 0 {
            return Poll::Ready(Ok(StreamResult::Completed));
        }
        let mut batch = Vec::new();
        let mut ended = false;
        while batch.len() < wanted {
            match Pin::new(&mut self.0).poll_next(cx) {
                Poll::Ready(Some(item)) => batch.push(item),
                Poll::Ready(None) => {
                    ended = true;
                    break;
                }
                Poll::Pending => break,
            }
        }
        if batch.is_empty() {
            if ended {
                return Poll::Ready(Ok(StreamResult::Dropped));
            }
            if finish {
                return Poll::Ready(Ok(StreamResult::Cancelled));
            }
            return Poll::Pending;
        }
        destination.set_buffer(batch.into());
        Poll::Ready(Ok(if ended {
            StreamResult::Dropped
        } else {
            StreamResult::Completed
        }))
    }
}
