use super::*;
use crate::compiler::operations::{LaneOp, Packed};

fn simd(out: &mut Vec<u8>, op: u32) {
    out.push(0xfd);
    unsigned(out, op);
}

fn pair(out: &mut Vec<u8>, words: &[u32]) {
    get(out, words[0]);
    simd(out, 0x12); // i64x2.splat
    get(out, words[1]);
    simd(out, 0x1e);
    out.push(1); // i64x2.replace_lane
}

fn low32(out: &mut Vec<u8>) {
    out.push(0x42);
    signed(out, i64::from(u32::MAX));
    out.push(0x83); // i64.and
}

fn bias64(out: &mut Vec<u8>) {
    simd(out, 0x0c);
    out.extend((1u64 << 63).to_le_bytes().repeat(2));
    simd(out, 0x51);
}

impl Emitter<'_> {
    pub(super) fn packed(&mut self, out: &mut Vec<u8>, op: Packed, args: &[Expr]) {
        if op.both {
            // The scalar Wasm profile keeps each lane as an ordinary binary64 value.
            let instruction = match op.op {
                LaneOp::FloatAdd => 0xa0,
                LaneOp::FloatMul => 0xa2,
                LaneOp::FloatDiv => 0xa3,
                _ => unreachable!(),
            };
            let locals = args.iter().map(|arg| self.capture(out, arg)).collect::<Vec<_>>();
            for lane in 0..2 {
                get(out, locals[lane]);
                get(out, locals[lane + 2]);
                out.push(instruction);
            }
            return;
        }
        let mut words = Vec::new();
        for arg in args {
            self.expr(out, arg);
            let local = self.temporary(Ty::U64);
            indexed(out, 0x21, local);
            words.push(local);
        }
        pair(out, &words);
        if op.op == LaneOp::Mask {
            simd(out, 0x64);
            out.push(0xad);
            return;
        }
        let bias = op.op == LaneOp::Gt && op.bits == 64;
        if bias {
            bias64(out);
        }
        pair(out, &words[2..]);
        if bias {
            bias64(out);
        }
        if op.op == LaneOp::EqMask {
            simd(out, 0x23); // i8x16.eq
            simd(out, 0x64); // i8x16.bitmask
            out.push(0xad); // i64.extend_i32_u
            return;
        }
        if op.op == LaneOp::Mul && op.bits == 8 {
            simd(out, 0x9e); // i16x8.extmul_low_i8x16_u
            pair(out, &words);
            pair(out, &words[2..]);
            simd(out, 0x9f);
            simd(out, 0x0d);
            out.extend((0u8..32).step_by(2));
        } else {
            let ops = match op.op {
                LaneOp::Add => [0x6e, 0x8e, 0xae, 0xce],
                LaneOp::Sub => [0x71, 0x91, 0xb1, 0xd1],
                LaneOp::Mul => [0, 0x95, 0xb5, 0xd5],
                LaneOp::Eq => [0x23, 0x2d, 0x37, 0xd6],
                LaneOp::Gt => [0x28, 0x32, 0x3c, 0xd9],
                LaneOp::And => [0x4e; 4],
                LaneOp::Or => [0x50; 4],
                LaneOp::Xor => [0x51; 4],
                LaneOp::FloatAdd | LaneOp::FloatMul | LaneOp::FloatDiv => unreachable!(),
                LaneOp::Mask | LaneOp::EqMask => unreachable!(),
            };
            simd(out, ops[(op.bits.trailing_zeros() - 3) as usize]);
        }
        simd(out, 0x1d);
        out.push(u8::from(op.high));
    }

    pub(super) fn mul_high(&mut self, out: &mut Vec<u8>, args: &[Expr]) {
        // Split into 32-bit halves. The carry into the top word is the sum
        // of the middle partial products and the top half of the low product.
        let mut halves = Vec::new();
        for arg in args {
            self.expr(out, arg);
            let word = self.temporary(Ty::U64);
            indexed(out, 0x21, word);
            for high in [false, true] {
                get(out, word);
                if high {
                    out.extend([0x42, 32, 0x88]);
                } else {
                    low32(out);
                }
                let half = self.temporary(Ty::U64);
                indexed(out, 0x21, half);
                halves.push(half);
            }
        }
        let mut products = Vec::new();
        for (a, b) in [(0, 2), (0, 3), (1, 2), (1, 3)] {
            get(out, halves[a]);
            get(out, halves[b]);
            out.push(0x7e);
            let p = self.temporary(Ty::U64);
            indexed(out, 0x21, p);
            products.push(p);
        }
        get(out, products[0]);
        out.extend([0x42, 32, 0x88]);
        for p in &products[1..3] {
            get(out, *p);
            low32(out);
            out.push(0x7c);
        }
        out.extend([0x42, 32, 0x88]);
        for p in &products[1..3] {
            get(out, *p);
            out.extend([0x42, 32, 0x88, 0x7c]);
        }
        get(out, products[3]);
        out.push(0x7c);
    }
}
