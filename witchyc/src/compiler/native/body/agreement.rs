use super::{Body, Result, Value, error, storage::Runtime};
use crate::compiler::{
    host::{Host, HostOp},
    logical_body::{Expr, Ty, Value as Logical},
    logical_place::Place,
};

impl Body<'_, '_> {
    pub(super) fn p384_operation(
        &mut self,
        op: HostOp,
        receiver: Option<&Place>,
        operands: &[Expr],
    ) -> Result<Value> {
        let random = Ty::Host(Host::Rand);
        let ephemeral = Ty::Host(Host::P384Key);
        let (function, handle, slices): (_, _, &[_]) = match (op, receiver, operands) {
            (HostOp::RandP384Start, Some(source), [Expr { kind: Logical::U64(0), .. }])
                if source.ty == random => (Runtime::P384Start, self.p384_receiver(source)?, &[]),
            (HostOp::RandP384Agree, Some(source), [_, _, _])
                if source.ty == random => (Runtime::P384Agree, self.p384_receiver(source)?, operands),
            (HostOp::RandP384Public, Some(key), [_])
                if key.ty == ephemeral => (Runtime::P384Public, self.p384_receiver(key)?, operands),
            (HostOp::RandP384Finish, None, [key, slices @ ..])
                if key.ty == ephemeral && slices.len() == 2 => (Runtime::P384Finish, self.one(key)?, slices),
            (HostOp::RandP384Close, None, [key, Expr { kind: Logical::U64(0), .. }])
                if key.ty == ephemeral => (Runtime::P384Close, self.one(key)?, &[]),
            _ => return Err(error("invalid native P-384 operation")),
        };
        let mut arguments = Vec::with_capacity(1 + 2 * slices.len());
        arguments.push(handle);
        for operand in slices {
            let (address, length) = self.slice(operand)?;
            arguments.extend([address, length]);
        }
        self.runtime_value(function, &arguments)
    }

    fn p384_receiver(&mut self, place: &Place) -> Result<Value> {
        let location = self.locate(place)?;
        Ok(self.read_location(location)?[0])
    }
}
