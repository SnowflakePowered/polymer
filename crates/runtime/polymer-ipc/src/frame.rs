//! Shared-memory frame-ready transport.

use crate::{
    control::{ChannelKind, LocalSocket, MapOffer, Message},
    shared,
};
use polymer_protocol::FrameReady;
use std::io;

pub struct FramePublisher {
    shared: shared::Publisher,
    generation: u64,
}

impl FramePublisher {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            shared: shared::Publisher::create("frame")?,
            generation: 1,
        })
    }

    pub fn offer(&self, control: &mut LocalSocket, peer_process_id: u32) -> io::Result<()> {
        control.send(Message::MapOffer(MapOffer {
            channel: ChannelKind::Frame,
            handle: self.shared.duplicate_for(peer_process_id)?,
            mapping_len: self.shared.mapping_len(),
            generation: self.generation,
        }))
    }

    pub fn publish(&mut self, frame: FrameReady) -> io::Result<()> {
        self.shared.publish(&crate::control::encode_frame(frame))
    }
}

pub struct FrameSubscriber {
    shared: shared::Subscriber,
    scratch: Vec<u8>,
}

impl FrameSubscriber {
    pub fn from_offer(offer: MapOffer) -> io::Result<Self> {
        if offer.channel != ChannelKind::Frame {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wrong shared channel kind",
            ));
        }
        // SAFETY: the producer transferred a handle duplicated into this process.
        let shared = unsafe { shared::Subscriber::from_handle(offer.handle, offer.mapping_len)? };
        Ok(Self {
            shared,
            scratch: Vec::new(),
        })
    }

    pub fn latest(&mut self) -> io::Result<Option<FrameReady>> {
        if self.shared.latest(&mut self.scratch)? {
            crate::control::decode_frame(&self.scratch).map(Some)
        } else {
            Ok(None)
        }
    }
}
