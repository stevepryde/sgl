use std::io::{self, Read, Write};

use tungstenite::protocol::{Message, WebSocket};
use tungstenite::{Error as WebSocketError, Result as WebSocketResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DrainResult {
    Empty,
    Blocked,
}

pub(super) trait MessageSink {
    fn write_message(&mut self, message: Message) -> WebSocketResult<()>;
    fn flush_messages(&mut self) -> WebSocketResult<()>;
}

impl<Stream: Read + Write> MessageSink for WebSocket<Stream> {
    fn write_message(&mut self, message: Message) -> WebSocketResult<()> {
        self.write(message)
    }

    fn flush_messages(&mut self) -> WebSocketResult<()> {
        self.flush()
    }
}

/// Flushes previously accepted bytes, then admits queued messages until the
/// socket blocks or the queue is empty.
pub(super) fn drain_outbound<S, Next>(
    sink: &mut S,
    pending: &mut Option<Message>,
    mut next: Next,
) -> WebSocketResult<DrainResult>
where
    S: MessageSink,
    Next: FnMut() -> Option<Message>,
{
    if flush(sink)? == DrainResult::Blocked {
        return Ok(DrainResult::Blocked);
    }

    loop {
        let Some(message) = pending.take().or_else(&mut next) else {
            return flush(sink);
        };
        match sink.write_message(message) {
            Ok(()) => {}
            Err(WebSocketError::WriteBufferFull(message)) => {
                *pending = Some(*message);
                return Ok(DrainResult::Blocked);
            }
            Err(WebSocketError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DrainResult::Blocked);
            }
            Err(error) => return Err(error),
        }
    }
}

fn flush<S: MessageSink>(sink: &mut S) -> WebSocketResult<DrainResult> {
    match sink.flush_messages() {
        Ok(()) => Ok(DrainResult::Empty),
        Err(WebSocketError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
            Ok(DrainResult::Blocked)
        }
        Err(error) => Err(error),
    }
}
