use std::fmt;
use std::net::SocketAddr;
use std::sync::mpsc::RecvError;
use std::time::Duration;
use fibers::net::TcpStream;
use fibers::net::futures::Connect;
use fibers::time::timer::{self, Timeout};
use futures::{Async, Future, Poll, Stream};
use slog::Logger;
use trackable::error::ErrorKindExt;

use {Error, ErrorKind, Result};
use client_side_handlers::{BoxResponseHandler, IncomingFrameHandler};
use frame::HandleFrame;
use frame_stream::FrameStream;
use message::{MessageSeqNo, OutgoingMessage};
use message_stream::{MessageStream, MessageStreamEvent};

#[derive(Debug)]
pub struct ClientSideChannel {
    logger: Logger,
    server: SocketAddr,
    keep_alive: KeepAlive,
    next_seqno: MessageSeqNo,
    message_stream: MessageStreamState,
    exponential_backoff: ExponentialBackoff,
}
impl ClientSideChannel {
    pub fn new(logger: Logger, server: SocketAddr) -> Self {
        ClientSideChannel {
            logger,
            server,
            keep_alive: KeepAlive::new(Duration::from_secs(60 * 10)),
            next_seqno: MessageSeqNo::new_client_side_seqno(),
            message_stream: MessageStreamState::new(server),
            exponential_backoff: ExponentialBackoff::new(),
        }
    }

    pub fn send_message(
        &mut self,
        message: OutgoingMessage,
        response_handler: Option<BoxResponseHandler>,
    ) {
        let seqno = self.next_seqno.next();
        self.message_stream
            .send_message(seqno, message, response_handler);
    }

    pub fn force_wakeup(&mut self) {
        if let MessageStreamState::Wait { .. } = self.message_stream {
            info!(self.logger, "Waked up");
            self.exponential_backoff.next();
            let next = MessageStreamState::Connecting {
                buffer: Vec::new(),
                future: TcpStream::connect(self.server),
            };
            self.message_stream = next;
        }
    }

    fn poll_message_stream(&mut self) -> Result<Async<Option<MessageStreamState>>> {
        match self.message_stream {
            MessageStreamState::Wait { ref mut timeout } => {
                if track!(timeout.poll().map_err(from_timeout_error))?.is_ready() {
                    info!(
                        self.logger,
                        "Reconnecting timeout expired; starts reconnecting"
                    );
                    self.exponential_backoff.next();
                    let next = MessageStreamState::Connecting {
                        buffer: Vec::new(),
                        future: TcpStream::connect(self.server),
                    };
                    Ok(Async::Ready(Some(next)))
                } else {
                    Ok(Async::NotReady)
                }
            }
            MessageStreamState::Connecting {
                ref mut future,
                ref mut buffer,
            } => {
                match track!(future.poll().map_err(Error::from)) {
                    Err(e) => {
                        error!(self.logger, "Failed to TCP connect: {}", e);
                        // TODO: 共通化
                        let next = if let Some(timeout) = self.exponential_backoff.timeout() {
                            MessageStreamState::Wait { timeout }
                        } else {
                            self.exponential_backoff.next();
                            MessageStreamState::Connecting {
                                buffer: Vec::new(),
                                future: TcpStream::connect(self.server),
                            }
                        };
                        Ok(Async::Ready(Some(next)))
                    }
                    Ok(Async::NotReady) => Ok(Async::NotReady),
                    Ok(Async::Ready(stream)) => {
                        info!(
                            self.logger,
                            "TCP connected: stream={:?}, buffer.len={}",
                            stream,
                            buffer.len()
                        );
                        let stream = FrameStream::new(stream);
                        let mut stream = MessageStream::new(stream, IncomingFrameHandler::new());
                        for m in buffer.drain(..) {
                            // TODO: 共通化
                            stream.send_message(m.seqno, m.message);
                            if let Some(handler) = m.handler {
                                stream
                                    .incoming_frame_handler_mut()
                                    .register_response_handler(m.seqno, handler);
                            }
                        }
                        let next = MessageStreamState::Connected { stream };
                        Ok(Async::Ready(Some(next)))
                    }
                }
            }
            MessageStreamState::Connected { ref mut stream } => match track!(stream.poll()) {
                Err(e) => {
                    error!(self.logger, "Message stream aborted: {}", e);
                    let next = if let Some(timeout) = self.exponential_backoff.timeout() {
                        MessageStreamState::Wait { timeout }
                    } else {
                        self.exponential_backoff.next();
                        MessageStreamState::Connecting {
                            buffer: Vec::new(),
                            future: TcpStream::connect(self.server),
                        }
                    };
                    Ok(Async::Ready(Some(next)))
                }
                Ok(Async::NotReady) => Ok(Async::NotReady),
                Ok(Async::Ready(None)) => {
                    warn!(self.logger, "Message stream terminated");
                    let next = if let Some(timeout) = self.exponential_backoff.timeout() {
                        MessageStreamState::Wait { timeout }
                    } else {
                        self.exponential_backoff.next();
                        MessageStreamState::Connecting {
                            buffer: Vec::new(),
                            future: TcpStream::connect(self.server),
                        }
                    };
                    Ok(Async::Ready(Some(next)))
                }
                Ok(Async::Ready(Some(event))) => {
                    if event.is_ok() {
                        self.exponential_backoff.reset();
                        self.keep_alive.extend_period();
                    }
                    match event {
                        MessageStreamEvent::Sent {
                            seqno,
                            result: Err(e),
                        }
                        | MessageStreamEvent::Received {
                            seqno,
                            result: Err(e),
                        } => {
                            stream.incoming_frame_handler_mut().handle_error(seqno, e);
                        }
                        _ => {}
                    }
                    Ok(Async::Ready(None))
                }
            },
        }
    }
}
impl Future for ClientSideChannel {
    type Item = ();
    type Error = Error;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if track!(self.keep_alive.poll())?.is_ready() {
            return Ok(Async::Ready(()));
        }
        while let Async::Ready(next) = track!(self.poll_message_stream())? {
            if let Some(next) = next {
                self.message_stream = next;
            }
        }
        Ok(Async::NotReady)
    }
}

