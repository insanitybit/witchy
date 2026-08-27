use crate::ProtocolError;

pub const FRAME_HEADER_BYTES: usize = 16;

pub(crate) struct Input<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Input<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.cursor
    }

    pub(crate) fn finish(self) -> Result<(), ProtocolError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ProtocolError::TrailingInput)
        }
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ProtocolError::Truncated)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ProtocolError::Truncated)?;
        self.cursor = end;
        Ok(value)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes: [u8; 2] = self.take(2)?.try_into().expect("exact length");
        Ok(u16::from_le_bytes(bytes))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("exact length");
        Ok(u32::from_le_bytes(bytes))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, ProtocolError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().expect("exact length");
        Ok(u64::from_le_bytes(bytes))
    }

    pub(crate) fn array_32(&mut self) -> Result<[u8; 32], ProtocolError> {
        Ok(self.take(32)?.try_into().expect("exact length"))
    }

    pub(crate) fn bytes(
        &mut self,
        maximum: usize,
        field: &'static str,
    ) -> Result<Vec<u8>, ProtocolError> {
        let length = self.u32()? as usize;
        if length > maximum {
            return Err(ProtocolError::BudgetExceeded(field));
        }
        Ok(self.take(length)?.to_vec())
    }
}

pub(crate) fn frame(
    magic: &[u8; 8],
    schema: u16,
    kind: u8,
    payload: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    let payload_length =
        u32::try_from(payload.len()).map_err(|_| ProtocolError::LengthOverflow("frame payload"))?;
    let mut output = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    output.extend_from_slice(magic);
    put_u16(&mut output, schema);
    put_u8(&mut output, kind);
    put_u8(&mut output, 0);
    put_u32(&mut output, payload_length);
    output.extend_from_slice(payload);
    Ok(output)
}

pub(crate) fn payload<'a>(
    bytes: &'a [u8],
    maximum: usize,
    magic: &[u8; 8],
    schema: u16,
    kind: u8,
) -> Result<&'a [u8], ProtocolError> {
    if bytes.len() > maximum {
        return Err(ProtocolError::BudgetExceeded("frame bytes"));
    }
    let mut input = Input::new(bytes);
    if input.take(magic.len())? != magic {
        return Err(ProtocolError::InvalidMagic);
    }
    let found_schema = input.u16()?;
    if found_schema != schema {
        return Err(ProtocolError::UnsupportedProtocolSchema(found_schema));
    }
    let found_kind = input.u8()?;
    if found_kind != kind {
        return Err(ProtocolError::UnknownMessageKind(found_kind));
    }
    if input.u8()? != 0 {
        return Err(ProtocolError::NonZeroReserved);
    }
    let length = input.u32()? as usize;
    if length > input.remaining() {
        return Err(ProtocolError::Truncated);
    }
    if length < input.remaining() {
        return Err(ProtocolError::TrailingInput);
    }
    input.take(length)
}

pub(crate) fn put_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

pub(crate) fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn put_bytes(
    output: &mut Vec<u8>,
    value: &[u8],
    field: &'static str,
) -> Result<(), ProtocolError> {
    let length = u32::try_from(value.len()).map_err(|_| ProtocolError::LengthOverflow(field))?;
    put_u32(output, length);
    output.extend_from_slice(value);
    Ok(())
}
