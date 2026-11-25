use super::{Body, I64, InstBuilder, MemFlagsData, Result, Ty, Value};

impl Body<'_, '_> {
    /// Scalar expressions can consume a float directly. Flattened aggregates
    /// and the internal word ABI still use the existing integer carriers.
    pub(super) fn load_expression(&mut self, addr: Value, ty: Ty) -> Result<Vec<Value>> {
        if ty == Ty::F64 {
            let float = self.fb.ins().load(super::F64, MemFlagsData::new(), addr, 0);
            return Ok(vec![float]);
        }
        self.load_typed(addr, ty)
    }

    /// Loads into the integer lanes used by the internal ABI. A narrow unsigned
    /// value occupies only its declared bytes even when reached through a loan.
    pub(super) fn load_typed(&mut self, addr: Value, ty: Ty) -> Result<Vec<Value>> {
        let storage = self.layout.storage(ty)?;
        Ok(storage
            .cells
            .iter()
            .map(|cell| {
                let width = cell.integer_type();
                let value =
                    self.fb
                        .ins()
                        .load(width, MemFlagsData::new(), addr, cell.offset as i32);
                if width == I64 {
                    value
                } else {
                    self.fb.ins().uextend(I64, value)
                }
            })
            .collect())
    }

    pub(super) fn store_typed(&mut self, addr: Value, ty: Ty, lanes: &[Value]) -> Result<()> {
        let storage = self.layout.storage(ty)?;
        debug_assert_eq!(storage.cells.len(), lanes.len());
        for (cell, &value) in storage.cells.iter().zip(lanes) {
            let width = cell.integer_type();
            let value = if width == I64 {
                value
            } else {
                self.fb.ins().ireduce(width, value)
            };
            self.fb
                .ins()
                .store(MemFlagsData::new(), value, addr, cell.offset as i32);
        }
        Ok(())
    }

    /// A sum's ABI payload is a union of words. The selected arm has exactly
    /// the same byte layout here as it does when borrowed from the stored sum.
    pub(super) fn pack_arm(&mut self, ty: Ty, lanes: Vec<Value>) -> Result<Vec<Value>> {
        let storage = self.layout.storage(ty)?;
        debug_assert_eq!(storage.cells.len(), lanes.len());
        if storage
            .cells
            .iter()
            .enumerate()
            .all(|(i, c)| c.offset == i as u32 * 8)
        {
            return Ok(lanes);
        }
        let mut words: Vec<Option<Value>> = vec![None; storage.size.div_ceil(8) as usize];
        for (cell, lane) in storage.cells.iter().zip(lanes) {
            let shift = (cell.offset % 8) * 8;
            let bits = if shift == 0 {
                lane
            } else {
                self.fb.ins().ishl_imm_s(lane, i64::from(shift))
            };
            let word = &mut words[(cell.offset / 8) as usize];
            *word = Some(match *word {
                None => bits,
                Some(prefix) => self.fb.ins().bor(prefix, bits),
            });
        }
        Ok(words
            .into_iter()
            .map(|v| v.unwrap_or_else(|| self.constant(0)))
            .collect())
    }

    pub(super) fn unpack_arm(&mut self, ty: Ty, words: &[Value]) -> Result<Vec<Value>> {
        let storage = self.layout.storage(ty)?;
        let mut lanes = Vec::with_capacity(storage.cells.len());
        for cell in &storage.cells {
            let bits = words[(cell.offset / 8) as usize];
            let shift = (cell.offset % 8) * 8;
            let bits = if shift == 0 {
                bits
            } else {
                self.fb.ins().ushr_imm_s(bits, i64::from(shift))
            };
            let width = cell.integer_type();
            lanes.push(if width == I64 {
                bits
            } else {
                let low = self.fb.ins().ireduce(width, bits);
                self.fb.ins().uextend(I64, low)
            });
        }
        Ok(lanes)
    }
}
