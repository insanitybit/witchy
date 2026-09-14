//! Retained empty directories displaced by rename still denote their opened
//! identity, never the incoming tree. POSIX leaves these handles enumerable but
//! unlinked: no child name exists and creation beneath them fails. File handles
//! separately retain a parent identity plus leaf, as the confined provider does.
use super::*;
use crate::canonical_safewir::AccessMode;

impl Engine<'_> {
    pub(super) fn detached_dir_host_call(&mut self, name: &str, destinations: &[ValueId],
        arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<bool, InterpretError>
    {
        let Some(HostCallArgument::Capability(id)) = arguments.first() else { return Ok(false); };
        let Some(InterpreterValue::Capability(token)) = values.get(id) else { return Ok(false); };
        if token.kind != DIR_KIND || token.registry.is_none() { return Ok(false); }
        if self.registry.directory_location(token)?.is_some() { return Ok(false); }
        let token = token.clone();
        // No output or namespace mutation precedes complete buffer preflight.
        // Missing names must not turn malformed transport into canned success.
        for (index, argument) in arguments.iter().enumerate().skip(1) {
            if let HostCallArgument::Buffer { place, .. } = argument {
                if place.access() == AccessMode::Read {
                    let bytes = self.host_bytes(place, values)?;
                    let raw = index == 2 && matches!(name, "dir_append_bytes_at" | "dir_write_bytes_at");
                    if !raw && std::str::from_utf8(&bytes).is_err() {
                        return Err(InterpretError::TypeMismatch("host string is not UTF-8"));
                    }
                } else { self.registry_output(place, values)?; }
            }
        }
        match (name, arguments, destinations) {
            ("dir_list_into", [_, HostCallArgument::Buffer { place, .. }], [destination]) => {
                let length = self.fill_host_buffer(place, &[], values)?;
                values.insert(*destination, InterpreterValue::Size(length));
            }
            ("dir_exists_at" | "dir_is_dir_at", [_, HostCallArgument::Buffer { place, .. }], [destination]) => {
                let path = self.host_string(place, values)?;
                if name == "dir_exists_at" { self.host_path("", &path)?; }
                let is_self = dir_create::normalize_path(&path).is_ok_and(|path| path.is_empty());
                values.insert(*destination, if name == "dir_exists_at" { InterpreterValue::Size(u64::from(is_self)) }
                    else { InterpreterValue::Bool(is_self) });
            }
            ("dir_subtree_at", [_, HostCallArgument::Buffer { place, .. }], [destination]) => {
                let path = self.host_path("", &self.host_string(place, values)?)?;
                if !path.is_empty() { return Err(InterpretError::ExplicitTrap); }
                values.insert(*destination, InterpreterValue::Capability(token));
            }
            ("dir_make_dir_at", [_, HostCallArgument::Buffer { place, .. }], []) => {
                let path = dir_create::normalize_path(&self.host_string(place, values)?)?;
                if !path.is_empty() { return Err(InterpretError::ExplicitTrap); }
            }
            _ => return Err(InterpretError::ExplicitTrap),
        }
        Ok(true)
    }
}
