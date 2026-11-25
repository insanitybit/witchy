//! Successful-path observations only; never included in proof identities or receipts.
#[derive(Default, Debug)]
pub(super) struct Measurements {
    pub goal_resolution_ns: u128,
    pub module_build_ns: u128,
    pub compiler_hash_a_ns: u128,
    pub compiler_hash_b_ns: u128,
    pub configuration_ns: u128,
    pub installation_hash_a_ns: u128,
    pub installation_hash_b_ns: u128,
    pub scratch_setup_write_ns: u128,
    pub child_spawn_to_exit_ns: u128,
    /// The longest single checker request: what the checker's per-request deadline bounds.
    pub slowest_request_ns: u128,
    pub output_audit_parse_ns: u128,
    pub selected_goals: usize,
    pub generated_module_bytes: usize,
    pub statement_bytes: usize,
    pub checked_nodes: usize,
    pub compiler_hashes: usize,
    pub installation_snapshots: usize,
}
impl Measurements {
    pub(super) fn json(&self) -> String {
        format!(concat!(
            "{{\"goal_resolution\":{},\"module_build\":{},\"compiler_hash_a\":{},\"compiler_hash_b\":{},",
            "\"configuration\":{},\"installation_hash_a\":{},\"installation_hash_b\":{},",
            "\"scratch_setup_write\":{},\"child_spawn_to_exit\":{},\"slowest_request\":{},\"output_audit_parse\":{},",
            "\"counts\":{{\"selected_goals\":{},\"generated_module_bytes\":{},\"statement_bytes\":{},",
            "\"checked_nodes\":{},\"compiler_hashes\":{},\"installation_snapshots\":{}}}}}"),
            self.goal_resolution_ns, self.module_build_ns, self.compiler_hash_a_ns, self.compiler_hash_b_ns,
            self.configuration_ns, self.installation_hash_a_ns, self.installation_hash_b_ns,
            self.scratch_setup_write_ns, self.child_spawn_to_exit_ns, self.slowest_request_ns, self.output_audit_parse_ns,
            self.selected_goals, self.generated_module_bytes, self.statement_bytes,
            self.checked_nodes, self.compiler_hashes, self.installation_snapshots)
    }
}
