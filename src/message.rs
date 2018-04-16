use std::fmt;
use bytecodec::{self, ByteCount, Decode, Encode, EncodeExt, Eos};
use bytecodec::marker::Never;
use byteorder::{BigEndian, ByteOrder};

use {ProcedureId, Result};

#[derive(Debug, Clone)]
pub struct MessageHeader {
    pub id: MessageId,
    pub procedure: ProcedureId,
    pub priority: u8,
    pub async: bool,
}
impl MessageHeader {
    pub const SIZE: usize = 8 + 4 + 1;

    pub fn write(&self, buf: &mut [u8]) {
        BigEndian::write_u64(buf, self.id.0);
        BigEndian::write_u32(&mut buf[8..], self.procedure.0);
        buf[12] = self.priority;
    }

    pub fn read(buf: &[u8]) -> Self {
        let id = MessageId(BigEndian::read_u64(buf));
        let procedure = ProcedureId(BigEndian::read_u32(&buf[8..]));
        let priority = buf[12];
        MessageHeader {
            id,
            procedure,
            priority,
            async: false, // dummy
        }
    }
}

pub trait AssignIncomingMessageHandler {
    type Handler: Decode + Send + 'static;
    fn assign_incoming_message_handler(&mut self, header: &MessageHeader) -> Result<Self::Handler>;
}

/// Message identifier.
///
/// This value is unique within a channel.
///
/// Note that request and response messages has the same identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub u64);
impl MessageId {
    pub fn next(&mut self) -> Self {
        let n = self.0;
        self.0 += 1;
        MessageId(n)
    }
}

#[derive(Debug)]
pub struct OutgoingMessage {
    pub header: MessageHeader,
    pub payload: OutgoingMessagePayload,
}

pub struct OutgoingMessagePayload(Box<Encode<Item = Never> + Send + 'static>);
impl OutgoingMessagePayload {
    pub fn new<E>(encoder: E) -> Self
    where
        E: Encode + Send + 'static,
    {
        OutgoingMessagePayload(Box::new(encoder.last()))
    }

    pub fn with_item<E>(encoder: E, item: E::Item) -> Self
    where
        E: Encode + Send + 'static,
        E::Item: Send + 'static,
    {
        let encoder = Lazy {
            inner: encoder,
            item: Some(item),
        };
        Self::new(encoder)
    }
}
impl fmt::Debug for OutgoingMessagePayload {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "OutgoingMessagePayload(_)")
    }
}
impl Encode for OutgoingMessagePayload {
    type Item = Never;

    fn encode(&mut self, buf: &mut [u8], eos: Eos) -> bytecodec::Result<usize> {
        self.0.encode(buf, eos)
    }

    fn start_encoding(&mut self, item: Self::Item) -> bytecodec::Result<()> {
        self.0.start_encoding(item)
    }

    fn is_idle(&self) -> bool {
        self.0.is_idle()
    }

    fn requiring_bytes(&self) -> ByteCount {
        self.0.requiring_bytes()
    }
}

#[derive(Debug)]
struct Lazy<E: Encode> {
    inner: E,
    item: Option<E::Item>,
}
impl<E: Encode> Encode for Lazy<E> {
    type Item = Never;

    fn encode(&mut self, buf: &mut [u8], eos: Eos) -> bytecodec::Result<usize> {
        if let Some(item) = self.item.take() {
            track!(self.inner.start_encoding(item))?;
        }
        track!(self.inner.encode(buf, eos))
    }

    fn start_encoding(&mut self, _item: Self::Item) -> bytecodec::Result<()> {
        unreachable!()
    }

    fn is_idle(&self) -> bool {
        self.item.is_none() && self.inner.is_idle()
    }

    fn requiring_bytes(&self) -> ByteCount {
        if self.item.is_some() {
            ByteCount::Unknown
        } else {
            self.inner.requiring_bytes()
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn message_id_next_works() {
        let mut message_id = MessageId(0);
        assert_eq!(message_id.0, 0);

        let prev = message_id.next();
        assert_eq!(prev.0, 0);
        assert_eq!(message_id.0, 1);
    }
}