#[derive(Debug)]
enum MessageStreamState {
    Wait {
        timeout: Timeout,
    },
    Connecting {
        buffer: Vec<BufferedMessage>,
        future: Connect,
    },
    Connected {
        stream: MessageStream<IncomingFrameHandler>,
    },
}
impl MessageStreamState {
    fn new(addr: SocketAddr) -> Self {
        MessageStreamState::Connecting {
            buffer: Vec::new(),
            future: TcpStream::connect(addr),
        }
    }

    fn send_message(
        &mut self,
        seqno: MessageSeqNo,
        message: OutgoingMessage,
        handler: Option<BoxResponseHandler>,
    ) {
        match *self {
            MessageStreamState::Wait { .. } => {
                if let Some(mut handler) = handler {
                    let e = ErrorKind::Unavailable
                        .cause("TCP stream disconnected (waiting for reconnecting)");
                    handler.handle_error(seqno, track!(e).into());
                }
            }
            MessageStreamState::Connecting { ref mut buffer, .. } => {
                buffer.push(BufferedMessage {
                    seqno,
                    message,
                    handler,
                });
            }
            MessageStreamState::Connected { ref mut stream } => {
                stream.send_message(seqno, message);
                if let Some(handler) = handler {
                    stream
                        .incoming_frame_handler_mut()
                        .register_response_handler(seqno, handler);
                }
            }
        }
    }
}

struct BufferedMessage {
    seqno: MessageSeqNo,
    message: OutgoingMessage,
    handler: Option<BoxResponseHandler>,
}
impl fmt::Debug for BufferedMessage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "BufferedMessage {{ seqno: {:?}, .. }}", self.seqno)
    }
}

#[derive(Debug)]
struct KeepAlive {
    future: Timeout,
    timeout: Duration,
    extend_period: bool,
}
impl KeepAlive {
    fn new(timeout: Duration) -> Self {
        KeepAlive {
            future: timer::timeout(timeout),
            timeout,
            extend_period: false,
        }
    }

    fn extend_period(&mut self) {
        self.extend_period = true;
    }

    fn poll_timeout(&mut self) -> Result<bool> {
        let result = track!(self.future.poll().map_err(from_timeout_error))?;
        Ok(result.is_ready())
    }
}
impl Future for KeepAlive {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        while track!(self.poll_timeout())? {
            if self.extend_period {
                self.future = timer::timeout(self.timeout);
                self.extend_period = false;
            } else {
                return Ok(Async::Ready(()));
            }
        }
        Ok(Async::NotReady)
    }
}

#[derive(Debug)]
struct ExponentialBackoff {
    retried_count: usize,
}
impl ExponentialBackoff {
    fn new() -> Self {
        ExponentialBackoff { retried_count: 0 }
    }
    fn next(&mut self) {
        self.retried_count += 1;
    }
    fn timeout(&self) -> Option<Timeout> {
        if self.retried_count == 0 {
            None
        } else {
            let duration = Duration::from_secs(2u64.pow(self.retried_count as u32 - 1));
            Some(timer::timeout(duration))
        }
    }
    fn reset(&mut self) {
        self.retried_count = 0;
    }
}

fn from_timeout_error(_: RecvError) -> Error {
    ErrorKind::Other.cause("Broken timer").into()
}
