use super::*;

mod prewarm {
    include!("gate_behavior/prewarm.rs");
}

mod state_and_gate {
    include!("gate_behavior/state_and_gate.rs");
}

mod canary {
    include!("gate_behavior/canary.rs");
}

mod preflight_reuse {
    include!("gate_behavior/preflight_reuse.rs");
}
