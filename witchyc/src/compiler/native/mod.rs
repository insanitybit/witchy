//! Native lowering starts from selected logical instances after source admission.
mod body;
mod inline;
mod layout;
mod link;
#[cfg(target_os = "macos")]
pub(super) use link::configure_qualification;
#[cfg(witchy_native_aws_lc)]
#[path = "../../../runtime/native/tls_grant.rs"]
mod tls_grant;

pub(super) use link::link;

use super::{
    Gate, analyze_gated, check_nopanic, concrete, grant_document::LaunchPlan, instances, proof_measure, proof_session,
    scalar_vc::{INDEX_PROVED, SLICE_PROVED}, source_vc, verify_build,
};
use crate::diagnostic::Diagnostic;
use cranelift_codegen::{
    ir::{AbiParam, Signature, types::{F64X2, I64}},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::{collections::BTreeSet, time::Instant};

/// The native runtime's brokered grants (directories, files, input, network, Exec, ...) exist on
/// macOS and Linux.
pub(super) const BROKERED: bool = cfg!(any(target_os = "macos", target_os = "linux"));

pub(super) const TARGET: &str = if cfg!(target_os = "macos") {
    "macos-arm64"
} else {
    "linux-arm64"
};

type Result<T> = std::result::Result<T, Diagnostic>;

fn error(message: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(
        "W0401",
        0..0,
        format!("{TARGET} native target: {message}"),
    )
}

#[derive(Default)]
pub(super) struct Times {
    pub read_ns: u128,
    parse_ns: u128,
    check_ns: u128,
    vc_ns: u128,
    pub proof_ns: u128,
    lean_invocations: usize,
    proof: proof_measure::Measurements,
    plan_ns: u128,
    lower_ns: u128,
    codegen_ns: u128,
    object_ns: u128,
    link_ns: u128,
    pub publish_ns: u128,
    functions: usize,
}

impl Times {
    pub fn json(&self) -> String {
        format!(
            concat!(
                "{{\"read_ns\":{},\"parse_ns\":{},\"check_ns\":{},\"vc_ns\":{},",
                "\"proof_ns\":{},\"lean_invocations\":{},\"plan_ns\":{},\"lower_ns\":{},",
                "\"codegen_ns\":{},\"object_ns\":{},\"link_ns\":{},\"publish_ns\":{},\"functions\":{},\"proof_detail\":{}}}"
            ),
            self.read_ns,
            self.parse_ns,
            self.check_ns,
            self.vc_ns,
            self.proof_ns,
            self.lean_invocations,
            self.plan_ns,
            self.lower_ns,
            self.codegen_ns,
            self.object_ns,
            self.link_ns,
            self.publish_ns,
            self.functions,
            self.proof.json(),
        )
    }
}

/// A launch manifest selected for the build: its text and SHA-256.
pub(super) struct Manifest<'a> { pub text: &'a str, pub digest: &'a str }

pub(super) struct NativeObject {
    bytes: Vec<u8>,
    requires_aws_lc: bool,
}

pub(super) fn compile(
    source: &str,
    modules: &[usize],
    mut session: Option<&mut proof_session::Session>,
    gate: Gate,
    manifest: Option<Manifest>,
) -> Result<(NativeObject, Times)> {
    let (checked, measured) =
        analyze_gated(source, modules, session.as_deref_mut(), Some(gate), None,
            if cfg!(target_os = "macos") { crate::parser::Target::MacosArm64 } else { crate::parser::Target::LinuxArm64 },
        )?;
    // The manifest is checked against this exact checked `main`, before any proof work.
    let launch = match manifest {
        None => None,
        Some(manifest) => Some({
            let plan = super::launch_plan_checked(&checked.syntax, manifest.text, manifest.digest)
                .map_err(|errors| error(format_args!("the launch manifest does not fit main: {}", errors.join("; "))))?;
            // Linux ARM64 grants, in a launch plan, only what its native runtime has at all
            // (docs/native-trusted-executables.md, "Linux: the work list").
            if !cfg!(target_os = "macos") {
                for line in String::from_utf8_lossy(&plan.wire).lines().skip(1) {
                    let tag = line.split('\t').next().unwrap_or("");
                    // What Linux implements, by name (review of round 5: a list of refusals
                    // would admit any record added later). Secrets need the AWS-LC FIPS module,
                    // linked only into a compiler built with it (WITCHY_NATIVE_AWS_LC_ROOT).
                    let secrets = cfg!(witchy_native_aws_lc) && matches!(tag, "secret" | "store" | "object");
                    if !secrets && !matches!(tag, "flag" | "output" | "allocator" | "clock" | "rand" | "env" | "args" | "grant" | "net"
                        | "listen" | "socket" | "dir" | "file" | "input" | "exec" | "exec-env" | "write-file" | "limit" | "identity" | "end") {
                        return Err(error(format_args!("a launch plan's `{tag}` grant is not implemented for linux-arm64 (native secrets need a compiler built with the AWS-LC FIPS module)")));
                    }
                }
            }
            plan
        }),
    };
    let phase = Instant::now();
    let goals = source_vc::prepare_with_sites(&checked.logical)?;
    let mut times = Times {
        parse_ns: measured.parse_ns,
        check_ns: measured.primary_typed_check_ns,
        vc_ns: phase.elapsed().as_nanos(),
        ..Times::default()
    };
    let catalog = super::proof_reach::Catalog::of(source, &checked.syntax, &checked.logical.functions);
    let (syntax, mut logical) = checked.into_parts();
    let parameters = logical.main_parameters;
    let secrets = std::mem::take(&mut logical.secret_slots);
    let promises = logical
        .functions
        .iter()
        .filter_map(|f| Some((f.definition.0, f.nopanic.clone()?)))
        .collect();
    // Instance planning consumes the source call edges. Use them only to schedule
    // likely leaves first. The finished CLIF determines eligibility.
    let source_leaves: Vec<_> = logical.functions.iter().map(|f| f.calls.is_empty()).collect();
    let phase = Instant::now();
    let plan = instances::prepare(logical)?;
    let grants = entry_grants(&plan)?;
    times.plan_ns = phase.elapsed().as_nanos();

    let phase = Instant::now();
    // What the object can run, and the program's own files, are proved (proof_reach.rs).
    let unreached = catalog.unreached(&plan.instances.iter().map(|i| i.definition.0).collect());
    let (goals, left_out) = unreached.select(goals);
    let proved = goals.proved_arithmetic.clone();
    let proofs = verify_build(&syntax, source, goals, session, &left_out)?;
    times.proof_ns = phase.elapsed().as_nanos();
    times.lean_invocations = proofs.invocations;
    times.proof = proofs.measurements;
    let phase = Instant::now();
    if plan.instances.iter().any(|i| i.function.nopanic.is_some()) {
        // The shared trap analysis still consumes Wasm's plan.
        let mut analysis = concrete::from_plan(plan.clone(), parameters, secrets.clone())?;
        check_nopanic(&mut analysis, &proved, &promises)?;
    }
    times.plan_ns += phase.elapsed().as_nanos();
    let object = object(plan, grants, &secrets, launch.as_ref(), &proved, &source_leaves, &mut times)?;
    Ok((object, times))
}

// Internal ARM64 calls can return a pair directly. Larger flattened results
// keep the caller-owned result area to limit live registers across call sites.
const DIRECT_RESULT_WORDS: u32 = 2;

/// Checked mode (`WITCHY_NATIVE_CHECKED=1` when compiling): the executable keeps every test a
/// proof lets the default one leave out, so the two can be run against each other
/// (scripts/native-checked.py). Arithmetic, array indexes and slice ranges are tested as in an
/// unproved function (an element read that a proved length left untested too,
/// body/expression.rs), and every cell access is the runtime's checked call (body/cells.rs). A
/// change that removes another proved test adds it here.
pub(super) fn checked() -> bool {
    std::env::var_os("WITCHY_NATIVE_CHECKED").is_some()
}

struct Function {
    id: FuncId,
    signature: Signature,
    output_lanes: u32,
    float_pair_result: bool,
    result: super::logical_body::Ty,
    proved_arithmetic: bool,
    proved_array_bounds: bool,
    proved_slice_bounds: bool,
}

impl Function {
    fn indirect_result(&self) -> bool {
        self.output_lanes > DIRECT_RESULT_WORDS && !self.float_pair_result
    }
}

#[cfg(witchy_native_aws_lc)]
const SIGNER_OR_SECRET_GRANT: u64 = 32768;
#[cfg(witchy_native_aws_lc)]
const SECRET_STORE_GRANT: u64 = 65536;
const RAND_GRANT: u64 = 32;

fn host_grant(host: super::host::Host) -> Option<u64> {
    use super::host::{Host, Right};

    if !BROKERED {
        return None;
    }
    // Native secrets need the AWS-LC FIPS module, linked only into a compiler built with it
    // (the arms below are compiled only then).
    Some(match host {
        Host::Net => 4,
        Host::Listener => 512,
        Host::Socket => 131072,
        Host::Dir(Right::Read) => 1024,
        Host::Dir(Right::Write) => 4096,
        Host::Dir(Right::All) => 8192,
        Host::Clock => 16,
        Host::Rand => RAND_GRANT,
        Host::Args => 64,
        Host::Env => 128,
        Host::Grants => 16384,
        #[cfg(witchy_native_aws_lc)]
        Host::Signer | Host::Secret(Right::Read | Right::Write | Right::All) => SIGNER_OR_SECRET_GRANT,
        #[cfg(witchy_native_aws_lc)]
        Host::TlsSigner => tls_grant::TLS_SIGNER,
        #[cfg(witchy_native_aws_lc)]
        Host::SecretStore => SECRET_STORE_GRANT,
        Host::Exec => 262144,
        Host::File(Right::Read) => 256,
        Host::File(Right::Write) => 2048,
        _ => return None,
    })
}

fn entry_grants(plan: &instances::Plan) -> Result<u64> {
    use super::logical_body::{Stmt, Value};

    let entry = &plan.instances[plan.main as usize].function;
    let mut grants = 0;
    // The front end emits synthetic grant bindings first. A grantable
    // record may expand into one policy handle plus local field lookups.
    for statement in &entry.body {
        let Stmt::Set(_, value) = statement else {
            break;
        };
        grants |= match value.kind {
            Value::EntryConsole => 1,
            Value::EntryInput if BROKERED => 8,
            Value::EntryInput => return Err(error("Console<Read> has no native launch grant")),
            Value::EntryAllocator => 2,
            Value::EntryHost(host, _) => host_grant(host)
                .ok_or_else(|| error(format_args!("{} has no native launch grant", host.name())))?,
            _ => break,
        };
    }
    Ok(grants)
}

fn object(
    plan: instances::Plan,
    grants: u64,
    secrets: &[(String, u8)],
    launch: Option<&LaunchPlan>,
    proved: &BTreeSet<u32>,
    source_leaves: &[bool],
    times: &mut Times,
) -> Result<NativeObject> {
    let mut requires_aws_lc = !secrets.is_empty() || launch.is_some_and(|plan| plan.requires_aws_lc);
    #[cfg(witchy_native_aws_lc)]
    {
        let provider_grants = SIGNER_OR_SECRET_GRANT | SECRET_STORE_GRANT | tls_grant::TLS_SIGNER | RAND_GRANT;
        requires_aws_lc |= grants & provider_grants != 0;
    }
    let phase = Instant::now();
    let mut flags = settings::builder();
    flags.set("is_pic", "true").map_err(error)?;
    // Cranelift's default is no mid-end at all: constants unfolded, loads repeated.
    flags.set("opt_level", "speed").map_err(error)?;
    flags.set("enable_probestack", "true").map_err(error)?;
    flags.set("probestack_strategy", "inline").map_err(error)?;
    // The default 4 KiB probe interval also works with larger host pages.
    let isa = cranelift_codegen::isa::lookup(target_lexicon::Triple::host())
        .map_err(error)?
        .finish(settings::Flags::new(flags))
        .map_err(error)?;
    let builder = ObjectBuilder::new(isa, "witchy", cranelift_module::default_libcall_names())
        .map_err(error)?;
    let mut module = ObjectModule::new(builder);
    let grant = module
        .declare_data("witchy_entry_grants", Linkage::Export, false, false)
        .map_err(error)?;
    let mut data = DataDescription::new();
    data.set_align(8);
    data.define(Box::new(grants.to_le_bytes()));
    module.define_data(grant, &data).map_err(error)?;

    {
        // The checked launch plan, read-only and NUL-terminated; a lone NUL is a development
        // build, whose launch reads the raw grant flags instead.
        let plan = module
            .declare_data("witchy_launch_plan", Linkage::Export, false, false)
            .map_err(error)?;
        let mut bytes = launch.map(|plan| plan.wire.clone()).unwrap_or_default();
        if bytes.contains(&0) {
            return Err(error("a launch plan holds no NUL byte"));
        }
        bytes.push(0);
        let mut data = DataDescription::new();
        data.define(bytes.into_boxed_slice());
        module.define_data(plan, &data).map_err(error)?;
    }
    {
        let slots = module
            .declare_data("witchy_secret_slots", Linkage::Export, false, false)
            .map_err(error)?;
        // Zero terminates the table; each other tag is the checked kind plus one,
        // followed by the NUL-terminated parameter name.
        let mut bytes = Vec::new();
        for (name, kind) in secrets {
            bytes.push(kind + 1);
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
        let mut data = DataDescription::new();
        data.define(bytes.into_boxed_slice());
        module.define_data(slots, &data).map_err(error)?;
    }

    let mut print = module.make_signature();
    print.params.resize(3, AbiParam::new(I64));
    let print = module
        .declare_function("witchy_print", Linkage::Import, &print)
        .map_err(error)?;
    let layout = layout::Layout::new(&plan.types);
    let functions = plan
        .instances
        .iter()
        .enumerate()
        .map(|(i, instance)| {
            let function = &instance.function;
            let mut signature = module.make_signature();
            let mut inputs = 0;
            for ty in &function.params {
                inputs += layout.lanes(*ty)?;
            }
            let output_lanes = layout.lanes(function.result)?;
            if inputs > body::MAX_FRAME_BYTES / body::LANE_BYTES {
                return Err(error(
                    "function arguments exceed the 64 KiB native frame limit",
                ));
            }
            let float_pair_result = layout.float_pair(function.result)?;
            let (name, linkage) = if i == plan.main as usize {
                // The runtime provides one mask of checked launch grants.
                signature.params.push(AbiParam::new(I64));
                ("witchy_entry".into(), Linkage::Export)
            } else {
                if output_lanes > DIRECT_RESULT_WORDS && !float_pair_result {
                    signature.params.push(AbiParam::new(I64));
                }
                for ty in &function.params {
                    if layout.float_pair(*ty)? {
                        signature.params.push(AbiParam::new(F64X2));
                    } else {
                        signature.params.extend((0..layout.lanes(*ty)?).map(|_| AbiParam::new(I64)));
                    }
                }
                // WITCHY_NATIVE_NAMES=1 adds each function's source name to its symbol, so a
                // sampling profiler's report can be read. Symbols are local: nothing links by them.
                let named = std::env::var_os("WITCHY_NATIVE_NAMES").is_some();
                let label: String = function.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                (if named { format!("witchy_function_{i}_{label}") } else { format!("witchy_function_{i}") }, Linkage::Local)
            };
            if float_pair_result {
                signature.returns.push(AbiParam::new(F64X2));
            } else if output_lanes <= DIRECT_RESULT_WORDS {
                signature.returns.resize(output_lanes as usize, AbiParam::new(I64));
            }
            let id = module
                .declare_function(&name, linkage, &signature)
                .map_err(error)?;
            Ok(Function {
                id,
                signature,
                output_lanes,
                float_pair_result,
                result: function.result,
                proved_arithmetic: !checked() && proved.contains(&instance.definition.0),
                proved_array_bounds: !checked() && proved.contains(&(instance.definition.0 | INDEX_PROVED)),
                proved_slice_bounds: !checked() && proved.contains(&(instance.definition.0 | SLICE_PROVED)),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    times.lower_ns = phase.elapsed().as_nanos();

    let mut builder = FunctionBuilderContext::new();
    let mut candidates = inline::Candidates::default();
    let mut literals = std::collections::BTreeMap::new();
    let mut context = module.make_context();
    let (first, rest): (Vec<_>, Vec<_>) = plan.instances
        .iter()
        .enumerate()
        .partition(|(_, instance)| source_leaves[instance.definition.0 as usize]);
    // Instances are discovered from callers. Lower the remaining instances
    // in reverse discovery order to make small wrappers available to callers.
    for (i, instance) in first.into_iter().chain(rest.into_iter().rev()) {
        let phase = Instant::now();
        let function = &functions[i];
        context.func.signature = function.signature.clone();
        let builder = FunctionBuilder::new(&mut context.func, &mut builder);
        let lowered = body::lower(
            builder,
            &mut module,
            &mut literals,
            &functions,
            print,
            &layout,
            &instance.function,
            function,
            i == plan.main as usize,
        )?;
        requires_aws_lc |= lowered.requires_aws_lc;
        let frame = lowered.frame_bytes;
        if i != plan.main as usize {
            candidates.consider(function.id, &context.func, frame, function.indirect_result());
        }
        times.lower_ns += phase.elapsed().as_nanos();

        let phase = Instant::now();
        candidates.inline(&mut context, frame, function.id)?;
        module
            .define_function(function.id, &mut context)
            .map_err(error)?;
        module.clear_context(&mut context);
        times.codegen_ns += phase.elapsed().as_nanos();
        times.functions += 1;
    }
    let phase = Instant::now();
    let bytes = module.finish().emit().map_err(error)?;
    times.object_ns = phase.elapsed().as_nanos();
    Ok(NativeObject { bytes, requires_aws_lc })
}

#[cfg(test)]
#[path = "../../../runtime/native/signer_binding.rs"]
mod signer_binding;

#[cfg(test)]
#[path = "../../../runtime/native/env_names.rs"]
mod env_names;
