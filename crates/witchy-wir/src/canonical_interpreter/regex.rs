//! Independent semantics: index a Unicode boundary table instead of sharing
//! the runtime's streaming byte-to-character conversion or buffer writes.
use super::*;

fn character_spans(pattern: &str, text: &str) -> Result<Vec<u8>, InterpretError> {
    let matcher = ::regex::Regex::new(pattern).map_err(|_| InterpretError::ExplicitTrap)?;
    let boundaries = text.char_indices().map(|(byte, _)| byte)
        .chain(std::iter::once(text.len())).collect::<Vec<_>>();
    let mut spans = Vec::new();
    for matched in matcher.find_iter(text) {
        let start = boundaries.binary_search(&matched.start()).map_err(|_| InterpretError::ExplicitTrap)?;
        let end = boundaries.binary_search(&matched.end()).map_err(|_| InterpretError::ExplicitTrap)?;
        spans.push(format!("{start},{end}"));
    }
    Ok(spans.join(";").into_bytes())
}

impl Engine<'_> {
    pub(super) fn regex_spans_host_call(&mut self, destinations: &[ValueId],
        arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError> {
        let ([destination], [HostCallArgument::Buffer { place: pattern, .. },
            HostCallArgument::Buffer { place: text, .. }, HostCallArgument::Buffer { place: output, .. }])
            = (destinations, arguments) else {
                return Err(InterpretError::UnsupportedOperation("regex spans host ABI"));
            };
        let pattern = self.host_string(pattern, values)?;
        let text = self.host_string(text, values)?;
        let (object, start, capacity) = self.slice_place(output, values)?;
        self.check_range(object, start, capacity, true)?;
        let encoded = character_spans(&pattern, &text)?;
        let required = u64::try_from(encoded.len()).map_err(|_| InterpretError::ArithmeticTrap)?;
        if required > i32::MAX as u64 { return Err(InterpretError::ArithmeticTrap); }
        if required <= capacity {
            for (offset, byte) in encoded.into_iter().enumerate() {
                let index = start.checked_add(offset as u64).ok_or(InterpretError::ArithmeticTrap)?;
                self.object_mut(object)?.cells.insert(CellKey::Element(index), InterpreterValue::Size(u64::from(byte)));
            }
        }
        values.insert(*destination, InterpreterValue::Size(required));
        Ok(())
    }
}
