//! A tonic codec over `prost_reflect::DynamicMessage`.
//!
//! This is what lets Swarmo call a service it was never compiled against:
//! tonic normally pairs generated request/response types with a `ProstCodec`,
//! but a `DynamicMessage` implements `prost::Message` too, so the only thing
//! the codec needs to carry is the descriptor to decode responses into.

use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::Status;

#[derive(Debug, Clone)]
pub struct DynamicCodec {
    response_type: MessageDescriptor,
}

impl DynamicCodec {
    pub fn new(response_type: MessageDescriptor) -> Self {
        Self { response_type }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            response_type: self.response_type.clone(),
        }
    }
}

#[derive(Debug)]
pub struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, buf: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(buf)
            .map_err(|e| Status::internal(format!("could not encode the request message: {e}")))
    }
}

#[derive(Debug)]
pub struct DynamicDecoder {
    response_type: MessageDescriptor,
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, buf: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let msg = DynamicMessage::decode(self.response_type.clone(), buf)
            .map_err(|e| Status::internal(format!("could not decode the response message: {e}")))?;
        Ok(Some(msg))
    }
}
