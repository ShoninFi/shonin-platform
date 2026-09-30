//! Turning what happens on the chain into the streams a strategy awaits.
//!
//! A strategy never polls. It holds the venue's account stream and the vault's
//! open and waits, and this is what fills them: one watcher reads the chain's
//! logs as blocks arrive and pushes what concerns this vault into a channel,
//! and a producer hands those to the component as it reads them.

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::mpsc::UnboundedReceiver;
use wasmtime::StoreContextMut;
use wasmtime::component::{Destination, StreamProducer, StreamResult, VecBuffer};

/// The host's end of a `stream<T>`: whatever is put into the channel comes out
/// of the stream, in order, and the stream ends when the channel closes.
pub struct ChannelProducer<T> {
    events: UnboundedReceiver<T>,
}

impl<T> ChannelProducer<T> {
    pub fn new(events: UnboundedReceiver<T>) -> Self {
        Self { events }
    }
}

impl<T, D> StreamProducer<D> for ChannelProducer<T>
where
    T: Send + Sync + Unpin + 'static,
{
    type Item = T;
    type Buffer = VecBuffer<T>;

    fn poll_produce<'a>(
        mut self: Pin<&mut Self>,
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
        loop {
            if batch.len() >= wanted {
                break;
            }
            match self.events.poll_recv(cx) {
                Poll::Ready(Some(event)) => batch.push(event),
                // The watcher has stopped, so the stream is over.
                Poll::Ready(None) => {
                    if batch.is_empty() {
                        return Poll::Ready(Ok(StreamResult::Dropped));
                    }
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

        destination.set_buffer(batch.into());
        Poll::Ready(Ok(StreamResult::Completed))
    }
}
