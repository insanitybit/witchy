//! Compiler-owned Glamour template-plan metadata.
//!
//! The `html` and `jsx` tags expand to ordinary checked Witchy calls. This pass
//! authenticates their definition sites, validates the generated stable plan
//! records, and joins them to the linker-retained tag origins. Browser tooling
//! and later protocol lowering consume this registry instead of reparsing
//! rendered source.

use super::CodegenError;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use witchy_syntax::ast::{
    BinOp, Block, Convention, Expr, Function, Item, MatchArm, Module, Param, Pattern, Stmt,
    Type,
};
use witchy_syntax::origin::{GeneratedNodeId, SourceSpan};
use witchy_types::runtime_type::{
    DeclarationIdentity, DeclarationKind, PackageSource, PrimitiveType, RuntimeConvention,
    RuntimeDeclarationCatalog, RuntimeTypeIdentity,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlamourTemplateSlotMetadata {
    pub index: u32,
    pub wire_id: u32,
    pub node: u32,
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlamourTemplateAttributeMetadata {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlamourTemplateNodeMetadata {
    Element {
        node: u32,
        tag: String,
        attributes: Vec<GlamourTemplateAttributeMetadata>,
        children: Vec<GlamourTemplateNodeMetadata>,
    },
    Text {
        node: u32,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlamourTemplateOriginMetadata {
    pub id: GeneratedNodeId,
    pub tag: DeclarationIdentity,
    pub invocation: SourceSpan,
    pub holes: Vec<SourceSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlamourTemplateMetadata {
    pub identity: String,
    pub wire_id: u32,
    pub slots: Vec<GlamourTemplateSlotMetadata>,
    pub root: GlamourTemplateNodeMetadata,
    pub origins: Vec<GlamourTemplateOriginMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourIslandMetadata {
    pub key: String,
    pub source_identity: String,
    pub mode: String,
    pub identity: String,
    pub wire_id: u32,
    pub registry_id: u32,
    pub program: DeclarationIdentity,
    pub program_name: String,
    pub authorize_name: String,
    pub initial_name: String,
    pub start_name: String,
    pub update_name: String,
    pub view_name: String,
    pub subscriptions_name: String,
    pub static_view: DeclarationIdentity,
    pub auth_type: Type,
    pub model_type: Type,
    pub message_type: Type,
    pub activation: String,
    pub media: Option<String>,
    pub prefetch: String,
    pub prefetch_media: Option<String>,
    pub diagnostic_name: Option<String>,
    pub work: Vec<GlamourWorkMetadata>,
    pub work_maps: Vec<GlamourWorkMapMetadata>,
    pub mapped_work: Vec<GlamourMappedWorkMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GlamourDevelopmentMigrationCodec {
    pub model_schema: [u8; 32],
    pub source_type: Type,
    pub decoder: String,
    pub migration: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GlamourDevelopmentCodecSpec {
    pub state_type: Type,
    pub encoder: String,
    pub decoder: String,
    pub migrations: Vec<GlamourDevelopmentMigrationCodec>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlamourBrowserPolicyMetadata {
    Fetch {
        scope: String,
        methods: String,
        prefix: String,
    },
    Navigation {
        base: String,
        rights: String,
    },
    Timer {
        minimum: i64,
    },
    Storage {
        provider: String,
        namespace: String,
        key_prefix: String,
        max_value_bytes: i64,
    },
    Worker {
        name: String,
        max_request_bytes: i64,
        max_result_bytes: i64,
        max_concurrency: i64,
        timeout_ms: i64,
    },
    HostPort {
        adapter: String,
        endpoint: String,
        max_request_bytes: i64,
        max_result_bytes: i64,
    },
    Port {
        name: String,
    },
    SecretField {
        form: String,
        field: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourWorkMetadata {
    pub channel: String,
    pub kind: String,
    pub handler: String,
    pub owner_name: String,
    pub owner: DeclarationIdentity,
    pub ordinal: u32,
    pub call_name: String,
    pub descriptor_id: u32,
    pub result_schema_id: u32,
    pub completion_id: u32,
    pub owner_scope_id: u32,
    pub browser_policy: GlamourBrowserPolicyMetadata,
    pub completion_source: String,
    pub completion: Expr,
    pub completion_message_type: Type,
    pub completion_captures: Vec<GlamourWorkCaptureMetadata>,
    pub worker_task: Option<GlamourWorkerTaskMetadata>,
    pub host_port: Option<GlamourHostPortMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourWorkerTaskMetadata {
    pub source_name: String,
    pub declaration: DeclarationIdentity,
    pub request_type: Type,
    pub result_type: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourHostPortMetadata {
    pub request_type: Type,
    pub result_type: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourWorkMapMetadata {
    pub channel: String,
    pub owner_name: String,
    pub ordinal: u32,
    pub mapper_id: u32,
    pub mapper_source: String,
    pub mapper: Expr,
    pub input_type: Type,
    pub output_type: Type,
    pub captures: Vec<GlamourWorkCaptureMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourMappedWorkMetadata {
    pub channel: String,
    pub kind: String,
    pub handler: String,
    pub owner_name: String,
    pub owner: DeclarationIdentity,
    pub mapper_id: u32,
    pub mapper_source: String,
    pub mapper: Expr,
    pub mapper_captures: Vec<GlamourWorkCaptureMetadata>,
    pub input_type: Type,
    pub output_type: Type,
    pub previous_descriptor_id: u32,
    pub previous_completion_id: u32,
    pub descriptor_id: u32,
    pub result_schema_id: u32,
    pub completion_id: u32,
    pub owner_scope_id: u32,
    pub browser_policy: GlamourBrowserPolicyMetadata,
    pub composition: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlamourWorkCaptureMetadata {
    pub name: String,
    pub ty: Type,
}

#[derive(Clone)]
struct Candidate {
    identity: String,
    slots: Vec<GlamourTemplateSlotMetadata>,
    root: GlamourTemplateNodeMetadata,
}

struct ObservedSlot {
    node: u32,
    kind: String,
    name: String,
}

pub fn checked_glamour_templates(
    checked: &witchy_types::pipeline::CheckedModule,
) -> Result<Vec<GlamourTemplateMetadata>, CodegenError> {
    let catalog = checked
        .runtime_declaration_catalog()
        .map_err(|error| metadata_error(error.to_string()))?;
    let Some(template_plan) =
        catalog.resolve("glamour.TemplatePlan", DeclarationKind::Type)
    else {
        return Ok(Vec::new());
    };
    if !is_glamour_declaration(template_plan, "TemplatePlan") {
        return Err(metadata_error(
            "`glamour.TemplatePlan` is not owned by the toolchain Glamour package",
        ));
    }

    let mut eligible = BTreeMap::<String, Vec<GlamourTemplateOriginMetadata>>::new();
    for origin in checked.origins().tagged_literals() {
        let Some(tag) = catalog.resolve(&origin.tag, DeclarationKind::Function) else {
            return Err(metadata_error(format!(
                "tagged literal `{}` has no authenticated declaration",
                origin.tag
            )));
        };
        if !matches!(tag.name(), "html" | "jsx") || !is_glamour_declaration(tag, tag.name()) {
            continue;
        }
        eligible
            .entry(origin_key(&origin.invocation))
            .or_default()
            .push(GlamourTemplateOriginMetadata {
                id: origin.id.clone(),
                tag: tag.clone(),
                invocation: origin.invocation.clone(),
                holes: origin.holes.clone(),
            });
    }
    if eligible.is_empty() {
        return Ok(Vec::new());
    }
    for origins in eligible.values_mut() {
        origins.sort_by(|left, right| {
            left.id
                .module
                .cmp(&right.id.module)
                .then_with(|| left.id.ordinal.cmp(&right.id.ordinal))
        });
    }

    let mut candidates = BTreeMap::<String, Vec<Candidate>>::new();
    visit_module_exprs(checked.module(), &mut |expression| {
        let Some((key, candidate)) = template_candidate(expression)? else {
            return Ok(());
        };
        if eligible.contains_key(&key) {
            candidates.entry(key).or_default().push(candidate);
        }
        Ok(())
    })?;

    let mut plans = BTreeMap::<
        String,
        (
            Vec<GlamourTemplateSlotMetadata>,
            GlamourTemplateNodeMetadata,
            Vec<GlamourTemplateOriginMetadata>,
        ),
    >::new();
    for (key, origins) in eligible {
        let generated = candidates.remove(&key).unwrap_or_default();
        if generated.len() != origins.len() {
            return Err(metadata_error(format!(
                "Glamour tag expansion at `{key}` produced {} checked template plan(s), expected {}",
                generated.len(),
                origins.len(),
            )));
        }
        for (candidate, origin) in generated.into_iter().zip(origins) {
            match plans.get_mut(&candidate.identity) {
                Some((slots, root, plan_origins)) => {
                    if slots != &candidate.slots || root != &candidate.root {
                        return Err(metadata_error(format!(
                            "Glamour template `{}` has conflicting checked structure",
                            candidate.identity
                        )));
                    }
                    plan_origins.push(origin);
                }
                None => {
                    plans.insert(
                        candidate.identity,
                        (candidate.slots, candidate.root, vec![origin]),
                    );
                }
            }
        }
    }

    let mut wire_ids = BTreeMap::new();
    plans
        .into_iter()
        .map(|(identity, (slots, root, mut origins))| {
            origins.sort_by(|left, right| {
                left.id
                    .module
                    .cmp(&right.id.module)
                    .then_with(|| left.id.ordinal.cmp(&right.id.ordinal))
            });
            let wire_id = template_wire_id(&identity)?;
            if let Some(existing) = wire_ids.insert(wire_id, identity.clone()) {
                return Err(metadata_error(format!(
                    "Glamour templates `{existing}` and `{identity}` collide at wire identity {wire_id}"
                )));
            }
            Ok(GlamourTemplateMetadata {
                identity,
                wire_id,
                slots,
                root,
                origins,
            })
        })
        .collect()
}

/// Clone the authenticated checked module for static evaluation and inject the
/// compiler-owned origin token accepted by Glamour's sealed Interactive value.
/// The original checked module remains the authority for metadata and codegen.
pub fn checked_glamour_static_evaluation_module(
    checked: &witchy_types::pipeline::CheckedModule,
) -> Result<witchy_types::pipeline::CheckedEvaluationModule, CodegenError> {
    let catalog = checked
        .runtime_declaration_catalog()
        .map_err(|error| island_metadata_error(error.to_string()))?;
    let hidden = catalog
        .resolve(
            "glamour.interactive_with_origin",
            DeclarationKind::Function,
        )
        .ok_or_else(|| {
            island_metadata_error(
                "toolchain Glamour is missing the compiler-owned interactive origin constructor",
            )
        })?;
    if !is_glamour_declaration(hidden, "interactive_with_origin") {
        return Err(island_metadata_error(
            "`glamour.interactive_with_origin` is not owned by the toolchain Glamour package",
        ));
    }
    let fresh_hidden = catalog
        .resolve(
            "glamour.client_region_with_origin",
            DeclarationKind::Function,
        )
        .ok_or_else(|| {
            island_metadata_error(
                "toolchain Glamour is missing the compiler-owned client-region origin constructor",
            )
        })?;
    if !is_glamour_declaration(fresh_hidden, "client_region_with_origin") {
        return Err(island_metadata_error(
            "`glamour.client_region_with_origin` is not owned by the toolchain Glamour package",
        ));
    }

    let mut module = checked.module().clone();
    rewrite_interactive_calls(&mut module, &catalog)?;
    witchy_types::pipeline::check_compiler_evaluation_module(checked, module)
        .map_err(|error| island_metadata_error(error.to_string()))
}

/// Clone one checked application for its island artifact and replace reachable
/// source-level effect constructors with compiler-authenticated constructors
/// carrying the exact descriptor and completion identities selected above.
pub fn checked_glamour_island_execution_module(
    checked: &witchy_types::pipeline::CheckedModule,
    island: &GlamourIslandMetadata,
) -> Result<Module, CodegenError> {
    checked_glamour_execution_module(checked, island, None)
}

/// Clone the checked application into one capability-denied worker executable.
/// The selected descriptor authenticates the direct task declaration and its
/// exact request/result codecs; the generated public export accepts and returns
/// only the closed `IslandCapture` wire.
pub fn checked_glamour_worker_execution_module(
    checked: &witchy_types::pipeline::CheckedModule,
    island: &GlamourIslandMetadata,
    descriptor_id: u32,
) -> Result<Module, CodegenError> {
    checked_glamour_execution_module(checked, island, Some(descriptor_id))
}

fn checked_glamour_execution_module(
    checked: &witchy_types::pipeline::CheckedModule,
    island: &GlamourIslandMetadata,
    worker_descriptor: Option<u32>,
) -> Result<Module, CodegenError> {
    let mut by_owner = BTreeMap::<String, BTreeMap<u32, &GlamourWorkMetadata>>::new();
    for work in &island.work {
        if by_owner
            .entry(work.owner_name.clone())
            .or_default()
            .insert(work.ordinal, work)
            .is_some()
        {
            return Err(island_metadata_error(format!(
                "island `{}` repeats work ordinal {} in `{}`",
                island.key, work.ordinal, work.owner_name,
            )));
        }
    }
    let mut maps_by_owner = BTreeMap::<String, BTreeMap<u32, &GlamourWorkMapMetadata>>::new();
    for mapper in &island.work_maps {
        if maps_by_owner
            .entry(mapper.owner_name.clone())
            .or_default()
            .insert(mapper.ordinal, mapper)
            .is_some()
        {
            return Err(island_metadata_error(format!(
                "island `{}` repeats map ordinal {} in `{}`",
                island.key, mapper.ordinal, mapper.owner_name,
            )));
        }
    }
    let mut module = checked.module().clone();
    let generated_owner = module.linked_entry.clone().unwrap_or_default();
    let mut capture_codecs = GlamourCaptureCodecs::new(&module);
    let message_decoder = capture_codecs.codec_for(&island.message_type)?.1;
    for item in &mut module.items {
        let Item::Function(function) = item else { continue };
        let sites = by_owner.get(&function.name);
        let map_sites = maps_by_owner.get(&function.name);
        if sites.is_none() && map_sites.is_none() {
            continue;
        }
        let mut ordinal = 0_u32;
        let mut map_ordinal = 0_u32;
        let mut rewritten_maps = 0_usize;
        visit_block_mut(&mut function.body, &mut |expression| {
            if glamour_work_map_channel_from_expression(expression).is_some() {
                map_ordinal = map_ordinal.checked_add(1).ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour map declaration `{}` has too many map sites",
                        function.name,
                    ))
                })?;
                let Some(site) = map_sites.and_then(|sites| sites.get(&map_ordinal)) else {
                    return Ok(());
                };
                rewrite_glamour_work_map_call(
                    expression,
                    site,
                    &generated_owner,
                    &mut capture_codecs,
                )?;
                rewritten_maps += 1;
                return Ok(());
            }
            let Expr::Call { name, args } = expression else { return Ok(()) };
            if glamour_work_signature(name).is_none() { return Ok(()) }
            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour work declaration `{}` has too many effect sites",
                    function.name,
                ))
            })?;
            let site = sites.and_then(|sites| sites.get(&ordinal)).ok_or_else(|| {
                island_metadata_error(format!(
                    "island `{}` has no authenticated work site {} in `{}`",
                    island.key, ordinal, function.name,
                ))
            })?;
            if site.call_name != *name {
                return Err(island_metadata_error(format!(
                    "island `{}` work site {} in `{}` changed from `{}` to `{name}`",
                    island.key, ordinal, function.name, site.call_name,
                )));
            }
            rewrite_glamour_work_call(
                name,
                args,
                site,
                &mut capture_codecs,
            )?;
            Ok(())
        })?;
        if ordinal as usize != sites.map_or(0, |sites| sites.len()) {
            return Err(island_metadata_error(format!(
                "island `{}` authenticated {} work sites in `{}`, but rewrote {ordinal}",
                island.key,
                sites.map_or(0, |sites| sites.len()),
                function.name,
            )));
        }
        if rewritten_maps != map_sites.map_or(0, |sites| sites.len()) {
            return Err(island_metadata_error(format!(
                "island `{}` authenticated {} map sites in `{}`, but rewrote {map_ordinal}",
                island.key,
                map_sites.map_or(0, |sites| sites.len()),
                function.name,
            )));
        }
    }
    module.items.push(Item::Function(glamour_completion_capture_dispatch(
        island,
        &mut capture_codecs,
        &module,
    )?));
    module.items.push(Item::Function(glamour_completion_dispatch(
        island,
        &message_decoder,
        &module,
    )));
    if let Some(descriptor_id) = worker_descriptor {
        let site = island
            .work
            .iter()
            .find(|site| site.descriptor_id == descriptor_id && site.kind == "worker")
            .ok_or_else(|| island_metadata_error(format!(
                "Glamour worker descriptor {descriptor_id} is not an authenticated base worker task",
            )))?;
        let task = site.worker_task.as_ref().ok_or_else(|| {
            island_metadata_error("authenticated worker descriptor lost its task declaration")
        })?;
        let (maximum_request, maximum_result) = match &site.browser_policy {
            GlamourBrowserPolicyMetadata::Worker {
                max_request_bytes,
                max_result_bytes,
                ..
            } => (*max_request_bytes, *max_result_bytes),
            _ => return Err(island_metadata_error("authenticated worker descriptor lost its worker policy")),
        };
        let request_decoder = capture_codecs.codec_for(&task.request_type)?.1;
        let result_encoder = capture_codecs.codec_for(&task.result_type)?.0;
        let input_name = "__glamour_worker_input".to_string();
        let request_name = "__glamour_worker_request".to_string();
        let output_name = "__glamour_worker_output".to_string();
        module.items.push(Item::Function(Function {
            line: 0,
            public: true,
            comptime_only: false,
            attributes: vec!["browser".into()],
            name: generated_owner_name(&generated_owner, "export_glamour_worker_execute"),
            params: vec![generated_param(
                input_name.clone(),
                Type::Named("String".into(), Vec::new()),
            )],
            ret: Some(Type::Named("String".into(), Vec::new())),
            body: Block {
                stmts: vec![
                    Stmt::Let {
                        name: request_name.clone(),
                        ty: Some(task.request_type.clone()),
                        mutable: false,
                        value: Expr::Call {
                            name: request_decoder,
                            args: vec![Expr::Call {
                                name: "glamour.island_capture_from_wire".into(),
                                args: vec![Expr::Var(input_name), Expr::Int(maximum_request)],
                            }],
                        },
                    },
                    Stmt::Let {
                        name: output_name.clone(),
                        ty: Some(task.result_type.clone()),
                        mutable: false,
                        value: Expr::Call {
                            name: task.source_name.clone(),
                            args: vec![Expr::Var(request_name)],
                        },
                    },
                    Stmt::Expr(Expr::Call {
                        name: "glamour.island_capture_to_wire".into(),
                        args: vec![
                            Expr::Call {
                                name: result_encoder,
                                args: vec![Expr::Var(output_name)],
                            },
                            Expr::Int(maximum_result),
                        ],
                    }),
                ],
                lines: vec![0, 0, 0],
                region: None,
            },
            bounds: Vec::new(),
            is_gen: false,
            is_async: false,
        }));
    }
    capture_codecs.install_callback_support(island);
    module.items.extend(capture_codecs.finish()?);
    Ok(module)
}

fn glamour_completion_capture_dispatch(
    island: &GlamourIslandMetadata,
    capture_codecs: &mut GlamourCaptureCodecs,
    module: &Module,
) -> Result<Function, CodegenError> {
    let mut forbidden = BTreeSet::new();
    for site in &island.work {
        forbidden.extend(
            site.completion_captures
                .iter()
                .map(|capture| capture.name.clone()),
        );
        if let Expr::Lambda { params, .. } = &site.completion {
            forbidden.extend(params.iter().map(|param| param.name.clone()));
        }
    }
    for site in &island.mapped_work {
        forbidden.extend(
            site.mapper_captures
                .iter()
                .map(|capture| capture.name.clone()),
        );
        if let Expr::Lambda { params, .. } = &site.mapper {
            forbidden.extend(params.iter().map(|param| param.name.clone()));
        }
    }
    let completion_name = generated_binding("__glamour_completion", &forbidden);
    forbidden.insert(completion_name.clone());
    let environment_name = generated_binding("__glamour_environment", &forbidden);
    forbidden.insert(environment_name.clone());
    let status_name = generated_binding("__glamour_status", &forbidden);
    forbidden.insert(status_name.clone());
    let payload_name = generated_binding("__glamour_payload", &forbidden);
    forbidden.insert(payload_name.clone());
    let previous_name = generated_binding("__glamour_previous", &forbidden);

    let environment = || Expr::Var(environment_name.clone());
    let status = || Expr::Var(status_name.clone());
    let payload = || Expr::Var(payload_name.clone());
    let mut arms = Vec::with_capacity(island.work.len() + island.mapped_work.len() + 1);
    for site in &island.work {
        let mut statements = Vec::new();
        if matches!(site.kind.as_str(), "timer" | "interval") {
            statements.push(Stmt::Expr(Expr::Call {
                name: "glamour.island_unit_completion".into(),
                args: vec![payload(), status()],
            }));
            statements.push(Stmt::Expr(environment()));
        } else {
            let fields = Expr::Call {
                name: "glamour.island_capture_list_value".into(),
                args: vec![environment()],
            };
            statements.push(Stmt::Expr(Expr::If {
                cond: Box::new(Expr::Binary {
                    op: BinOp::NotEq,
                    lhs: Box::new(Expr::MethodCall {
                        receiver: Box::new(fields.clone()),
                        method: "length".into(),
                        args: Vec::new(),
                    }),
                    rhs: Box::new(Expr::Int(site.completion_captures.len() as i64)),
                }),
                then_block: Block {
                    stmts: vec![Stmt::Return(Some(Expr::Call {
                        name: "glamour.island_capture_abort".into(),
                        args: Vec::new(),
                    }))],
                    lines: vec![0],
                    region: None,
                },
                else_block: None,
            }));
            for (index, capture) in site.completion_captures.iter().enumerate() {
                let decoder = capture_codecs.codec_for(&capture.ty)?.1;
                statements.push(Stmt::Let {
                    name: capture.name.clone(),
                    ty: Some(capture.ty.clone()),
                    mutable: false,
                    value: Expr::Call {
                        name: decoder,
                        args: vec![Expr::MethodCall {
                            receiver: Box::new(fields.clone()),
                            method: "at".into(),
                            args: vec![Expr::Int(index as i64)],
                        }],
                    },
                });
            }
            let (result_type, result_value) = if site.kind == "worker" {
                let task = site.worker_task.as_ref().ok_or_else(|| {
                    island_metadata_error("authenticated worker completion lost its task metadata")
                })?;
                let maximum = match &site.browser_policy {
                    GlamourBrowserPolicyMetadata::Worker { max_result_bytes, .. } => *max_result_bytes,
                    _ => return Err(island_metadata_error("authenticated worker completion lost its worker policy")),
                };
                let decoder = capture_codecs.codec_for(&task.result_type)?.1;
                let success = Expr::Ctor {
                    name: "Ok".into(),
                    args: vec![Expr::Call {
                        name: decoder,
                        args: vec![Expr::Call {
                            name: "glamour.island_worker_completion_capture".into(),
                            args: vec![payload(), status(), Expr::Int(maximum)],
                        }],
                    }],
                };
                let failure = Expr::Ctor {
                    name: "Err".into(),
                    args: vec![Expr::Call {
                        name: "glamour.island_worker_completion_problem".into(),
                        args: vec![payload(), status()],
                    }],
                };
                (
                    Type::Named(
                        "Result".into(),
                        vec![task.result_type.clone(), Type::Named("String".into(), Vec::new())],
                    ),
                    Expr::If {
                        cond: Box::new(Expr::Binary {
                            op: BinOp::Eq,
                            lhs: Box::new(status()),
                            rhs: Box::new(Expr::Int(0)),
                        }),
                        then_block: Block {
                            stmts: vec![Stmt::Expr(success)],
                            lines: vec![0],
                            region: None,
                        },
                        else_block: Some(Block {
                            stmts: vec![Stmt::Expr(failure)],
                            lines: vec![0],
                            region: None,
                        }),
                    },
                )
            } else if site.kind == "host-port" {
                let port = site.host_port.as_ref().ok_or_else(|| {
                    island_metadata_error("authenticated host-port completion lost its type metadata")
                })?;
                let maximum = match &site.browser_policy {
                    GlamourBrowserPolicyMetadata::HostPort { max_result_bytes, .. } => *max_result_bytes,
                    _ => return Err(island_metadata_error("authenticated host-port completion lost its registry policy")),
                };
                let decoder = capture_codecs.codec_for(&port.result_type)?.1;
                let success = Expr::Ctor {
                    name: "Ok".into(),
                    args: vec![Expr::Call {
                        name: decoder,
                        args: vec![Expr::Call {
                            name: "glamour.island_host_port_completion_capture".into(),
                            args: vec![payload(), status(), Expr::Int(maximum)],
                        }],
                    }],
                };
                let failure = Expr::Ctor {
                    name: "Err".into(),
                    args: vec![Expr::Call {
                        name: "glamour.island_host_port_completion_problem".into(),
                        args: vec![payload(), status()],
                    }],
                };
                (
                    Type::Named(
                        "Result".into(),
                        vec![port.result_type.clone(), Type::Named("String".into(), Vec::new())],
                    ),
                    Expr::If {
                        cond: Box::new(Expr::Binary {
                            op: BinOp::Eq,
                            lhs: Box::new(status()),
                            rhs: Box::new(Expr::Int(0)),
                        }),
                        then_block: Block {
                            stmts: vec![Stmt::Expr(success)],
                            lines: vec![0],
                            region: None,
                        },
                        else_block: Some(Block {
                            stmts: vec![Stmt::Expr(failure)],
                            lines: vec![0],
                            region: None,
                        }),
                    },
                )
            } else {
                let (result_decoder, result_type) = match site.kind.as_str() {
                    "http" => ("glamour.island_http_completion", "glamour.HttpResult"),
                    "navigation" => ("glamour.island_navigation_completion", "glamour.NavigationResult"),
                    "port" | "secret" => ("glamour.island_port_completion", "glamour.PortResult"),
                    "storage-get" | "storage-set" | "storage-remove" => ("glamour.island_storage_completion", "glamour.StorageResult"),
                    kind => {
                        return Err(island_metadata_error(format!(
                            "Glamour completion {} uses unsupported production result kind `{kind}`",
                            site.completion_id,
                        )));
                    }
                };
                (
                    Type::Named(result_type.into(), Vec::new()),
                    Expr::Call {
                        name: result_decoder.into(),
                        args: vec![payload(), status()],
                    },
                )
            };
            let result_name = generated_binding("__glamour_result", &forbidden);
            statements.push(Stmt::Let {
                name: result_name.clone(),
                ty: Some(result_type),
                mutable: false,
                value: result_value,
            });
            let encoder = capture_codecs.codec_for(&site.completion_message_type)?.0;
            statements.push(Stmt::Expr(Expr::Call {
                name: encoder,
                args: vec![Expr::Apply {
                    func: Box::new(site.completion.clone()),
                    args: vec![Expr::Var(result_name)],
                }],
            }));
        }
        let lines = vec![0; statements.len()];
        arms.push(MatchArm {
            line: 0,
            pattern: Pattern::Int(i64::from(site.completion_id)),
            guard: None,
            body: Expr::Block(Block {
                stmts: statements,
                lines,
                region: None,
            }),
        });
    }
    let owner = module.linked_entry.as_deref().unwrap_or_default();
    let capture_dispatch_name = generated_owner_name(owner, "glamour_island_complete_capture");
    for site in &island.mapped_work {
        let fields = Expr::Call {
            name: "glamour.island_capture_node_fields".into(),
            args: vec![
                environment(),
                Expr::Int(i64::from(site.completion_id)),
                Expr::Int(2),
            ],
        };
        let captures = Expr::Call {
            name: "glamour.island_capture_list_value".into(),
            args: vec![Expr::MethodCall {
                receiver: Box::new(fields.clone()),
                method: "at".into(),
                args: vec![Expr::Int(1)],
            }],
        };
        let mut statements = vec![Stmt::Expr(Expr::If {
            cond: Box::new(Expr::Binary {
                op: BinOp::NotEq,
                lhs: Box::new(Expr::MethodCall {
                    receiver: Box::new(captures.clone()),
                    method: "length".into(),
                    args: Vec::new(),
                }),
                rhs: Box::new(Expr::Int(site.mapper_captures.len() as i64)),
            }),
            then_block: Block {
                stmts: vec![Stmt::Return(Some(Expr::Call {
                    name: "glamour.island_capture_abort".into(),
                    args: Vec::new(),
                }))],
                lines: vec![0],
                region: None,
            },
            else_block: None,
        })];
        for (index, capture) in site.mapper_captures.iter().enumerate() {
            let decoder = capture_codecs.codec_for(&capture.ty)?.1;
            statements.push(Stmt::Let {
                name: capture.name.clone(),
                ty: Some(capture.ty.clone()),
                mutable: false,
                value: Expr::Call {
                    name: decoder,
                    args: vec![Expr::MethodCall {
                        receiver: Box::new(captures.clone()),
                        method: "at".into(),
                        args: vec![Expr::Int(index as i64)],
                    }],
                },
            });
        }
        let input_decoder = capture_codecs.codec_for(&site.input_type)?.1;
        let output_encoder = capture_codecs.codec_for(&site.output_type)?.0;
        statements.push(Stmt::Let {
            name: previous_name.clone(),
            ty: Some(site.input_type.clone()),
            mutable: false,
            value: Expr::Call {
                name: input_decoder,
                args: vec![Expr::Call {
                    name: capture_dispatch_name.clone(),
                    args: vec![
                        Expr::Int(i64::from(site.previous_completion_id)),
                        Expr::MethodCall {
                            receiver: Box::new(fields),
                            method: "at".into(),
                            args: vec![Expr::Int(0)],
                        },
                        status(),
                        payload(),
                    ],
                }],
            },
        });
        statements.push(Stmt::Expr(Expr::Call {
            name: output_encoder,
            args: vec![Expr::Apply {
                func: Box::new(site.mapper.clone()),
                args: vec![Expr::Var(previous_name.clone())],
            }],
        }));
        let lines = vec![0; statements.len()];
        arms.push(MatchArm {
            line: 0,
            pattern: Pattern::Int(i64::from(site.completion_id)),
            guard: None,
            body: Expr::Block(Block {
                stmts: statements,
                lines,
                region: None,
            }),
        });
    }
    arms.push(MatchArm {
        line: 0,
        pattern: Pattern::Wildcard,
        guard: None,
        body: Expr::Call {
            name: "glamour.island_capture_abort".into(),
            args: Vec::new(),
        },
    });
    Ok(Function {
        line: 0,
        public: false,
        comptime_only: false,
        attributes: Vec::new(),
        name: capture_dispatch_name,
        params: vec![
            generated_param(
                completion_name.clone(),
                Type::Named("Int".into(), Vec::new()),
            ),
            generated_param(
                environment_name.clone(),
                Type::Named("glamour.IslandCapture".into(), Vec::new()),
            ),
            generated_param(status_name, Type::Named("Int".into(), Vec::new())),
            generated_param(payload_name, Type::Named("Bytes".into(), Vec::new())),
        ],
        ret: Some(Type::Named("glamour.IslandCapture".into(), Vec::new())),
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Match {
                scrutinee: Box::new(Expr::Var(completion_name)),
                arms,
            })],
            lines: vec![0],
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    })
}

fn glamour_completion_dispatch(
    island: &GlamourIslandMetadata,
    message_decoder: &str,
    module: &Module,
) -> Function {
    let owner = module.linked_entry.as_deref().unwrap_or_default();
    let name = generated_owner_name(owner, "glamour_island_complete");
    let capture_dispatch_name = generated_owner_name(owner, "glamour_island_complete_capture");
    Function {
        line: 0,
        public: false,
        comptime_only: false,
        attributes: Vec::new(),
        name,
        params: vec![
            generated_param(
                "__glamour_completion".into(),
                Type::Named("Int".into(), Vec::new()),
            ),
            generated_param(
                "__glamour_environment".into(),
                Type::Named("glamour.IslandCapture".into(), Vec::new()),
            ),
            generated_param("__glamour_status".into(), Type::Named("Int".into(), Vec::new())),
            generated_param("__glamour_payload".into(), Type::Named("Bytes".into(), Vec::new())),
        ],
        ret: Some(island.message_type.clone()),
        body: Block {
            stmts: vec![Stmt::Expr(Expr::Call {
                name: message_decoder.into(),
                args: vec![Expr::Call {
                    name: capture_dispatch_name,
                    args: vec![
                        Expr::Var("__glamour_completion".into()),
                        Expr::Var("__glamour_environment".into()),
                        Expr::Var("__glamour_status".into()),
                        Expr::Var("__glamour_payload".into()),
                    ],
                }],
            })],
            lines: vec![0],
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    }
}

fn generated_owner_name(owner: &str, local: &str) -> String {
    if owner.is_empty() {
        local.into()
    } else {
        format!("{owner}.{local}")
    }
}

/// Add the typed aggregate snapshot codecs selected by the authenticated
/// development metadata. These helpers are ordinary checked Witchy functions;
/// the WIR ABI only moves their bounded `String` result, never representation-
/// specific aggregate pointers, across builds.
pub(super) fn checked_glamour_development_codec_module(
    checked: &witchy_types::pipeline::CheckedModule,
    spec: &GlamourDevelopmentCodecSpec,
) -> Result<Module, CodegenError> {
    const MAXIMUM_BYTES: i64 = 1024 * 1024;
    let mut module = checked.module().clone();
    let mut codecs = GlamourCaptureCodecs::new(&module);
    let (state_encoder, state_decoder) = codecs.codec_for(&spec.state_type)?;
    let value = "__glamour_value".to_string();
    module.items.push(Item::Function(generated_development_codec_function(
        spec.encoder.clone(),
        value.clone(),
        spec.state_type.clone(),
        Type::Named("String".into(), Vec::new()),
        Expr::Call {
            name: "glamour.island_capture_to_wire".into(),
            args: vec![
                Expr::Call {
                    name: state_encoder,
                    args: vec![Expr::Var(value.clone())],
                },
                Expr::Int(MAXIMUM_BYTES),
            ],
        },
    )));
    module.items.push(Item::Function(generated_development_codec_function(
        spec.decoder.clone(),
        value.clone(),
        Type::Named("String".into(), Vec::new()),
        spec.state_type.clone(),
        Expr::Call {
            name: state_decoder,
            args: vec![Expr::Call {
                name: "glamour.island_capture_from_wire".into(),
                args: vec![Expr::Var(value.clone()), Expr::Int(MAXIMUM_BYTES)],
            }],
        },
    )));
    for migration in &spec.migrations {
        let old_decoder = codecs.codec_for(&migration.source_type)?.1;
        module.items.push(Item::Function(generated_development_codec_function(
            migration.decoder.clone(),
            value.clone(),
            Type::Named("String".into(), Vec::new()),
            spec.state_type.clone(),
            Expr::Call {
                name: migration.migration.clone(),
                args: vec![Expr::Call {
                    name: old_decoder,
                    args: vec![Expr::Call {
                        name: "glamour.island_capture_from_wire".into(),
                        args: vec![Expr::Var(value.clone()), Expr::Int(MAXIMUM_BYTES)],
                    }],
                }],
            },
        )));
    }
    module.items.extend(codecs.finish()?);
    module.item_lines.clear();
    Ok(module)
}

fn generated_development_codec_function(
    name: String,
    parameter: String,
    parameter_type: Type,
    return_type: Type,
    expression: Expr,
) -> Function {
    Function {
        line: 0,
        public: false,
        comptime_only: false,
        attributes: Vec::new(),
        name,
        params: vec![generated_param(parameter, parameter_type)],
        ret: Some(return_type),
        body: Block {
            stmts: vec![Stmt::Expr(expression)],
            lines: vec![0],
            region: None,
        },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    }
}

fn generated_binding(base: &str, forbidden: &BTreeSet<String>) -> String {
    let mut name = base.to_string();
    while forbidden.contains(&name) {
        name.push('_');
    }
    name
}

fn generated_param(name: String, ty: Type) -> Param {
    Param {
        name,
        ty: Some(ty),
        convention: Convention::default(),
        default: None,
    }
}

fn rewrite_glamour_work_call(
    name: &mut String,
    args: &mut Vec<Expr>,
    site: &GlamourWorkMetadata,
    capture_codecs: &mut GlamourCaptureCodecs,
) -> Result<(), CodegenError> {
    let original = std::mem::take(args);
    let descriptor = Expr::Int(i64::from(site.descriptor_id));
    let completion = Expr::Int(i64::from(site.completion_id));
    let environment = if matches!(site.kind.as_str(), "timer" | "interval") {
        let message = original
            .get(glamour_work_signature(name).expect("authenticated work signature").completion)
            .ok_or_else(|| island_metadata_error("authenticated work message is absent"))?;
        let encoder = capture_codecs.codec_for(&site.completion_message_type)?.0;
        Expr::Call {
            name: encoder,
            args: vec![message.clone()],
        }
    } else {
        let captures = site
            .completion_captures
            .iter()
            .map(|capture| {
                let encoder = capture_codecs.codec_for(&capture.ty)?.0;
                Ok(Expr::Call {
                    name: encoder,
                    args: vec![Expr::Var(capture.name.clone())],
                })
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        Expr::Ctor {
            name: "glamour.IslandCaptureList".into(),
            args: vec![Expr::List(captures)],
        }
    };
    let (rewritten_name, rewritten_args) = match (name.as_str(), original.as_slice()) {
        ("glamour.after", [timer, milliseconds, _message]) => (
            "glamour.island_cmd_after",
            vec![descriptor, completion, timer.clone(), milliseconds.clone(), environment],
        ),
        ("glamour.schedule", [id, timer, milliseconds, _message]) => (
            "glamour.island_cmd_schedule",
            vec![descriptor, completion, id.clone(), timer.clone(), milliseconds.clone(), environment],
        ),
        ("glamour.http_get", [id, fetch, url, _on_done]) => (
            "glamour.island_cmd_http",
            vec![descriptor, completion, Expr::Int(i64::from(site.owner_scope_id)), id.clone(), fetch.clone(), Expr::Str("GET".into()), url.clone(), Expr::Str(String::new()), environment],
        ),
        ("glamour.http_post", [id, fetch, url, body, _on_done]) => (
            "glamour.island_cmd_http",
            vec![descriptor, completion, Expr::Int(i64::from(site.owner_scope_id)), id.clone(), fetch.clone(), Expr::Str("POST".into()), url.clone(), body.clone(), environment],
        ),
        ("glamour.http_request", [id, fetch, method, url, body, _on_done]) => (
            "glamour.island_cmd_http",
            vec![descriptor, completion, Expr::Int(i64::from(site.owner_scope_id)), id.clone(), fetch.clone(), method.clone(), url.clone(), body.clone(), environment],
        ),
        ("glamour.navigate", [id, route, path, _on_done]) => (
            "glamour.island_cmd_navigate",
            vec![descriptor, completion, Expr::Int(i64::from(site.owner_scope_id)), id.clone(), route.clone(), path.clone(), environment],
        ),
        ("glamour.navigate_route", [id, authority, route, values, _on_done]) => (
            "glamour.island_cmd_navigate_route",
            vec![descriptor, completion, Expr::Int(i64::from(site.owner_scope_id)), id.clone(), authority.clone(), route.clone(), values.clone(), environment],
        ),
        ("glamour.port", [id, credential, argument, _on_done]) => (
            "glamour.island_cmd_port",
            vec![descriptor, completion, id.clone(), credential.clone(), argument.clone(), environment],
        ),
        ("glamour.host_port", [id, authority, request, _on_done]) => {
            let port = site.host_port.as_ref().ok_or_else(|| {
                island_metadata_error("authenticated host-port descriptor lost its type metadata")
            })?;
            let encoder = capture_codecs.codec_for(&port.request_type)?.0;
            (
                "glamour.island_cmd_host_port",
                vec![
                    descriptor,
                    completion,
                    id.clone(),
                    authority.clone(),
                    Expr::Call {
                        name: encoder,
                        args: vec![request.clone()],
                    },
                    environment,
                ],
            )
        }
        ("glamour.submit_secret", [id, secret, credential, _on_done]) => (
            "glamour.island_cmd_submit_secret",
            vec![descriptor, completion, id.clone(), secret.clone(), credential.clone(), environment],
        ),
        ("glamour.storage_get", [id, storage, key, _on_done]) => (
            "glamour.island_cmd_storage_get",
            vec![descriptor, completion, id.clone(), storage.clone(), key.clone(), environment],
        ),
        ("glamour.storage_set", [id, storage, key, value, _on_done]) => (
            "glamour.island_cmd_storage_set",
            vec![descriptor, completion, id.clone(), storage.clone(), key.clone(), value.clone(), environment],
        ),
        ("glamour.storage_remove", [id, storage, key, _on_done]) => (
            "glamour.island_cmd_storage_remove",
            vec![descriptor, completion, id.clone(), storage.clone(), key.clone(), environment],
        ),
        ("glamour.worker", [id, authority, _task, input, _on_done]) => {
            let task = site.worker_task.as_ref().ok_or_else(|| {
                island_metadata_error("authenticated worker descriptor lost its task metadata")
            })?;
            let encoder = capture_codecs.codec_for(&task.request_type)?.0;
            (
                "glamour.island_cmd_worker",
                vec![
                    descriptor,
                    completion,
                    id.clone(),
                    authority.clone(),
                    Expr::Call {
                        name: encoder,
                        args: vec![input.clone()],
                    },
                    environment,
                ],
            )
        }
        ("glamour.every", [id, timer, milliseconds, _message]) => (
            "glamour.island_sub_every",
            vec![descriptor, completion, id.clone(), timer.clone(), milliseconds.clone(), environment],
        ),
        _ => {
            return Err(island_metadata_error(format!(
                "authenticated work call `{name}` has an unexpected checked shape"
            )));
        }
    };
    *name = rewritten_name.into();
    *args = rewritten_args;
    Ok(())
}

fn rewrite_glamour_work_map_call(
    expression: &mut Expr,
    site: &GlamourWorkMapMetadata,
    generated_owner: &str,
    capture_codecs: &mut GlamourCaptureCodecs,
) -> Result<(), CodegenError> {
    let placeholder = Expr::Int(0);
    let (receiver, mapper) = match std::mem::replace(expression, placeholder) {
        Expr::MethodCall { receiver, method, args } if method == "map" => {
            let [mapper] = <[Expr; 1]>::try_from(args).map_err(|_| {
                island_metadata_error(format!(
                    "authenticated Glamour {} map site in `{}` changed shape",
                    site.channel, site.owner_name,
                ))
            })?;
            (*receiver, mapper)
        }
        Expr::Call { name, args } => {
            let direct_channel = match name.rsplit('.').next().unwrap_or(&name).split_once("__") {
                Some(("Cmd", "map")) => "cmd",
                Some(("Sub", "map")) => "sub",
                _ => "",
            };
            let [receiver, mapper] = <[Expr; 2]>::try_from(args).map_err(|_| {
                island_metadata_error(format!(
                    "authenticated Glamour {} map site in `{}` changed shape",
                    site.channel, site.owner_name,
                ))
            })?;
            if direct_channel != site.channel {
                return Err(island_metadata_error(format!(
                    "authenticated Glamour {} map site in `{}` changed channel",
                    site.channel, site.owner_name,
                )));
            }
            (receiver, mapper)
        }
        _ => {
            return Err(island_metadata_error(
                "authenticated Glamour map site changed shape",
            ));
        }
    };
    let captures = site
        .captures
        .iter()
        .map(|capture| {
            let encoder = capture_codecs.codec_for(&capture.ty)?.0;
            Ok(Expr::Call {
                name: encoder,
                args: vec![Expr::Var(capture.name.clone())],
            })
        })
        .collect::<Result<Vec<_>, CodegenError>>()?;
    *expression = Expr::Call {
        name: match site.channel.as_str() {
            "cmd" => generated_owner_name(
                generated_owner,
                &format!("glamour_island_capture_cmd_map_{}", site.mapper_id),
            ),
            "sub" => generated_owner_name(
                generated_owner,
                &format!("glamour_island_capture_sub_map_{}", site.mapper_id),
            ),
            channel => {
                return Err(island_metadata_error(format!(
                    "authenticated Glamour map site has unknown channel `{channel}`",
                )));
            }
        },
        args: vec![
            receiver,
            mapper,
            Expr::Ctor {
                name: "glamour.IslandCaptureList".into(),
                args: vec![Expr::List(captures)],
            },
        ],
    };
    Ok(())
}

/// Compiler-private, typed closure environments use Glamour's closed
/// `IslandCapture` tree. This generator emits one exact encoder/decoder pair
/// per concrete capture type. The representation never crosses the Wasm/host
/// boundary; it exists only so RFC-0108 state copy-out can persist callbacks
/// without retaining function values.
struct GlamourCaptureCodecs {
    owner: String,
    defs: BTreeMap<String, witchy_syntax::ast::TypeDef>,
    codecs: BTreeMap<String, (String, String)>,
    names: BTreeMap<String, String>,
    sources: Vec<String>,
}

impl GlamourCaptureCodecs {
    fn new(module: &Module) -> Self {
        let defs = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Type(definition) => {
                    Some((definition.name.clone(), definition.clone()))
                }
                _ => None,
            })
            .collect();
        Self {
            owner: module.linked_entry.clone().unwrap_or_default(),
            defs,
            codecs: BTreeMap::new(),
            names: BTreeMap::new(),
            sources: Vec::new(),
        }
    }

    fn install_callback_support(&mut self, island: &GlamourIslandMetadata) {
        for site in &island.work_maps {
            let input = witchy_syntax::format::type_str(&site.input_type);
            let output = witchy_syntax::format::type_str(&site.output_type);
            let local = format!(
                "glamour_island_capture_{}_map_{}",
                site.channel, site.mapper_id,
            );
            let pair = format!(
                "glamour_island_capture_{}_map_pair_{}",
                site.channel, site.mapper_id,
            );
            let transitions = island
                .mapped_work
                .iter()
                .filter(|mapped| mapped.mapper_id == site.mapper_id)
                .map(|mapped| {
                    format!(
                        "if descriptor == {} && completion == {}:\n        ({}, {}, glamour.IslandCaptureNode({}, [callback, captures]))",
                        mapped.previous_descriptor_id,
                        mapped.previous_completion_id,
                        mapped.descriptor_id,
                        mapped.completion_id,
                        mapped.completion_id,
                    )
                })
                .collect::<Vec<_>>();
            let transitions = if transitions.is_empty() {
                "glamour.island_capture_abort()".to_string()
            } else {
                format!(
                    "{}\n    else:\n        glamour.island_capture_abort()",
                    transitions.join("\n    else "),
                )
            };
            self.sources.push(format!(
                r#"fn {pair}(descriptor: Int, completion: Int, callback: glamour.IslandCapture, captures: glamour.IslandCapture) -> (Int, Int, glamour.IslandCapture):
    {transitions}
"#,
            ));
            let body = if site.channel == "cmd" {
                format!(
                    r#"fn {local}(command: glamour.Cmd({input}), _f: fn({input}) -> {output}, captures: glamour.IslandCapture) -> glamour.Cmd({output}):
    match command:
        glamour.NoCmd -> glamour.NoCmd
        glamour.CancelCmd(id) -> glamour.CancelCmd(id)
        glamour.Batch(commands) ->
            var mapped: List(glamour.Cmd({output})) = []
            for child in commands:
                mapped.push({local}(child, _f, captures))
            glamour.Batch(mapped)
        glamour.IslandAfter(descriptor, completion, timer, milliseconds, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandAfter(mapped_descriptor, mapped_completion, timer, milliseconds, mapped_callback)
        glamour.IslandAfterStable(descriptor, completion, id, timer, milliseconds, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandAfterStable(mapped_descriptor, mapped_completion, id, timer, milliseconds, mapped_callback)
        glamour.IslandHttpTask(descriptor, completion, owner, id, fetch, method, url, body, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandHttpTask(mapped_descriptor, mapped_completion, owner, id, fetch, method, url, body, mapped_callback)
        glamour.IslandNavigationTask(descriptor, completion, owner, id, route, path, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandNavigationTask(mapped_descriptor, mapped_completion, owner, id, route, path, mapped_callback)
        glamour.IslandPortTask(descriptor, completion, id, credential, argument, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandPortTask(mapped_descriptor, mapped_completion, id, credential, argument, mapped_callback)
        glamour.IslandHostPortTask(descriptor, completion, id, adapter, endpoint, maximum_request_bytes, maximum_result_bytes, request, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandHostPortTask(mapped_descriptor, mapped_completion, id, adapter, endpoint, maximum_request_bytes, maximum_result_bytes, request, mapped_callback)
        glamour.IslandSecretTask(descriptor, completion, id, secret, credential, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandSecretTask(mapped_descriptor, mapped_completion, id, secret, credential, mapped_callback)
        glamour.IslandStorageGetTask(descriptor, completion, id, storage, key, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandStorageGetTask(mapped_descriptor, mapped_completion, id, storage, key, mapped_callback)
        glamour.IslandStorageSetTask(descriptor, completion, id, storage, key, value, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandStorageSetTask(mapped_descriptor, mapped_completion, id, storage, key, value, mapped_callback)
        glamour.IslandStorageRemoveTask(descriptor, completion, id, storage, key, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandStorageRemoveTask(mapped_descriptor, mapped_completion, id, storage, key, mapped_callback)
        _ -> glamour.island_capture_abort()
"#,
                )
            } else {
                format!(
                    r#"fn {local}(subscription: glamour.Sub({input}), _f: fn({input}) -> {output}, captures: glamour.IslandCapture) -> glamour.Sub({output}):
    match subscription:
        glamour.NoSub -> glamour.NoSub
        glamour.BatchSub(subscriptions) ->
            var mapped: List(glamour.Sub({output})) = []
            for child in subscriptions:
                mapped.push({local}(child, _f, captures))
            glamour.BatchSub(mapped)
        glamour.IslandEvery(descriptor, completion, id, timer, milliseconds, callback) ->
            match {pair}(descriptor, completion, callback, captures):
                (mapped_descriptor, mapped_completion, mapped_callback) -> glamour.IslandEvery(mapped_descriptor, mapped_completion, id, timer, milliseconds, mapped_callback)
        _ -> glamour.island_capture_abort()
"#,
                )
            };
            self.sources.push(body);
        }
    }

    fn codec_for(&mut self, ty: &Type) -> Result<(String, String), CodegenError> {
        let ty = ty.unqualified().clone();
        let material = witchy_syntax::format::type_str(&ty);
        if let Some(codec) = self.codecs.get(&material) {
            return Ok(codec.clone());
        }
        let digest = Sha256::digest(material.as_bytes());
        let suffix = digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if let Some(existing) = self.names.insert(suffix.clone(), material.clone())
            && existing != material
        {
            return Err(island_metadata_error(format!(
                "Glamour callback capture types `{existing}` and `{material}` collide"
            )));
        }
        let local_encoder = format!("glamour_island_capture_encode_{suffix}");
        let local_decoder = format!("glamour_island_capture_decode_{suffix}");
        let encoder = if self.owner.is_empty() {
            local_encoder.clone()
        } else {
            format!("{}.{}", self.owner, local_encoder)
        };
        let decoder = if self.owner.is_empty() {
            local_decoder.clone()
        } else {
            format!("{}.{}", self.owner, local_decoder)
        };
        self.codecs
            .insert(material.clone(), (encoder.clone(), decoder.clone()));
        let source = self
            .codec_source(&ty, &local_encoder, &local_decoder)
            .inspect_err(|_| {
                self.codecs.remove(&material);
            })?;
        self.sources.push(source);
        Ok((encoder, decoder))
    }

    fn codec_source(
        &mut self,
        ty: &Type,
        encoder: &str,
        decoder: &str,
    ) -> Result<String, CodegenError> {
        let type_source = witchy_syntax::format::type_str(ty);
        match ty {
            Type::Named(name, args) => {
                let head = name.rsplit('.').next().unwrap_or(name);
                if head == "Duration" && args.is_empty() {
                    return Ok(format!(
                        "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    glamour.IslandCaptureInt(duration_to_int(value))\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        glamour.IslandCaptureInt(inner) -> int_to_duration(inner)\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n"
                    ));
                }
                let leaf = match (head, args.as_slice()) {
                    ("Int", []) => Some("Int"),
                    ("Float", []) => Some("Float"),
                    ("Bool", []) => Some("Bool"),
                    ("String", []) => Some("String"),
                    ("Bytes", []) => Some("Bytes"),
                    ("Nil", []) => Some("Nil"),
                    _ => None,
                };
                if let Some(variant) = leaf {
                    let pattern = if variant == "Nil" {
                        "glamour.IslandCaptureNil".to_string()
                    } else {
                        format!("glamour.IslandCapture{variant}(inner)")
                    };
                    let decoded = if variant == "Nil" { "Nil" } else { "inner" };
                    return Ok(format!(
                        "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    glamour.IslandCapture{variant}{}\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        {pattern} -> {decoded}\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n",
                        if variant == "Nil" { "".to_string() } else { "(value)".to_string() },
                    ));
                }
                if head == "List" && args.len() == 1 {
                    let (item_encoder, item_decoder) = self.codec_for(&args[0])?;
                    let item_type = witchy_syntax::format::type_str(&args[0]);
                    return Ok(format!(
                        "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    var output: List(glamour.IslandCapture) = []\n    for item in value:\n        output.push({item_encoder}(item))\n    glamour.IslandCaptureList(output)\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        glamour.IslandCaptureList(items) ->\n            var output: List({item_type}) = []\n            for item in items:\n                output.push({item_decoder}(item))\n            output\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n"
                    ));
                }
                if head == "Set" && args.len() == 1 {
                    let (item_encoder, item_decoder) = self.codec_for(&args[0])?;
                    let item_type = witchy_syntax::format::type_str(&args[0]);
                    return Ok(format!(
                        "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    glamour.IslandCaptureList(set.to_list(value).map(fn(item: {item_type}): {item_encoder}(item)))\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        glamour.IslandCaptureList(items) -> set.from_list(items.map(fn(item: glamour.IslandCapture): {item_decoder}(item)))\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n"
                    ));
                }
                if head == "Dict" && args.len() == 2 {
                    let (key_encoder, key_decoder) = self.codec_for(&args[0])?;
                    let (value_encoder, value_decoder) = self.codec_for(&args[1])?;
                    let key_type = witchy_syntax::format::type_str(&args[0]);
                    let value_type = witchy_syntax::format::type_str(&args[1]);
                    return Ok(format!(
                        "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    glamour.IslandCaptureList(dict.pairs(value).map(fn(pair: ({key_type}, {value_type})):\n        match pair:\n            (key, item) -> glamour.IslandCaptureNode(0, [{key_encoder}(key), {value_encoder}(item)])))\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        glamour.IslandCaptureList(items) -> dict.from_pairs(items.map(fn(item: glamour.IslandCapture) -> ({key_type}, {value_type}):\n            match item:\n                glamour.IslandCaptureNode(0, fields) ->\n                    if fields.length() != 2:\n                        return {decoder}_abort()\n                    ({key_decoder}(fields.at(0)), {value_decoder}(fields.at(1)))\n                _ -> {decoder}_abort()))\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n"
                    ));
                }
                self.nominal_codec_source(ty, name, args, encoder, decoder)
            }
            Type::Tuple(items) => {
                let codecs = items
                    .iter()
                    .map(|item| self.codec_for(item))
                    .collect::<Result<Vec<_>, _>>()?;
                let bindings = (0..items.len())
                    .map(|index| format!("field{index}"))
                    .collect::<Vec<_>>();
                let encoded = codecs
                    .iter()
                    .zip(&bindings)
                    .map(|((item_encoder, _), binding)| {
                        format!("{item_encoder}({binding})")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let decoded = codecs
                    .iter()
                    .enumerate()
                    .map(|(index, (_, item_decoder))| {
                        format!("{item_decoder}(fields.at({index}))")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Ok(format!(
                    "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    match value:\n        ({}) -> glamour.IslandCaptureNode(0, [{encoded}])\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        glamour.IslandCaptureNode(0, fields) ->\n            if fields.length() != {}:\n                return {decoder}_abort()\n            ({decoded})\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n",
                    bindings.join(", "),
                    items.len(),
                ))
            }
            Type::Fn(_, _, _) | Type::Dyn(_, _) => Err(island_metadata_error(format!(
                "Glamour callback environment cannot persist `{type_source}`"
            ))),
            Type::RecordCompose { .. } => Err(island_metadata_error(
                "Glamour callback environment contains an unnormalized record composition",
            )),
            Type::Qualified(_, _) => unreachable!("unqualified before codec generation"),
        }
    }

    fn nominal_codec_source(
        &mut self,
        ty: &Type,
        name: &str,
        args: &[Type],
        encoder: &str,
        decoder: &str,
    ) -> Result<String, CodegenError> {
        let definition = self.resolve_definition(name)?.clone();
        if definition.sealed {
            return Err(island_metadata_error(format!(
                "Glamour callback environment cannot persist sealed type `{}`",
                witchy_syntax::format::type_str(ty),
            )));
        }
        let fields = witchy_types::storage::instantiate_type_def_fields(&definition, args);
        let mut encode_arms = Vec::new();
        let mut decode_arms = Vec::new();
        for (tag, (variant, field_types)) in definition
            .variants
            .iter()
            .zip(fields.iter())
            .enumerate()
        {
            let codecs = field_types
                .iter()
                .map(|field| self.codec_for(field))
                .collect::<Result<Vec<_>, _>>()?;
            let bindings = (0..field_types.len())
                .map(|index| format!("field{index}"))
                .collect::<Vec<_>>();
            let pattern = if bindings.is_empty() {
                variant.name.clone()
            } else {
                format!("{}({})", variant.name, bindings.join(", "))
            };
            let encoded = codecs
                .iter()
                .zip(&bindings)
                .map(|((field_encoder, _), binding)| {
                    format!("{field_encoder}({binding})")
                })
                .collect::<Vec<_>>()
                .join(", ");
            encode_arms.push(format!(
                "        {pattern} -> glamour.IslandCaptureNode({tag}, [{encoded}])"
            ));
            let decoded = codecs
                .iter()
                .enumerate()
                .map(|(index, (_, field_decoder))| {
                    format!("{field_decoder}(fields.at({index}))")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let constructed = if codecs.is_empty() {
                variant.name.clone()
            } else {
                format!("{}({decoded})", variant.name)
            };
            let prefix = if tag == 0 { "if" } else { "else if" };
            decode_arms.push(format!(
                "            {prefix} tag == {tag}:\n                if fields.length() != {}:\n                    return {decoder}_abort()\n                {constructed}",
                field_types.len(),
            ));
        }
        let type_source = witchy_syntax::format::type_str(ty);
        Ok(format!(
            "pub fn {encoder}(value: {type_source}) -> glamour.IslandCapture:\n    match value:\n{}\n\npub fn {decoder}(value: glamour.IslandCapture) -> {type_source}:\n    match value:\n        glamour.IslandCaptureNode(tag, fields) ->\n{}\n            else:\n                {decoder}_abort()\n        _ -> {decoder}_abort()\n\nfn {decoder}_abort() -> {type_source}:\n    fail(\"glamour island: callback environment does not match `{type_source}`\")\n    {decoder}_abort()\n",
            encode_arms.join("\n"),
            decode_arms.join("\n"),
        ))
    }

    fn resolve_definition(
        &self,
        name: &str,
    ) -> Result<&witchy_syntax::ast::TypeDef, CodegenError> {
        if let Some(definition) = self.defs.get(name) {
            return Ok(definition);
        }
        let suffix = format!(".{name}");
        let mut matches = self
            .defs
            .iter()
            .filter(|(candidate, _)| candidate.ends_with(&suffix));
        let Some((_, definition)) = matches.next() else {
            return Err(island_metadata_error(format!(
                "Glamour callback environment type `{name}` has no concrete definition"
            )));
        };
        if matches.next().is_some() {
            return Err(island_metadata_error(format!(
                "Glamour callback environment type `{name}` is ambiguous"
            )));
        }
        Ok(definition)
    }

    fn finish(self) -> Result<Vec<Item>, CodegenError> {
        let mut items = Vec::new();
        for source in self.sources {
            let mut generated = witchy_syntax::parser::parse_module(&source).map_err(|error| {
                island_metadata_error(format!(
                    "cannot synthesize typed Glamour callback environment codec: {error}"
                ))
            })?;
            for item in &mut generated.items {
                let Item::Function(function) = item else { continue };
                visit_block_mut(&mut function.body, &mut |expression| {
                    let constructor = match expression {
                        Expr::Call { name, args }
                            if name
                                .rsplit('.')
                                .next()
                                .is_some_and(|leaf| leaf.starts_with(char::is_uppercase)) =>
                        {
                            Some((name.clone(), std::mem::take(args)))
                        }
                        Expr::MethodCall { receiver, method, args }
                            if matches!(receiver.as_ref(), Expr::Var(_))
                                && method.starts_with(char::is_uppercase) =>
                        {
                            let Expr::Var(module) = receiver.as_ref() else {
                                unreachable!("checked constructor receiver")
                            };
                            Some((
                                format!("{module}.{method}"),
                                std::mem::take(args),
                            ))
                        }
                        Expr::Field { base, field }
                            if matches!(base.as_ref(), Expr::Var(_))
                                && field.starts_with(char::is_uppercase) =>
                        {
                            let Expr::Var(module) = base.as_ref() else {
                                unreachable!("checked constructor receiver")
                            };
                            Some((format!("{module}.{field}"), Vec::new()))
                        }
                        _ => None,
                    };
                    if let Some((name, args)) = constructor {
                        *expression = Expr::Ctor { name, args };
                    }
                    if !self.owner.is_empty() {
                        let linked_call = match expression {
                            Expr::Call { name, .. }
                                if !name.contains('.')
                                    && name.starts_with("glamour_island_capture_") =>
                            {
                                Some(format!("{}.{}", self.owner, name))
                            }
                            Expr::MethodCall { receiver, method, .. }
                                if matches!(receiver.as_ref(), Expr::Var(module) if module == &self.owner)
                                    && method.starts_with("glamour_island_capture_") =>
                            {
                                Some(format!("{}.{}", self.owner, method))
                            }
                            _ => None,
                        };
                        if let Some(name) = linked_call {
                            let args = match expression {
                                Expr::Call { args, .. } | Expr::MethodCall { args, .. } => {
                                    std::mem::take(args)
                                }
                                _ => unreachable!("linked generated call shape"),
                            };
                            *expression = Expr::Call { name, args };
                        }
                        let standard_call = match expression {
                            Expr::MethodCall { receiver, method, .. }
                                if matches!(receiver.as_ref(), Expr::Var(module) if matches!(module.as_str(), "set" | "dict" | "glamour")) =>
                            {
                                let Expr::Var(module) = receiver.as_ref() else {
                                    unreachable!("checked standard module receiver")
                                };
                                Some(format!("{module}.{method}"))
                            }
                            _ => None,
                        };
                        if let Some(name) = standard_call {
                            let Expr::MethodCall { args, .. } = expression else {
                                unreachable!("checked standard module call")
                            };
                            *expression = Expr::Call {
                                name,
                                args: std::mem::take(args),
                            };
                        }
                    }
                    Ok(())
                })?;
            }
            if !self.owner.is_empty() {
                for item in &mut generated.items {
                    if let Item::Function(function) = item {
                        function.name = format!("{}.{}", self.owner, function.name);
                    }
                }
            }
            items.extend(generated.items);
        }
        Ok(items)
    }
}

/// Authenticate direct typed `glamour.island` declarations before their
/// generic values are erased into the closed static Site value.
pub fn checked_glamour_islands(
    checked: &witchy_types::pipeline::CheckedModule,
) -> Result<Vec<GlamourIslandMetadata>, CodegenError> {
    let catalog = checked
        .runtime_declaration_catalog()
        .map_err(|error| island_metadata_error(error.to_string()))?;
    let Some(island_plan) = catalog.resolve("glamour.IslandPlan", DeclarationKind::Type)
    else {
        return Ok(Vec::new());
    };
    if !is_glamour_declaration(island_plan, "IslandPlan") {
        return Err(island_metadata_error(
            "`glamour.IslandPlan` is not owned by the toolchain Glamour package",
        ));
    }
    let typed = witchy_types::typeck::annotate_checked_source(checked.module().clone())
        .map_err(|error| {
            island_metadata_error(format!(
                "cannot authenticate callback environment types: {error}"
            ))
        })?;
    let module = checked.module();
    let typed_module = typed.module();
    let type_table = typed.table();

    let mut islands = Vec::new();
    let mut keys = BTreeSet::new();
    let mut wire_ids = BTreeMap::new();
    let mut registry_ids = BTreeMap::new();
    visit_module_exprs(module, &mut |expression| {
        let Expr::Call { name, args } = expression else {
            return Ok(());
        };
        // Linked qualified names are assigned by the authenticated loader. A
        // source package cannot mint the `glamour` toolchain namespace, and the
        // return type above independently authenticates the closed plan shape.
        if name != "glamour.island" {
            return Ok(());
        }
        let [Expr::Str(key), program_expression, _initial, static_view_expression, activation_expression] =
            args.as_slice()
        else {
            return Err(island_metadata_error(
                "`glamour.island` must use its checked five-argument positional form with a literal key",
            ));
        };
        if !valid_island_key(key) || !keys.insert(key.clone()) {
            return Err(island_metadata_error(format!(
                "island key `{key}` is invalid or declared more than once"
            )));
        }
        let Expr::Call {
            name: program_name,
            args: program_arguments,
        } = program_expression
        else {
            return Err(island_metadata_error(format!(
                "island `{key}` program must be a direct zero-argument factory call"
            )));
        };
        if !program_arguments.is_empty() {
            return Err(island_metadata_error(format!(
                "island `{key}` program factory must take no arguments"
            )));
        }
        let program = catalog
            .resolve(program_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "island `{key}` program factory `{program_name}` has no authenticated declaration"
                ))
            })?
            .clone();
        let (auth_type, model_type, message_type) = island_program_types(
            module,
            &catalog,
            key,
            program_name,
        )?;
        let authorize_name = program_function_name(
            module,
            key,
            program_name,
            0,
            "authorize",
        )?;
        let initial_name = program_function_name(
            module,
            key,
            program_name,
            1,
            "initial",
        )?;
        let start_name = program_function_name(
            module,
            key,
            program_name,
            2,
            "start",
        )?;
        let update_name = program_function_name(
            module,
            key,
            program_name,
            3,
            "update",
        )?;
        let view_name = program_function_name(
            module,
            key,
            program_name,
            4,
            "view",
        )?;
        let subscriptions_name = program_function_name(
            module,
            key,
            program_name,
            5,
            "subscriptions",
        )?;
        let Expr::Var(static_view_name) = static_view_expression else {
            return Err(island_metadata_error(format!(
                "island `{key}` static projection must be a direct function declaration"
            )));
        };
        let static_view = catalog
            .resolve(static_view_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "island `{key}` static projection `{static_view_name}` has no authenticated declaration"
                ))
            })?
            .clone();
        let (activation, media) = island_activation(key, activation_expression)?;
        let auth_identity = catalog
            .type_identity(&auth_type)
            .map_err(|error| island_metadata_error(error.to_string()))?;
        let model_identity = catalog
            .type_identity(&model_type)
            .map_err(|error| island_metadata_error(error.to_string()))?;
        let message_identity = catalog
            .type_identity(&message_type)
            .map_err(|error| island_metadata_error(error.to_string()))?;
        let material = format!(
            "witchy.glamour.island.v1|program={}|view={}|auth={}|model={}|message={}|activation={}|media={}",
            declaration_material(&program),
            declaration_material(&static_view),
            runtime_type_material(&auth_identity),
            runtime_type_material(&model_identity),
            runtime_type_material(&message_identity),
            activation,
            media.as_deref().unwrap_or_default(),
        );
        let digest = Sha256::digest(material.as_bytes());
        let identity = format!("glamour-island1-{}", hex_digest(&digest));
        let wire_id = u32::from_be_bytes(digest[..4].try_into().expect("SHA-256 prefix"));
        if wire_id == 0 {
            return Err(island_metadata_error(format!(
                "island `{key}` produces reserved wire identity zero"
            )));
        }
        if let Some(existing) = wire_ids.insert(wire_id, identity.clone()) {
            if existing != identity {
                return Err(island_metadata_error(format!(
                    "island `{key}` collides with another executable at wire identity {wire_id}"
                )));
            }
        }
        let registry_digest = Sha256::digest(
            format!("witchy.glamour.island-registry.v1|{identity}").as_bytes(),
        );
        let registry_id =
            u32::from_be_bytes(registry_digest[..4].try_into().expect("SHA-256 prefix"));
        if registry_id == 0 {
            return Err(island_metadata_error(format!(
                "island `{key}` produces reserved registry identity zero"
            )));
        }
        if let Some(existing) = registry_ids.insert(registry_id, identity.clone()) {
            if existing != identity {
                return Err(island_metadata_error(format!(
                    "island `{key}` collides with another event registry at identity {registry_id}"
                )));
            }
        }
        let work = glamour_work_metadata(
            typed_module,
            type_table,
            &catalog,
            &program,
            &identity,
            &message_identity,
            (authorize_name, [start_name, update_name, subscriptions_name]),
        )?;
        let work_maps = glamour_work_map_metadata(
            typed_module,
            type_table,
            &catalog,
            &program,
            &identity,
            [start_name, update_name, subscriptions_name],
        )?;
        let mapped_work = glamour_mapped_work_metadata(&identity, &work, &work_maps)?;
        islands.push(GlamourIslandMetadata {
            key: key.clone(),
            source_identity: format!("low-level:{key}"),
            mode: "resume".into(),
            identity,
            wire_id,
            registry_id,
            program,
            program_name: program_name.clone(),
            authorize_name: authorize_name.into(),
            initial_name: initial_name.into(),
            start_name: start_name.into(),
            update_name: update_name.into(),
            view_name: view_name.into(),
            subscriptions_name: subscriptions_name.into(),
            static_view,
            auth_type,
            model_type,
            message_type,
            activation,
            media,
            prefetch: "none".into(),
            prefetch_media: None,
            diagnostic_name: None,
            work,
            work_maps,
            mapped_work,
        });
        Ok(())
    })?;
    let mut interactive_count = 0_usize;
    visit_function_interactive_calls(module, &mut |owner_name, ordinal, expression| {
        let Expr::Call { name, args } = expression else {
            return Ok(());
        };
        let (mode, constructor) = match name.as_str() {
            "glamour.interactive" => ("resume", "interactive"),
            "glamour.client_region" => ("fresh", "client_region"),
            _ => return Ok(()),
        };
        let [program_expression, boundary_expression] = args.as_slice() else {
            return Err(island_metadata_error(format!(
                "`glamour.{constructor}` must use its checked two-argument positional form"
            )));
        };
        let fallback_material = if mode == "fresh" {
            let Expr::Call {
                name: fallback_constructor,
                args: fallback_arguments,
            } = boundary_expression
            else {
                return Err(island_metadata_error(format!(
                    "client region {} fallback must be a direct `glamour.static_ui(...)` value",
                    interactive_count + 1
                )));
            };
            if fallback_constructor != "glamour.static_ui" || fallback_arguments.len() != 1 {
                return Err(island_metadata_error(format!(
                    "client region {} fallback must be a direct `glamour.static_ui(...)` value",
                    interactive_count + 1
                )));
            }
            witchy_syntax::format::expr_str(boundary_expression)
        } else {
            String::new()
        };
        interactive_count += 1;
        let owner = catalog
            .resolve(owner_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "interactive region {interactive_count} containing declaration `{owner_name}` has no authenticated identity"
                ))
            })?;
        let source_identity = interactive_source_identity(owner, ordinal, constructor);
        let key = format!("interactive-candidate-{source_identity}");
        let Expr::Call {
            name: program_name,
            args: program_arguments,
        } = program_expression
        else {
            return Err(island_metadata_error(format!(
                "interactive region {interactive_count} program must be a direct zero-argument factory call"
            )));
        };
        if !program_arguments.is_empty() {
            return Err(island_metadata_error(format!(
                "interactive region {interactive_count} program factory must take no arguments"
            )));
        }
        let program = catalog
            .resolve(program_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "interactive region {interactive_count} program factory `{program_name}` has no authenticated declaration"
                ))
            })?
            .clone();
        let (auth_type, model_type, message_type) = island_program_types(
            module,
            &catalog,
            &key,
            program_name,
        )?;
        let authorize_name = program_function_name(
            module,
            &key,
            program_name,
            0,
            "authorize",
        )?;
        let initial_name = program_function_name(
            module,
            &key,
            program_name,
            1,
            "initial",
        )?;
        let start_name = program_function_name(
            module,
            &key,
            program_name,
            2,
            "start",
        )?;
        let update_name = program_function_name(
            module,
            &key,
            program_name,
            3,
            "update",
        )?;
        let static_view_name = program_function_name(
            module,
            &key,
            program_name,
            4,
            "view",
        )?;
        let subscriptions_name = program_function_name(
            module,
            &key,
            program_name,
            5,
            "subscriptions",
        )?;
        let static_view = catalog
            .resolve(static_view_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "interactive region {interactive_count} view `{static_view_name}` has no authenticated declaration"
                ))
            })?
            .clone();
        let auth_identity = catalog
            .type_identity(&auth_type)
            .map_err(|error| island_metadata_error(error.to_string()))?;
        let model_identity = catalog
            .type_identity(&model_type)
            .map_err(|error| island_metadata_error(error.to_string()))?;
        let message_identity = catalog
            .type_identity(&message_type)
            .map_err(|error| island_metadata_error(error.to_string()))?;
        let material = format!(
            "witchy.glamour.{constructor}.v1|program={}|view={}|auth={}|model={}|message={}|fallback={}|activation=visible|prefetch=none",
            declaration_material(&program),
            declaration_material(&static_view),
            runtime_type_material(&auth_identity),
            runtime_type_material(&model_identity),
            runtime_type_material(&message_identity),
            fallback_material,
        );
        let digest = Sha256::digest(material.as_bytes());
        let identity = format!("glamour-island1-{}", hex_digest(&digest));
        let wire_id = u32::from_be_bytes(digest[..4].try_into().expect("SHA-256 prefix"));
        if wire_id == 0 {
            return Err(island_metadata_error(format!(
                "interactive region {interactive_count} produces reserved wire identity zero"
            )));
        }
        if let Some(existing) = wire_ids.insert(wire_id, identity.clone()) {
            if existing != identity {
                return Err(island_metadata_error(format!(
                    "interactive region {interactive_count} collides with another executable at wire identity {wire_id}"
                )));
            }
        }
        let registry_digest = Sha256::digest(
            format!("witchy.glamour.island-registry.v1|{identity}").as_bytes(),
        );
        let registry_id =
            u32::from_be_bytes(registry_digest[..4].try_into().expect("SHA-256 prefix"));
        if registry_id == 0 {
            return Err(island_metadata_error(format!(
                "interactive region {interactive_count} produces reserved registry identity zero"
            )));
        }
        if let Some(existing) = registry_ids.insert(registry_id, identity.clone()) {
            if existing != identity {
                return Err(island_metadata_error(format!(
                    "interactive region {interactive_count} collides with another event registry at identity {registry_id}"
                )));
            }
        }
        let work = glamour_work_metadata(
            typed_module,
            type_table,
            &catalog,
            &program,
            &identity,
            &message_identity,
            (authorize_name, [start_name, update_name, subscriptions_name]),
        )?;
        let work_maps = glamour_work_map_metadata(
            typed_module,
            type_table,
            &catalog,
            &program,
            &identity,
            [start_name, update_name, subscriptions_name],
        )?;
        let mapped_work = glamour_mapped_work_metadata(&identity, &work, &work_maps)?;
        islands.push(GlamourIslandMetadata {
            key,
            source_identity,
            mode: mode.into(),
            identity,
            wire_id,
            registry_id,
            program,
            program_name: program_name.clone(),
            authorize_name: authorize_name.into(),
            initial_name: initial_name.into(),
            start_name: start_name.into(),
            update_name: update_name.into(),
            view_name: static_view_name.into(),
            subscriptions_name: subscriptions_name.into(),
            static_view,
            auth_type,
            model_type,
            message_type,
            activation: "visible".into(),
            media: None,
            prefetch: "none".into(),
            prefetch_media: None,
            diagnostic_name: None,
            work,
            work_maps,
            mapped_work,
        });
        Ok(())
    })?;
    islands.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(islands)
}

fn program_function_name<'a>(
    module: &'a Module,
    key: &str,
    program_name: &str,
    index: usize,
    label: &str,
) -> Result<&'a str, CodegenError> {
    use witchy_syntax::ast::Stmt;

    let function = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == program_name => Some(function),
            _ => None,
        })
        .ok_or_else(|| {
            island_metadata_error(format!(
                "interactive `{key}` program factory `{program_name}` is missing from the checked module"
            ))
        })?;
    let Some(Stmt::Expr(Expr::Call { name, args })) = function.body.stmts.last() else {
        return Err(island_metadata_error(format!(
            "interactive `{key}` program factory must return a direct `glamour.program(...)` call"
        )));
    };
    if name != "glamour.program" || args.len() != 6 {
        return Err(island_metadata_error(format!(
            "interactive `{key}` program factory must return the checked six-argument `glamour.program(...)` form"
        )));
    }
    let Expr::Var(function) = &args[index] else {
        return Err(island_metadata_error(format!(
            "interactive `{key}` Program {label} must be a direct function declaration"
        )));
    };
    Ok(function)
}

#[derive(Clone, Copy)]
struct GlamourWorkSignature {
    channel: &'static str,
    kind: &'static str,
    handler: &'static str,
    completion: usize,
}

fn glamour_work_signature(name: &str) -> Option<GlamourWorkSignature> {
    let signature = match name {
        "glamour.after" => ("effect", "timer", "timer", 2),
        "glamour.schedule" => ("effect", "timer", "timer", 3),
        "glamour.http_get" => ("effect", "http", "request", 3),
        "glamour.http_post" => ("effect", "http", "request", 4),
        "glamour.http_request" => ("effect", "http", "request", 5),
        "glamour.navigate" => ("effect", "navigation", "navigation", 3),
        "glamour.navigate_route" => ("effect", "navigation", "navigation", 4),
        "glamour.port" => ("effect", "port", "port", 3),
        "glamour.host_port" => ("effect", "host-port", "port", 3),
        "glamour.submit_secret" => ("effect", "secret", "port", 3),
        "glamour.storage_get" => ("effect", "storage-get", "storage", 3),
        "glamour.storage_set" => ("effect", "storage-set", "storage", 4),
        "glamour.storage_remove" => ("effect", "storage-remove", "storage", 3),
        "glamour.worker" => ("effect", "worker", "worker", 4),
        "glamour.every" => ("subscription", "interval", "interval", 3),
        _ => return None,
    };
    Some(GlamourWorkSignature {
        channel: signature.0,
        kind: signature.1,
        handler: signature.2,
        completion: signature.3,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BrowserAuthorityValue {
    Unknown,
    Root,
    Policies(BTreeSet<GlamourBrowserPolicyMetadata>),
    Structure(Vec<BrowserAuthorityValue>),
}

impl BrowserAuthorityValue {
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (left, right) if left == right => left,
            (Self::Policies(mut left), Self::Policies(right)) => {
                left.extend(right);
                Self::Policies(left)
            }
            (Self::Structure(left), Self::Structure(right)) if left.len() == right.len() => {
                Self::Structure(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| left.merge(right))
                        .collect(),
                )
            }
            _ => Self::Unknown,
        }
    }
}

struct BrowserPolicyAnalyzer<'a> {
    functions: BTreeMap<String, &'a Function>,
    catalog: &'a RuntimeDeclarationCatalog,
    program: &'a DeclarationIdentity,
    active: BTreeSet<String>,
    work: BTreeMap<(String, u32), BTreeSet<GlamourBrowserPolicyMetadata>>,
}

impl<'a> BrowserPolicyAnalyzer<'a> {
    fn new(
        module: &'a Module,
        catalog: &'a RuntimeDeclarationCatalog,
        program: &'a DeclarationIdentity,
    ) -> Self {
        Self {
            functions: module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Function(function) => Some((function.name.clone(), function)),
                    _ => None,
                })
                .collect(),
            catalog,
            program,
            active: BTreeSet::new(),
            work: BTreeMap::new(),
        }
    }

    fn analyze(
        mut self,
        authorize: &str,
        roots: [&str; 3],
    ) -> Result<BTreeMap<(String, u32), GlamourBrowserPolicyMetadata>, CodegenError> {
        let auth = self.function(authorize, vec![BrowserAuthorityValue::Root])?;
        if matches!(auth, BrowserAuthorityValue::Unknown) {
            return Err(island_metadata_error(format!(
                "Glamour Program authorize function `{authorize}` has no closed compiler-authenticated capability shape"
            )));
        }
        for root in roots {
            let function = self.functions.get(root).ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour browser-policy root `{root}` is missing from the checked module"
                ))
            })?;
            let mut arguments = vec![BrowserAuthorityValue::Unknown; function.params.len()];
            if let Some(first) = arguments.first_mut() {
                *first = auth.clone();
            }
            self.function(root, arguments)?;
        }
        self.work
            .into_iter()
            .map(|(site, policies)| {
                let policies = policies.into_iter().collect::<Vec<_>>();
                let [policy] = policies.as_slice() else {
                    return Err(island_metadata_error(format!(
                        "Glamour work descriptor `{}#{}` must resolve to exactly one compiler-authenticated narrowed policy",
                        site.0, site.1,
                    )));
                };
                Ok((site, policy.clone()))
            })
            .collect()
    }

    fn function(
        &mut self,
        name: &str,
        arguments: Vec<BrowserAuthorityValue>,
    ) -> Result<BrowserAuthorityValue, CodegenError> {
        let Some(function) = self.functions.get(name).copied() else {
            return Ok(BrowserAuthorityValue::Unknown);
        };
        if !self.active.insert(name.to_string()) {
            return Ok(BrowserAuthorityValue::Unknown);
        }
        let mut environment = function
            .params
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                (
                    parameter.name.clone(),
                    arguments.get(index).cloned().unwrap_or(BrowserAuthorityValue::Unknown),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut ordinal = 0_u32;
        let result = self.block(name, &function.body, &mut environment, &mut ordinal);
        self.active.remove(name);
        result
    }

    fn block(
        &mut self,
        owner: &str,
        block: &Block,
        environment: &mut BTreeMap<String, BrowserAuthorityValue>,
        ordinal: &mut u32,
    ) -> Result<BrowserAuthorityValue, CodegenError> {
        let mut result = BrowserAuthorityValue::Unknown;
        for statement in &block.stmts {
            match statement {
                Stmt::Let { name, value, .. } | Stmt::Assign { name, value } => {
                    let value = self.expression(owner, value, environment, ordinal)?;
                    environment.insert(name.clone(), value);
                }
                Stmt::LetPattern { pattern, value } => {
                    let value = self.expression(owner, value, environment, ordinal)?;
                    bind_browser_authority(pattern, value, environment);
                }
                Stmt::Expr(value) | Stmt::Yield(value) => {
                    result = self.expression(owner, value, environment, ordinal)?;
                }
                Stmt::Return(Some(value)) => {
                    result = self.expression(owner, value, environment, ordinal)?;
                }
                Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(result)
    }

    fn expression(
        &mut self,
        owner: &str,
        expression: &Expr,
        environment: &BTreeMap<String, BrowserAuthorityValue>,
        ordinal: &mut u32,
    ) -> Result<BrowserAuthorityValue, CodegenError> {
        match expression {
            Expr::Var(name) => Ok(environment
                .get(name)
                .cloned()
                .unwrap_or(BrowserAuthorityValue::Unknown)),
            Expr::List(values) | Expr::Tuple(values) => Ok(BrowserAuthorityValue::Structure(
                values
                    .iter()
                    .map(|value| self.expression(owner, value, environment, ordinal))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
                Ok(BrowserAuthorityValue::Structure(
                    args.iter()
                        .map(|argument| self.expression(owner, argument, environment, ordinal))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            Expr::Call { name, args } => {
                if let Some(signature) = glamour_work_signature(name) {
                    *ordinal = ordinal.checked_add(1).ok_or_else(|| {
                        island_metadata_error(format!(
                            "Glamour work declaration `{owner}` has too many effect sites"
                        ))
                    })?;
                    let authority_index = glamour_work_authority_index(name)
                        .expect("every work signature has an authority argument");
                    let authority = args
                        .get(authority_index)
                        .map(|argument| self.expression(owner, argument, environment, ordinal))
                        .transpose()?
                        .unwrap_or(BrowserAuthorityValue::Unknown);
                    let BrowserAuthorityValue::Policies(policies) = authority else {
                        return Err(island_metadata_error(format!(
                            "Glamour work descriptor `{owner}#{ordinal}` does not resolve its narrowed capability from `authorize`"
                        )));
                    };
                    for argument in args.iter().skip(authority_index + 1) {
                        self.expression(owner, argument, environment, ordinal)?;
                    }
                    if !policies.iter().all(|policy| browser_policy_matches_work(policy, signature.kind)) {
                        return Err(island_metadata_error(format!(
                            "Glamour work descriptor `{owner}#{ordinal}` resolves the wrong narrowed capability kind"
                        )));
                    }
                    self.work
                        .entry((owner.to_string(), *ordinal))
                        .or_default()
                        .extend(policies);
                    return Ok(BrowserAuthorityValue::Unknown);
                }
                if let Some(policy) = self.policy(owner, name, args, environment, ordinal)? {
                    return Ok(BrowserAuthorityValue::Policies(BTreeSet::from([policy])));
                }
                let arguments = args
                    .iter()
                    .map(|argument| self.expression(owner, argument, environment, ordinal))
                    .collect::<Result<Vec<_>, _>>()?;
                if self.functions.contains_key(name)
                    && self
                        .catalog
                        .resolve(name, DeclarationKind::Function)
                        .is_some_and(|callee| callee.package() == self.program.package())
                {
                    self.function(name, arguments)
                } else {
                    Ok(BrowserAuthorityValue::Unknown)
                }
            }
            Expr::If { cond, then_block, else_block } => {
                self.expression(owner, cond, environment, ordinal)?;
                let mut then_environment = environment.clone();
                let then_value = self.block(owner, then_block, &mut then_environment, ordinal)?;
                let else_value = if let Some(else_block) = else_block {
                    let mut else_environment = environment.clone();
                    self.block(owner, else_block, &mut else_environment, ordinal)?
                } else {
                    BrowserAuthorityValue::Unknown
                };
                Ok(then_value.merge(else_value))
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee = self.expression(owner, scrutinee, environment, ordinal)?;
                let mut result: Option<BrowserAuthorityValue> = None;
                for arm in arms {
                    let mut arm_environment = environment.clone();
                    bind_browser_authority(&arm.pattern, scrutinee.clone(), &mut arm_environment);
                    if let Some(guard) = &arm.guard {
                        self.expression(owner, guard, &arm_environment, ordinal)?;
                    }
                    let value = self.expression(
                        owner,
                        &arm.body,
                        &arm_environment,
                        ordinal,
                    )?;
                    result = Some(match result {
                        Some(previous) => previous.merge(value),
                        None => value,
                    });
                }
                Ok(result.unwrap_or(BrowserAuthorityValue::Unknown))
            }
            Expr::Block(block) => {
                let mut inner = environment.clone();
                self.block(owner, block, &mut inner, ordinal)
            }
            Expr::Lambda { params, body, .. } => {
                let mut inner = environment.clone();
                inner.extend(params.iter().map(|parameter| {
                    (parameter.name.clone(), BrowserAuthorityValue::Unknown)
                }));
                self.block(owner, body, &mut inner, ordinal)?;
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::Unary { expr, .. }
            | Expr::Try(expr)
            | Expr::As { expr, .. }
            | Expr::ExistentialPack { expr, .. }
            | Expr::ExistentialUpcast { expr, .. }
            | Expr::Field { base: expr, .. } => self.expression(owner, expr, environment, ordinal),
            Expr::Binary { lhs, rhs, .. } => {
                self.expression(owner, lhs, environment, ordinal)?;
                self.expression(owner, rhs, environment, ordinal)?;
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::MethodCall { receiver, args, .. }
            | Expr::ExistentialCall { receiver, args, .. } => {
                self.expression(owner, receiver, environment, ordinal)?;
                for argument in args {
                    self.expression(owner, argument, environment, ordinal)?;
                }
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::Apply { func, args } => {
                self.expression(owner, func, environment, ordinal)?;
                for argument in args {
                    self.expression(owner, argument, environment, ordinal)?;
                }
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::RecordUpdate { base, fields, .. } => {
                self.expression(owner, base, environment, ordinal)?;
                for (_, value) in fields {
                    self.expression(owner, value, environment, ordinal)?;
                }
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::Record { fields, spread, .. } => {
                let mut values = fields
                    .iter()
                    .map(|(_, value)| self.expression(owner, value, environment, ordinal))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(spread) = spread {
                    values.push(self.expression(owner, spread, environment, ordinal)?);
                }
                Ok(BrowserAuthorityValue::Structure(values))
            }
            Expr::While { cond, body } => {
                self.expression(owner, cond, environment, ordinal)?;
                let mut inner = environment.clone();
                self.block(owner, body, &mut inner, ordinal)?;
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::For { var, iter, body } => {
                self.expression(owner, iter, environment, ordinal)?;
                let mut inner = environment.clone();
                inner.insert(var.clone(), BrowserAuthorityValue::Unknown);
                self.block(owner, body, &mut inner, ordinal)?;
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::Range { lo, hi, .. } | Expr::Index { base: lo, index: hi } => {
                self.expression(owner, lo, environment, ordinal)?;
                self.expression(owner, hi, environment, ordinal)?;
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::WhileLet { pattern, scrutinee, body } => {
                let value = self.expression(owner, scrutinee, environment, ordinal)?;
                let mut inner = environment.clone();
                bind_browser_authority(pattern, value, &mut inner);
                self.block(owner, body, &mut inner, ordinal)?;
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::LabeledCall { args, .. } => {
                for (_, argument) in args {
                    self.expression(owner, argument, environment, ordinal)?;
                }
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::LabeledMethodCall { receiver, args, .. } => {
                self.expression(owner, receiver, environment, ordinal)?;
                for (_, argument) in args {
                    self.expression(owner, argument, environment, ordinal)?;
                }
                Ok(BrowserAuthorityValue::Unknown)
            }
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::Duration(_)
            | Expr::Str(_)
            | Expr::Bool(_)
            | Expr::TaggedLit { .. } => Ok(BrowserAuthorityValue::Unknown),
        }
    }

    fn policy(
        &mut self,
        owner: &str,
        name: &str,
        args: &[Expr],
        environment: &BTreeMap<String, BrowserAuthorityValue>,
        ordinal: &mut u32,
    ) -> Result<Option<GlamourBrowserPolicyMetadata>, CodegenError> {
        let Some(declaration) = self.catalog.resolve(name, DeclarationKind::Function) else {
            return Ok(None);
        };
        if !matches!(declaration.name(), "fetch_scope" | "route_scope" | "timer_scope" | "storage_scope" | "worker_scope" | "credential_port" | "credential_get_exchange" | "credential_create_exchange" | "secret_field")
            || !is_glamour_declaration(declaration, declaration.name())
        {
            return Ok(None);
        }
        let root = args
            .first()
            .map(|argument| self.expression(owner, argument, environment, ordinal))
            .transpose()?
            .unwrap_or(BrowserAuthorityValue::Unknown);
        if root != BrowserAuthorityValue::Root {
            return Err(island_metadata_error(format!(
                "Glamour capability policy in `{owner}` must narrow the authenticated Program UiRoot"
            )));
        }
        let policy = match (declaration.name(), args) {
            ("fetch_scope", [_, Expr::Str(scope), Expr::Str(methods), Expr::Str(prefix)]) => {
                GlamourBrowserPolicyMetadata::Fetch {
                    scope: checked_policy_label(scope, "Fetch scope")?,
                    methods: checked_policy_methods(methods)?,
                    prefix: checked_policy_path(prefix, "Fetch prefix")?,
                }
            }
            ("route_scope", [_, Expr::Str(base), Expr::Str(rights)]) => {
                if !matches!(rights.as_str(), "push" | "replace") {
                    return Err(island_metadata_error(format!(
                        "Glamour route policy in `{owner}` must use literal `push` or `replace` rights"
                    )));
                }
                GlamourBrowserPolicyMetadata::Navigation {
                    base: checked_policy_path(base, "route base")?,
                    rights: rights.clone(),
                }
            }
            ("timer_scope", [_, Expr::Int(minimum)]) => {
                if !(0..=i64::from(i32::MAX)).contains(minimum) {
                    return Err(island_metadata_error(format!(
                        "Glamour timer policy in `{owner}` has an invalid minimum"
                    )));
                }
                GlamourBrowserPolicyMetadata::Timer { minimum: *minimum }
            }
            ("storage_scope", [_, Expr::Str(provider), Expr::Str(namespace), Expr::Str(key_prefix), Expr::Int(maximum)]) => {
                if !matches!(provider.as_str(), "session" | "local") {
                    return Err(island_metadata_error(format!(
                        "Glamour storage policy in `{owner}` must use literal `session` or `local` provider"
                    )));
                }
                if key_prefix.as_bytes().contains(&0) || key_prefix.len() > 256 {
                    return Err(island_metadata_error(format!(
                        "Glamour storage policy in `{owner}` has an invalid key prefix"
                    )));
                }
                if !(0..=65_536).contains(maximum) {
                    return Err(island_metadata_error(format!(
                        "Glamour storage policy in `{owner}` has an invalid maximum value size"
                    )));
                }
                GlamourBrowserPolicyMetadata::Storage {
                    provider: provider.clone(),
                    namespace: checked_policy_label(namespace, "storage namespace")?,
                    key_prefix: key_prefix.clone(),
                    max_value_bytes: *maximum,
                }
            }
            ("worker_scope", [_, Expr::Str(name), Expr::Int(max_request_bytes), Expr::Int(max_result_bytes), Expr::Int(max_concurrency), Expr::Int(timeout_ms)]) => {
                if !(1..=65_536).contains(max_request_bytes)
                    || !(1..=65_536).contains(max_result_bytes)
                    || !(1..=16).contains(max_concurrency)
                    || !(1..=300_000).contains(timeout_ms)
                {
                    return Err(island_metadata_error(format!(
                        "Glamour worker policy in `{owner}` must use positive limits no greater than 64 KiB request/result, 16 concurrent tasks, and 300 seconds"
                    )));
                }
                GlamourBrowserPolicyMetadata::Worker {
                    name: checked_policy_label(name, "worker name")?,
                    max_request_bytes: *max_request_bytes,
                    max_result_bytes: *max_result_bytes,
                    max_concurrency: *max_concurrency,
                    timeout_ms: *timeout_ms,
                }
            }
            ("credential_port", [_, Expr::Str(port)]) => GlamourBrowserPolicyMetadata::Port {
                name: checked_policy_label(port, "credential port")?,
            },
            (adapter @ ("credential_get_exchange" | "credential_create_exchange"), [_, Expr::Str(endpoint)]) => {
                GlamourBrowserPolicyMetadata::HostPort {
                    adapter: if adapter == "credential_get_exchange" {
                        "credential.get-exchange.v1".into()
                    } else {
                        "credential.create-exchange.v1".into()
                    },
                    endpoint: checked_policy_path(endpoint, "credential exchange endpoint")?,
                    max_request_bytes: 61_440,
                    max_result_bytes: 512,
                }
            }
            ("secret_field", [_, Expr::Str(form), Expr::Str(field)]) => {
                GlamourBrowserPolicyMetadata::SecretField {
                    form: checked_policy_label(form, "secret form")?,
                    field: checked_policy_label(field, "secret field")?,
                }
            }
            _ => {
                return Err(island_metadata_error(format!(
                    "Glamour capability policy in `{owner}` must be bounded by compiler-visible literals"
                )));
            }
        };
        Ok(Some(policy))
    }
}

fn bind_browser_authority(
    pattern: &Pattern,
    value: BrowserAuthorityValue,
    environment: &mut BTreeMap<String, BrowserAuthorityValue>,
) {
    match pattern {
        Pattern::Var(name) if name != "_" => {
            environment.insert(name.clone(), value);
        }
        Pattern::Ctor { args, .. } | Pattern::AnonCtor { args, .. }
            if matches!(value, BrowserAuthorityValue::Structure(_)) =>
        {
            let BrowserAuthorityValue::Structure(values) = value else { unreachable!() };
            for (pattern, value) in args.iter().zip(values) {
                bind_browser_authority(pattern, value, environment);
            }
        }
        Pattern::Tuple(patterns) if matches!(value, BrowserAuthorityValue::Structure(_)) => {
            let BrowserAuthorityValue::Structure(values) = value else { unreachable!() };
            for (pattern, value) in patterns.iter().zip(values) {
                bind_browser_authority(pattern, value, environment);
            }
        }
        _ => {
            let mut names = Vec::new();
            witchy_syntax::ast::pattern_binds(pattern, &mut names);
            environment.extend(names.into_iter().map(|name| {
                (name, BrowserAuthorityValue::Unknown)
            }));
        }
    }
}

fn glamour_work_authority_index(name: &str) -> Option<usize> {
    Some(match name {
        "glamour.after" => 0,
        "glamour.schedule" => 1,
        "glamour.http_get" | "glamour.http_post" | "glamour.http_request" => 1,
        "glamour.navigate" | "glamour.navigate_route" => 1,
        "glamour.port" => 1,
        "glamour.host_port" => 1,
        "glamour.submit_secret" => 2,
        "glamour.storage_get" | "glamour.storage_set" | "glamour.storage_remove" => 1,
        "glamour.worker" => 1,
        "glamour.every" => 1,
        _ => return None,
    })
}

fn browser_policy_matches_work(policy: &GlamourBrowserPolicyMetadata, kind: &str) -> bool {
    matches!(
        (policy, kind),
        (GlamourBrowserPolicyMetadata::Timer { .. }, "timer" | "interval")
            | (GlamourBrowserPolicyMetadata::Fetch { .. }, "http")
            | (GlamourBrowserPolicyMetadata::Navigation { .. }, "navigation")
            | (GlamourBrowserPolicyMetadata::Port { .. }, "port" | "secret")
            | (GlamourBrowserPolicyMetadata::HostPort { .. }, "host-port")
            | (GlamourBrowserPolicyMetadata::Storage { .. }, "storage-get" | "storage-set" | "storage-remove")
            | (GlamourBrowserPolicyMetadata::Worker { .. }, "worker")
    )
}

fn checked_policy_label(value: &str, label: &str) -> Result<String, CodegenError> {
    let mut bytes = value.bytes();
    if value.len() > 128
        || !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(island_metadata_error(format!(
            "{label} `{value}` must be a bounded static identifier"
        )));
    }
    Ok(value.to_string())
}

fn checked_policy_methods(value: &str) -> Result<String, CodegenError> {
    let mut methods = BTreeSet::new();
    for method in value.split(',').map(str::trim) {
        if !matches!(method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD") {
            return Err(island_metadata_error(format!(
                "Fetch policy method `{method}` is not in the closed browser method set"
            )));
        }
        methods.insert(method);
    }
    if methods.is_empty() {
        return Err(island_metadata_error(
            "Fetch policy must grant at least one static method",
        ));
    }
    Ok(methods.into_iter().collect::<Vec<_>>().join(","))
}

fn checked_policy_path(value: &str, label: &str) -> Result<String, CodegenError> {
    if value.len() > 2048
        || !value.starts_with('/')
        || value.starts_with("//")
        || value.contains(['\\', '?', '#', '\0'])
    {
        return Err(island_metadata_error(format!(
            "{label} `{value}` must be a bounded same-origin absolute path"
        )));
    }
    Ok(value.to_string())
}

fn glamour_work_metadata(
    module: &Module,
    type_table: &witchy_types::typeck::TypeTable,
    catalog: &RuntimeDeclarationCatalog,
    program: &DeclarationIdentity,
    island_identity: &str,
    message_identity: &RuntimeTypeIdentity,
    program_functions: (&str, [&str; 3]),
) -> Result<Vec<GlamourWorkMetadata>, CodegenError> {
    let (authorize, roots) = program_functions;
    let browser_policies = BrowserPolicyAnalyzer::new(module, catalog, program)
        .analyze(authorize, roots)?;
    let functions = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some((function.name.clone(), function)),
            _ => None,
        })
        .collect::<BTreeMap<String, &Function>>();
    let global_values = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function.name.clone()),
            Item::Const { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut pending = roots.into_iter().map(str::to_string).collect::<BTreeSet<_>>();
    let mut visited = BTreeSet::new();
    let mut work = Vec::new();
    let mut descriptor_ids = BTreeMap::new();
    let mut result_schema_ids = BTreeMap::new();
    let mut completion_ids = BTreeMap::new();
    let mut owner_scope_ids = BTreeMap::new();

    while let Some(owner_name) = pending.iter().next().cloned() {
        pending.remove(&owner_name);
        if !visited.insert(owner_name.clone()) {
            continue;
        }
        let function = functions.get(&owner_name).ok_or_else(|| {
            island_metadata_error(format!(
                "Glamour work root `{owner_name}` is missing from the checked module"
            ))
        })?;
        let owner = catalog
            .resolve(&owner_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour work root `{owner_name}` has no authenticated declaration"
                ))
            })?
            .clone();
        let mut ordinal = 0_u32;
        let mut discovered = BTreeSet::new();
        visit_block(&function.body, &mut |expression| {
            let Expr::Call { name, args } = expression else {
                return Ok(());
            };
            if functions.contains_key(name)
                && catalog
                    .resolve(name, DeclarationKind::Function)
                    .is_some_and(|callee| callee.package() == program.package())
            {
                discovered.insert(name.clone());
            }
            let Some(signature) = glamour_work_signature(name) else {
                return Ok(());
            };
            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour work declaration `{owner_name}` has too many effect sites"
                ))
            })?;
            let completion = args.get(signature.completion).ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour work call `{name}` in `{owner_name}` is missing its typed completion"
                ))
            })?;
            let completion_type = type_table
                .type_of(completion)
                .and_then(witchy_types::typeck::ty_to_ast)
                .ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour work call `{name}` in `{owner_name}` has no concrete typed completion",
                    ))
                })?;
            let (completion_message_type, completion_input_type) = if matches!(signature.kind, "timer" | "interval") {
                (completion_type, None)
            } else {
                let Type::Fn(inputs, output, _) = completion_type.unqualified() else {
                    return Err(island_metadata_error(format!(
                        "Glamour work call `{name}` in `{owner_name}` has a non-function completion",
                    )));
                };
                if inputs.len() != 1 {
                    return Err(island_metadata_error(format!(
                        "Glamour work call `{name}` in `{owner_name}` completion must accept one result",
                    )));
                }
                (output.as_ref().clone(), Some(inputs[0].clone()))
            };
            let worker_task = if signature.kind == "worker" {
                let task = args.get(2).ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour worker call in `{owner_name}` is missing its task declaration",
                    ))
                })?;
                let Expr::Var(task_name) = task else {
                    return Err(island_metadata_error(format!(
                        "Glamour worker call in `{owner_name}` must use a direct capability-free function declaration",
                    )));
                };
                if !functions.contains_key(task_name) {
                    return Err(island_metadata_error(format!(
                        "Glamour worker task `{task_name}` in `{owner_name}` must be a checked linked Witchy function",
                    )));
                }
                let task_declaration = catalog
                    .resolve(task_name, DeclarationKind::Function)
                    .ok_or_else(|| {
                        island_metadata_error(format!(
                            "Glamour worker task `{task_name}` in `{owner_name}` has no authenticated declaration",
                        ))
                    })?
                    .clone();
                let task_type = type_table
                    .type_of(task)
                    .and_then(witchy_types::typeck::ty_to_ast)
                    .ok_or_else(|| {
                        island_metadata_error(format!(
                            "Glamour worker task `{task_name}` in `{owner_name}` has no concrete function type",
                        ))
                    })?;
                let Type::Fn(inputs, output, _) = task_type.unqualified() else {
                    return Err(island_metadata_error(format!(
                        "Glamour worker task `{task_name}` in `{owner_name}` is not a function",
                    )));
                };
                let [request_type] = inputs.as_slice() else {
                    return Err(island_metadata_error(format!(
                        "Glamour worker task `{task_name}` in `{owner_name}` must accept exactly one request",
                    )));
                };
                let request_type = request_type.clone();
                let result_type = output.as_ref().clone();
                catalog
                    .capability_free_type_identity(&request_type, module)
                    .map_err(|error| island_metadata_error(format!(
                        "Glamour worker task `{task_name}` cannot receive its request: {error}",
                    )))?;
                catalog
                    .capability_free_type_identity(&result_type, module)
                    .map_err(|error| island_metadata_error(format!(
                        "Glamour worker task `{task_name}` cannot return its result: {error}",
                    )))?;
                let Some(completion_input_type) = completion_input_type.as_ref() else {
                    unreachable!("worker completions are functions")
                };
                let completion_matches = matches!(
                    completion_input_type.unqualified(),
                    Type::Named(name, arguments)
                        if name == "Result"
                            && arguments.as_slice() == [
                                result_type.clone(),
                                Type::Named("String".into(), Vec::new()),
                            ]
                );
                if !completion_matches {
                    return Err(island_metadata_error(format!(
                        "Glamour worker completion in `{owner_name}` must accept `Result` of task `{task_name}`'s exact result type and `String`",
                    )));
                }
                Some(GlamourWorkerTaskMetadata {
                    source_name: task_name.clone(),
                    declaration: task_declaration,
                    request_type,
                    result_type,
                })
            } else {
                None
            };
            let host_port = if signature.kind == "host-port" {
                let request = args.get(2).ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour host-port call in `{owner_name}` is missing its typed request",
                    ))
                })?;
                let request_type = type_table
                    .type_of(request)
                    .and_then(witchy_types::typeck::ty_to_ast)
                    .ok_or_else(|| {
                        island_metadata_error(format!(
                            "Glamour host-port request in `{owner_name}` has no concrete type",
                        ))
                    })?;
                let authority = args.get(1).ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour host-port call in `{owner_name}` is missing its authority",
                    ))
                })?;
                let authority_type = type_table
                    .type_of(authority)
                    .and_then(witchy_types::typeck::ty_to_ast)
                    .ok_or_else(|| {
                        island_metadata_error(format!(
                            "Glamour host-port authority in `{owner_name}` has no concrete type",
                        ))
                    })?;
                let Type::Named(name, arguments) = authority_type.unqualified() else {
                    return Err(island_metadata_error(format!(
                        "Glamour host-port authority in `{owner_name}` is not a HostPort",
                    )));
                };
                let [authority_request, result_type] = arguments.as_slice() else {
                    return Err(island_metadata_error(format!(
                        "Glamour host-port authority in `{owner_name}` has no concrete request/result pair",
                    )));
                };
                if name != "glamour.HostPort" && name != "HostPort" {
                    return Err(island_metadata_error(format!(
                        "Glamour host-port authority in `{owner_name}` is not a toolchain HostPort",
                    )));
                }
                if authority_request != &request_type {
                    return Err(island_metadata_error(format!(
                        "Glamour host-port request in `{owner_name}` differs from its authority type",
                    )));
                }
                let Some(completion_input_type) = completion_input_type.as_ref() else {
                    unreachable!("host-port completions are functions")
                };
                let completion_matches = matches!(
                    completion_input_type.unqualified(),
                    Type::Named(name, arguments)
                        if name == "Result"
                            && arguments.as_slice() == [
                                result_type.clone(),
                                Type::Named("String".into(), Vec::new()),
                            ]
                );
                if !completion_matches {
                    return Err(island_metadata_error(format!(
                        "Glamour host-port completion in `{owner_name}` must accept `Result` of the authority's exact result type and `String`",
                    )));
                }
                catalog
                    .capability_free_type_identity(&request_type, module)
                    .map_err(|error| island_metadata_error(format!(
                        "Glamour host-port request in `{owner_name}` is not serializable: {error}",
                    )))?;
                catalog
                    .capability_free_type_identity(result_type, module)
                    .map_err(|error| island_metadata_error(format!(
                        "Glamour host-port result in `{owner_name}` is not serializable: {error}",
                    )))?;
                Some(GlamourHostPortMetadata {
                    request_type,
                    result_type: result_type.clone(),
                })
            } else {
                None
            };
            catalog
                .capability_free_type_identity(&completion_message_type, module)
                .map_err(|error| {
                    island_metadata_error(format!(
                        "Glamour completion in `{owner_name}` cannot persist its typed message: {error}",
                    ))
                })?;
            let completion_source = witchy_syntax::format::expr_str(completion);
            let completion_captures = checked_completion_captures(
                completion,
                type_table,
                module,
                catalog,
                &global_values,
                &owner_name,
            )?;
            let capture_schema = completion_captures
                .iter()
                .map(|capture| {
                    let identity = catalog
                        .capability_free_type_identity(&capture.ty, module)
                        .map_err(|error| island_metadata_error(error.to_string()))?;
                    Ok(format!(
                        "{}{}",
                        framed(&capture.name),
                        framed(&runtime_type_material(&identity)),
                    ))
                })
                .collect::<Result<String, CodegenError>>()?;
            let worker_material = worker_task
                .as_ref()
                .map(|task| {
                    let request = catalog
                        .capability_free_type_identity(&task.request_type, module)
                        .map_err(|error| island_metadata_error(error.to_string()))?;
                    let result = catalog
                        .capability_free_type_identity(&task.result_type, module)
                        .map_err(|error| island_metadata_error(error.to_string()))?;
                    Ok(format!(
                        "declaration={}|request={}|result={}",
                        declaration_material(&task.declaration),
                        runtime_type_material(&request),
                        runtime_type_material(&result),
                    ))
                })
                .transpose()?
                .unwrap_or_default();
            let host_port_material = host_port
                .as_ref()
                .map(|port| {
                    let request = catalog
                        .capability_free_type_identity(&port.request_type, module)
                        .map_err(|error| island_metadata_error(error.to_string()))?;
                    let result = catalog
                        .capability_free_type_identity(&port.result_type, module)
                        .map_err(|error| island_metadata_error(error.to_string()))?;
                    Ok(format!(
                        "request={}|result={}",
                        runtime_type_material(&request),
                        runtime_type_material(&result),
                    ))
                })
                .transpose()?
                .unwrap_or_default();
            let site = format!(
                "island={island_identity}|owner={}|ordinal={ordinal}|channel={}|kind={}|handler={}|completion={completion_source}|captures={}|worker={}|host-port={}",
                declaration_material(&owner),
                signature.channel,
                signature.kind,
                signature.handler,
                framed(&capture_schema),
                framed(&worker_material),
                framed(&host_port_material),
            );
            let descriptor_material = format!(
                "witchy.glamour.work-descriptor.v1|{site}"
            );
            let result_schema_material = format!(
                "witchy.glamour.work-result-schema.v1|message={}|{site}",
                runtime_type_material(message_identity),
            );
            let completion_material = format!(
                "witchy.glamour.work-completion.v1|{site}"
            );
            let descriptor_id = checked_metadata_id(
                "work descriptor",
                &descriptor_material,
                &mut descriptor_ids,
            )?;
            let result_schema_id = checked_metadata_id(
                "work result schema",
                &result_schema_material,
                &mut result_schema_ids,
            )?;
            let completion_id = checked_metadata_id(
                "work completion",
                &completion_material,
                &mut completion_ids,
            )?;
            let owner_scope_id = checked_metadata_id(
                "work owner scope",
                &format!(
                    "witchy.glamour.work-owner.v1|island={island_identity}|owner={}",
                    declaration_material(&owner),
                ),
                &mut owner_scope_ids,
            )?;
            let browser_policy = browser_policies
                .get(&(owner_name.clone(), ordinal))
                .ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour work descriptor `{owner_name}#{ordinal}` has no compiler-authenticated browser policy"
                    ))
                })?
                .clone();
            work.push(GlamourWorkMetadata {
                channel: signature.channel.into(),
                kind: signature.kind.into(),
                handler: signature.handler.into(),
                owner_name: owner_name.clone(),
                owner: owner.clone(),
                ordinal,
                call_name: name.clone(),
                descriptor_id,
                result_schema_id,
                completion_id,
                owner_scope_id,
                browser_policy,
                completion_source,
                completion: completion.clone(),
                completion_message_type,
                completion_captures,
                worker_task,
                host_port,
            });
            Ok(())
        })?;
        pending.extend(discovered.into_iter().filter(|name| !visited.contains(name)));
    }
    work.sort_by(|left, right| {
        declaration_material(&left.owner)
            .cmp(&declaration_material(&right.owner))
            .then_with(|| left.ordinal.cmp(&right.ordinal))
    });
    Ok(work)
}

fn glamour_work_map_metadata(
    module: &Module,
    type_table: &witchy_types::typeck::TypeTable,
    catalog: &RuntimeDeclarationCatalog,
    program: &DeclarationIdentity,
    island_identity: &str,
    roots: [&str; 3],
) -> Result<Vec<GlamourWorkMapMetadata>, CodegenError> {
    let functions = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some((function.name.clone(), function)),
            _ => None,
        })
        .collect::<BTreeMap<String, &Function>>();
    let global_values = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(function) => Some(function.name.clone()),
            Item::Const { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let global_callables = module
        .items
        .iter()
        .flat_map(|item| match item {
            Item::Function(function) => vec![function.name.clone()],
            Item::Type(definition) => definition
                .variants
                .iter()
                .map(|variant| variant.name.clone())
                .collect(),
            _ => Vec::new(),
        })
        .collect::<BTreeSet<_>>();
    let mut pending = roots.into_iter().map(str::to_string).collect::<BTreeSet<_>>();
    let mut visited = BTreeSet::new();
    let mut maps = Vec::new();
    let mut mapper_ids = BTreeMap::new();

    while let Some(owner_name) = pending.iter().next().cloned() {
        pending.remove(&owner_name);
        if !visited.insert(owner_name.clone()) {
            continue;
        }
        let function = functions.get(&owner_name).ok_or_else(|| {
            island_metadata_error(format!(
                "Glamour work root `{owner_name}` is missing from the checked module"
            ))
        })?;
        let owner = catalog
            .resolve(&owner_name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour map owner `{owner_name}` has no authenticated declaration"
                ))
            })?
            .clone();
        let mut ordinal = 0_u32;
        let mut discovered = BTreeSet::new();
        visit_block(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression {
                if functions.contains_key(name)
                    && catalog
                        .resolve(name, DeclarationKind::Function)
                        .is_some_and(|callee| callee.package() == program.package())
                {
                    discovered.insert(name.clone());
                }
            }
            if glamour_work_map_channel_from_expression(expression).is_none() {
                return Ok(());
            }
            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                island_metadata_error(format!(
                    "Glamour map declaration `{owner_name}` has too many map sites",
                ))
            })?;
            let Some((channel, _receiver, mapper)) =
                glamour_work_map_expression(expression, type_table)
            else {
                return Ok(());
            };
            let mapper_type = type_table
                .type_of(mapper)
                .and_then(witchy_types::typeck::ty_to_ast)
                .ok_or_else(|| {
                    island_metadata_error(format!(
                        "Glamour `{channel}.map` in `{owner_name}` has no concrete mapper type",
                    ))
                })?;
            let Type::Fn(inputs, output, _) = mapper_type.unqualified() else {
                return Err(island_metadata_error(format!(
                    "Glamour `{channel}.map` in `{owner_name}` mapper is not a function",
                )));
            };
            let [input_type] = inputs.as_slice() else {
                return Err(island_metadata_error(format!(
                    "Glamour `{channel}.map` in `{owner_name}` mapper must accept one message",
                )));
            };
            let input_type = input_type.clone();
            let output_type = output.as_ref().clone();
            catalog
                .capability_free_type_identity(&input_type, module)
                .map_err(|error| island_metadata_error(format!(
                    "Glamour `{channel}.map` in `{owner_name}` cannot persist its input message: {error}",
                )))?;
            catalog
                .capability_free_type_identity(&output_type, module)
                .map_err(|error| island_metadata_error(format!(
                    "Glamour `{channel}.map` in `{owner_name}` cannot persist its output message: {error}",
                )))?;
            let captures = match mapper {
                Expr::Lambda { .. } => checked_completion_captures(
                    mapper,
                    type_table,
                    module,
                    catalog,
                    &global_values,
                    &owner_name,
                )?,
                Expr::Var(name) if global_callables.contains(name) => Vec::new(),
                _ => {
                    return Err(island_metadata_error(format!(
                        "Glamour `{channel}.map` in `{owner_name}` uses a dynamically selected mapper; use a named function, message constructor, or closed lambda",
                    )));
                }
            };
            let mapper_source = witchy_syntax::format::expr_str(mapper);
            let input_identity = catalog
                .capability_free_type_identity(&input_type, module)
                .map_err(|error| island_metadata_error(error.to_string()))?;
            let output_identity = catalog
                .capability_free_type_identity(&output_type, module)
                .map_err(|error| island_metadata_error(error.to_string()))?;
            let capture_schema = captures
                .iter()
                .map(|capture| {
                    let identity = catalog
                        .capability_free_type_identity(&capture.ty, module)
                        .map_err(|error| island_metadata_error(error.to_string()))?;
                    Ok(format!(
                        "{}{}",
                        framed(&capture.name),
                        framed(&runtime_type_material(&identity)),
                    ))
                })
                .collect::<Result<String, CodegenError>>()?;
            let material = format!(
                "witchy.glamour.work-map.v1|island={island_identity}|owner={}|ordinal={ordinal}|channel={channel}|mapper={mapper_source}|input={}|output={}|captures={}",
                declaration_material(&owner),
                runtime_type_material(&input_identity),
                runtime_type_material(&output_identity),
                framed(&capture_schema),
            );
            let mapper_id = checked_metadata_id("work mapper", &material, &mut mapper_ids)?;
            maps.push(GlamourWorkMapMetadata {
                channel: channel.into(),
                owner_name: owner_name.clone(),
                ordinal,
                mapper_id,
                mapper_source,
                mapper: mapper.clone(),
                input_type,
                output_type,
                captures,
            });
            Ok(())
        })?;
        pending.extend(discovered.into_iter().filter(|name| !visited.contains(name)));
    }
    maps.sort_by(|left, right| {
        left.owner_name
            .cmp(&right.owner_name)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
    });
    Ok(maps)
}

fn glamour_mapped_work_metadata(
    island_identity: &str,
    work: &[GlamourWorkMetadata],
    maps: &[GlamourWorkMapMetadata],
) -> Result<Vec<GlamourMappedWorkMetadata>, CodegenError> {
    const MAX_MAPPED_WORK_ENTRIES: usize = 4096;

    #[derive(Clone)]
    struct CallbackState {
        channel: String,
        kind: String,
        handler: String,
        owner_name: String,
        owner: DeclarationIdentity,
        descriptor_id: u32,
        result_schema_id: u32,
        completion_id: u32,
        owner_scope_id: u32,
        browser_policy: GlamourBrowserPolicyMetadata,
        output_type: Type,
        composition: Vec<u32>,
    }

    let mut descriptor_ids = BTreeMap::new();
    let mut completion_ids = BTreeMap::new();
    let mut states = Vec::with_capacity(work.len());
    for site in work {
        descriptor_ids.insert(
            site.descriptor_id,
            format!(
                "witchy.glamour.base-descriptor.v1|{}|{}",
                site.descriptor_id, site.completion_source,
            ),
        );
        completion_ids.insert(
            site.completion_id,
            format!(
                "witchy.glamour.base-completion.v1|{}|{}",
                site.completion_id, site.completion_source,
            ),
        );
        states.push(CallbackState {
            channel: site.channel.clone(),
            kind: site.kind.clone(),
            handler: site.handler.clone(),
            owner_name: site.owner_name.clone(),
            owner: site.owner.clone(),
            descriptor_id: site.descriptor_id,
            result_schema_id: site.result_schema_id,
            completion_id: site.completion_id,
            owner_scope_id: site.owner_scope_id,
            browser_policy: site.browser_policy.clone(),
            output_type: site.completion_message_type.clone(),
            composition: Vec::new(),
        });
    }

    let mut mapped = Vec::new();
    let mut cursor = 0_usize;
    while cursor < states.len() {
        let previous = states[cursor].clone();
        cursor += 1;
        for mapper in maps {
            if !matches!(
                (mapper.channel.as_str(), previous.channel.as_str()),
                ("cmd", "effect") | ("sub", "subscription")
            )
                || mapper.input_type.unqualified() != previous.output_type.unqualified()
                || previous.composition.contains(&mapper.mapper_id)
            {
                continue;
            }
            if mapped.len() == MAX_MAPPED_WORK_ENTRIES {
                return Err(island_metadata_error(format!(
                    "island callback composition exceeds {MAX_MAPPED_WORK_ENTRIES} generated entries; split the command/subscription mapping graph",
                )));
            }
            let mut composition = previous.composition.clone();
            composition.push(mapper.mapper_id);
            let composition_material = composition
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let descriptor_material = format!(
                "witchy.glamour.mapped-work-descriptor.v1|island={island_identity}|previous={}|mapper={}|composition={composition_material}",
                previous.descriptor_id, mapper.mapper_id,
            );
            let completion_material = format!(
                "witchy.glamour.mapped-work-completion.v1|island={island_identity}|previous={}|mapper={}|composition={composition_material}",
                previous.completion_id, mapper.mapper_id,
            );
            let descriptor_id = checked_metadata_id(
                "mapped work descriptor",
                &descriptor_material,
                &mut descriptor_ids,
            )?;
            let completion_id = checked_metadata_id(
                "mapped work completion",
                &completion_material,
                &mut completion_ids,
            )?;
            let entry = GlamourMappedWorkMetadata {
                channel: previous.channel.clone(),
                kind: previous.kind.clone(),
                handler: previous.handler.clone(),
                owner_name: previous.owner_name.clone(),
                owner: previous.owner.clone(),
                mapper_id: mapper.mapper_id,
                mapper_source: mapper.mapper_source.clone(),
                mapper: mapper.mapper.clone(),
                mapper_captures: mapper.captures.clone(),
                input_type: mapper.input_type.clone(),
                output_type: mapper.output_type.clone(),
                previous_descriptor_id: previous.descriptor_id,
                previous_completion_id: previous.completion_id,
                descriptor_id,
                result_schema_id: previous.result_schema_id,
                completion_id,
                owner_scope_id: previous.owner_scope_id,
                browser_policy: previous.browser_policy.clone(),
                composition: composition.clone(),
            };
            mapped.push(entry);
            states.push(CallbackState {
                channel: previous.channel.clone(),
                kind: previous.kind.clone(),
                handler: previous.handler.clone(),
                owner_name: previous.owner_name.clone(),
                owner: previous.owner.clone(),
                descriptor_id,
                result_schema_id: previous.result_schema_id,
                completion_id,
                owner_scope_id: previous.owner_scope_id,
                browser_policy: previous.browser_policy.clone(),
                output_type: mapper.output_type.clone(),
                composition,
            });
        }
    }
    mapped.sort_by(|left, right| {
        left.mapper_id
            .cmp(&right.mapper_id)
            .then_with(|| left.previous_descriptor_id.cmp(&right.previous_descriptor_id))
            .then_with(|| left.previous_completion_id.cmp(&right.previous_completion_id))
    });
    Ok(mapped)
}

fn glamour_work_map_channel_from_expression(expression: &Expr) -> Option<&'static str> {
    match expression {
        Expr::MethodCall { method, .. } if method == "map" => Some("method"),
        Expr::Call { name, .. } if name.ends_with("__map") => Some("direct"),
        _ => None,
    }
}

fn glamour_work_map_expression<'a>(
    expression: &'a Expr,
    type_table: &witchy_types::typeck::TypeTable,
) -> Option<(&'static str, &'a Expr, &'a Expr)> {
    let (declared_channel, receiver, mapper) = match expression {
        Expr::MethodCall { receiver, method, args } if method == "map" => {
            let [mapper] = args.as_slice() else { return None };
            (None, receiver.as_ref(), mapper)
        }
        Expr::Call { name, args } if name.ends_with("__map") => {
            let [receiver, mapper] = args.as_slice() else { return None };
            let channel = match name.rsplit('.').next().unwrap_or(name).split_once("__") {
                Some(("Cmd", "map")) => Some("cmd"),
                Some(("Sub", "map")) => Some("sub"),
                _ => None,
            };
            (channel, receiver, mapper)
        }
        _ => return None,
    };
    let channel = declared_channel.or_else(|| glamour_work_map_receiver_channel(receiver, type_table))?;
    Some((channel, receiver, mapper))
}

fn glamour_work_map_receiver_channel(
    receiver: &Expr,
    type_table: &witchy_types::typeck::TypeTable,
) -> Option<&'static str> {
    let ty = type_table
        .type_of(receiver)
        .and_then(witchy_types::typeck::ty_to_ast)?;
    let Type::Named(name, arguments) = ty.unqualified() else {
        return None;
    };
    if arguments.len() != 1 {
        return None;
    }
    match name.as_str() {
        "glamour.Cmd" | "Cmd" => Some("cmd"),
        "glamour.Sub" | "Sub" => Some("sub"),
        _ => None,
    }
}

fn checked_completion_captures(
    completion: &Expr,
    type_table: &witchy_types::typeck::TypeTable,
    module: &Module,
    catalog: &RuntimeDeclarationCatalog,
    global_values: &BTreeSet<String>,
    owner_name: &str,
) -> Result<Vec<GlamourWorkCaptureMetadata>, CodegenError> {
    let Expr::Lambda { params, body, .. } = completion else {
        return Ok(Vec::new());
    };
    let names = witchy_syntax::lambda_scan::scan_lambda(params, body)
        .captures()
        .into_iter()
        .filter(|name| !global_values.contains(name));
    let mut captures = Vec::new();
    for name in names {
        let mut capture_type = None;
        visit_block(body, &mut |expression| {
            if capture_type.is_none()
                && matches!(expression, Expr::Var(candidate) if candidate == &name)
            {
                capture_type = type_table
                    .type_of(expression)
                    .and_then(witchy_types::typeck::ty_to_ast);
            }
            Ok(())
        })?;
        let ty = capture_type.ok_or_else(|| {
            island_metadata_error(format!(
                "Glamour completion in `{owner_name}` captures `{name}`, but its concrete storage type is unavailable"
            ))
        })?;
        catalog
            .capability_free_type_identity(&ty, module)
            .map_err(|error| {
                island_metadata_error(format!(
                    "Glamour completion in `{owner_name}` cannot persist capture `{name}`: {error}"
                ))
            })?;
        captures.push(GlamourWorkCaptureMetadata { name, ty });
    }
    Ok(captures)
}

fn checked_metadata_id(
    label: &str,
    material: &str,
    identities: &mut BTreeMap<u32, String>,
) -> Result<u32, CodegenError> {
    let digest = Sha256::digest(material.as_bytes());
    let id = u32::from_be_bytes(digest[..4].try_into().expect("SHA-256 prefix"));
    if id == 0 {
        return Err(island_metadata_error(format!(
            "{label} produces reserved identity zero"
        )));
    }
    if let Some(existing) = identities.insert(id, material.into()) {
        if existing != material {
            return Err(island_metadata_error(format!(
                "{label} identity collision at {id}"
            )));
        }
    }
    Ok(id)
}

fn island_program_types(
    module: &Module,
    catalog: &RuntimeDeclarationCatalog,
    key: &str,
    program_name: &str,
) -> Result<(Type, Type, Type), CodegenError> {
    let function = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == program_name => Some(function),
            _ => None,
        })
        .ok_or_else(|| {
            island_metadata_error(format!(
                "island `{key}` program factory `{program_name}` is missing from the checked module"
            ))
        })?;
    if !function.params.is_empty() {
        return Err(island_metadata_error(format!(
            "island `{key}` program factory must declare no parameters"
        )));
    }
    let Some(Type::Named(program_type, arguments)) =
        function.ret.as_ref().map(Type::unqualified)
    else {
        return Err(island_metadata_error(format!(
            "island `{key}` program factory must explicitly return `glamour.Program(auth, model, msg)`"
        )));
    };
    let Some(program_declaration) = catalog.resolve(program_type, DeclarationKind::Type) else {
        return Err(island_metadata_error(format!(
            "island `{key}` program factory return type has no authenticated declaration"
        )));
    };
    if !is_glamour_declaration(program_declaration, "Program") || arguments.len() != 3 {
        return Err(island_metadata_error(format!(
            "island `{key}` program factory must explicitly return toolchain `glamour.Program(auth, model, msg)`"
        )));
    }
    Ok((
        arguments[0].clone(),
        arguments[1].clone(),
        arguments[2].clone(),
    ))
}

fn island_metadata_error(message: impl Into<String>) -> CodegenError {
    CodegenError {
        message: format!("cannot build Glamour island metadata: {}", message.into()),
    }
}

fn valid_island_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn island_activation(
    key: &str,
    expression: &Expr,
) -> Result<(String, Option<String>), CodegenError> {
    let Expr::Ctor { name, args } = expression else {
        return Err(island_metadata_error(format!(
            "island `{key}` activation policy must be a direct Glamour constructor"
        )));
    };
    match (name.as_str(), args.as_slice()) {
        ("glamour.OnLoad", []) => Ok(("load".into(), None)),
        ("glamour.OnIdle", []) => Ok(("idle".into(), None)),
        ("glamour.OnVisible", []) => Ok(("visible".into(), None)),
        ("glamour.OnInteraction", []) => Ok(("interaction".into(), None)),
        (
            "glamour.OnMedia",
            [Expr::Ctor {
                name,
                args,
            }],
        ) if name == "glamour.MediaQuery" => match args.as_slice() {
            [Expr::Str(query)] => Ok(("media".into(), Some(query.clone()))),
            _ => Err(island_metadata_error(format!(
                "island `{key}` media condition must contain one checked string"
            ))),
        },
        _ => Err(island_metadata_error(format!(
            "island `{key}` has unsupported activation constructor `{name}`"
        ))),
    }
}

fn declaration_material(identity: &DeclarationIdentity) -> String {
    format!(
        "{:?}:{}@{}:{:?}:{}",
        identity.package().source(),
        identity.package().name(),
        identity.package().version(),
        identity.module(),
        identity.name(),
    )
}

fn interactive_source_identity(
    owner: &DeclarationIdentity,
    ordinal: usize,
    constructor: &str,
) -> String {
    let material = format!(
        "witchy.glamour.interactive-origin.v1|owner={}|structural-ordinal={ordinal}|constructor={constructor}",
        declaration_material(owner),
    );
    let digest = Sha256::digest(material.as_bytes());
    format!("interactive-origin1-{}", hex_digest(&digest))
}

fn runtime_type_material(identity: &RuntimeTypeIdentity) -> String {
    match identity {
        RuntimeTypeIdentity::Primitive(primitive) => match primitive {
            PrimitiveType::Int => "int".into(),
            PrimitiveType::Float => "float".into(),
            PrimitiveType::Duration => "duration".into(),
            PrimitiveType::String => "string".into(),
            PrimitiveType::Bytes => "bytes".into(),
            PrimitiveType::Bool => "bool".into(),
            PrimitiveType::Unit => "unit".into(),
        },
        RuntimeTypeIdentity::List(item) => {
            format!("list{}", framed(&runtime_type_material(item)))
        }
        RuntimeTypeIdentity::Tuple(items) => sequence_material("tuple", items),
        RuntimeTypeIdentity::Function {
            params,
            result,
            conventions,
            access,
            ..
        } => {
            let params = params
                .iter()
                .map(|parameter| {
                    format!(
                        "{}{}",
                        if parameter.is_authority() { "authority" } else { "value" },
                        framed(&runtime_type_material(parameter.identity())),
                    )
                })
                .collect::<String>();
            let conventions = conventions
                .iter()
                .map(|convention| match convention {
                    RuntimeConvention::Let => "let",
                    RuntimeConvention::Borrow => "borrow",
                    RuntimeConvention::Var => "var",
                    RuntimeConvention::Own => "own",
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "function{}{}{}",
                framed(&format!("params{}{}", params.len(), framed(&params))),
                framed(&runtime_type_material(result)),
                framed(&format!("{conventions}|access={access:?}")),
            )
        }
        RuntimeTypeIdentity::Nominal {
            declaration,
            arguments,
        } => format!(
            "nominal{}{}",
            framed(&declaration_material(declaration)),
            framed(&sequence_material("arguments", arguments)),
        ),
        RuntimeTypeIdentity::Existential {
            declaration,
            arguments,
        } => format!(
            "existential{}{}",
            framed(&declaration_material(declaration)),
            framed(&sequence_material("arguments", arguments)),
        ),
        RuntimeTypeIdentity::Record(fields) => {
            let field_count = fields.len();
            let fields = fields
                .iter()
                .map(|(name, ty)| {
                    format!(
                        "{}{}",
                        framed(name),
                        framed(&runtime_type_material(ty)),
                    )
                })
                .collect::<String>();
            format!("record{field_count}{}", framed(&fields))
        }
        RuntimeTypeIdentity::Union(variants) => {
            let variant_count = variants.len();
            let variants = variants
                .iter()
                .map(|variant| {
                    format!(
                        "{}{}",
                        framed(&variant.tag),
                        framed(&sequence_material("payloads", &variant.payloads)),
                    )
                })
                .collect::<String>();
            format!("union{variant_count}{}", framed(&variants))
        }
        RuntimeTypeIdentity::Capability { authority, .. } => {
            format!("capability:{authority:?}")
        }
    }
}

fn sequence_material(label: &str, items: &[RuntimeTypeIdentity]) -> String {
    let item_count = items.len();
    let items = items
        .iter()
        .map(runtime_type_material)
        .map(|item| framed(&item))
        .collect::<String>();
    format!("{label}{item_count}{}", framed(&items))
}

fn framed(value: &str) -> String {
    format!("{}:{value}", value.len())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn metadata_error(message: impl Into<String>) -> CodegenError {
    CodegenError {
        message: format!("cannot build Glamour template metadata: {}", message.into()),
    }
}

fn is_glamour_declaration(identity: &DeclarationIdentity, name: &str) -> bool {
    identity.package().source() == &PackageSource::Toolchain
        && identity.package().name() == "witchy/glamour"
        && identity.module() == ["src", "glamour"]
        && identity.name() == name
}

fn origin_key(span: &SourceSpan) -> String {
    format!("{}:{}", span.module, span.start.line)
}

fn template_candidate(
    expression: &Expr,
) -> Result<Option<(String, Candidate)>, CodegenError> {
    let Expr::Call { name, args } = expression else {
        return Ok(None);
    };
    if name != "glamour.planned" {
        return Ok(None);
    }
    let [Expr::Str(identity), Expr::Str(origin), Expr::List(slot_values), node_expression] =
        args.as_slice()
    else {
        return Err(metadata_error(
            "`glamour.planned` has a non-static compiler metadata shape",
        ));
    };
    if !valid_template_identity(identity) {
        return Err(metadata_error(format!(
            "`glamour.planned` has invalid identity `{identity}`"
        )));
    }
    let mut declared_slots = Vec::with_capacity(slot_values.len());
    let mut indices = BTreeSet::new();
    for slot in slot_values {
        let Expr::Call {
            name: constructor,
            args,
        } = slot
        else {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` contains a non-call slot"
            )));
        };
        let [Expr::Int(index), Expr::Str(kind), Expr::Str(slot_name)] = args.as_slice() else {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` contains a non-static slot"
            )));
        };
        if constructor != "glamour.template_slot" {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` contains an unauthenticated slot constructor"
            )));
        }
        let index = u32::try_from(*index).map_err(|_| {
            metadata_error(format!(
                "Glamour template `{identity}` has an invalid slot index"
            ))
        })?;
        if !indices.insert(index) {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` repeats slot {index}"
            )));
        }
        let wire_id = index.checked_add(1).ok_or_else(|| {
            metadata_error(format!(
                "Glamour template `{identity}` slot index exceeds the wire identity range"
            ))
        })?;
        if !matches!(
            kind.as_str(),
            "text"
                | "child"
                | "event"
                | "url"
                | "property"
                | "boolean"
                | "class"
                | "aria"
                | "attribute"
        ) {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` has unknown slot kind `{kind}`"
            )));
        }
        declared_slots.push((
            index,
            wire_id,
            kind.clone(),
            slot_name.clone(),
        ));
    }
    let mut next_node = 1_u32;
    let mut observed_slots = Vec::new();
    let root = template_node(
        identity,
        node_expression,
        &mut next_node,
        &mut observed_slots,
    )?;
    if declared_slots.len() != observed_slots.len() {
        return Err(metadata_error(format!(
            "Glamour template `{identity}` declares {} slot(s) but its checked tree has {}",
            declared_slots.len(),
            observed_slots.len(),
        )));
    }
    let slots = declared_slots
        .into_iter()
        .zip(observed_slots)
        .map(|((index, wire_id, kind, name), observed)| {
            if kind != observed.kind || name != observed.name {
                return Err(metadata_error(format!(
                    "Glamour template `{identity}` slot {index} disagrees with its checked node sink"
                )));
            }
            Ok(GlamourTemplateSlotMetadata {
                index,
                wire_id,
                node: observed.node,
                kind,
                name,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some((
        origin.clone(),
        Candidate {
            identity: identity.clone(),
            slots,
            root,
        },
    )))
}

fn template_node(
    identity: &str,
    expression: &Expr,
    next_node: &mut u32,
    slots: &mut Vec<ObservedSlot>,
) -> Result<GlamourTemplateNodeMetadata, CodegenError> {
    let Expr::Call { name, args } = expression else {
        return Err(metadata_error(format!(
            "Glamour template `{identity}` contains a non-constructor node"
        )));
    };
    let node = allocate_node(identity, next_node)?;
    match (name.as_str(), args.as_slice()) {
        ("glamour.template_child_region", [Expr::Str(_id), _value]) => {
            slots.push(ObservedSlot {
                node,
                kind: "child".into(),
                name: String::new(),
            });
            Ok(GlamourTemplateNodeMetadata::Text {
                node,
                text: String::new(),
            })
        }
        ("glamour.text", [value]) => {
            let text = match value {
                Expr::Str(value) => value.clone(),
                _ => {
                    slots.push(ObservedSlot {
                        node,
                        kind: "text".into(),
                        name: String::new(),
                    });
                    String::new()
                }
            };
            Ok(GlamourTemplateNodeMetadata::Text { node, text })
        }
        (
            "glamour.element",
            [Expr::Str(tag), Expr::List(attribute_values), Expr::List(child_values)],
        ) => {
            let mut attributes = Vec::new();
            let mut names = BTreeSet::new();
            for attribute in attribute_values {
                if let Some(attribute) =
                    template_attribute(identity, node, attribute, slots)?
                {
                    if !names.insert(attribute.name.clone()) {
                        return Err(metadata_error(format!(
                            "Glamour template `{identity}` repeats static attribute `{}`",
                            attribute.name
                        )));
                    }
                    attributes.push(attribute);
                }
            }
            attributes.sort_by(|left, right| left.name.cmp(&right.name));
            let mut children = Vec::with_capacity(child_values.len());
            for child in child_values {
                children.push(template_node(identity, child, next_node, slots)?);
            }
            Ok(GlamourTemplateNodeMetadata::Element {
                node,
                tag: tag.clone(),
                attributes,
                children,
            })
        }
        _ => Err(metadata_error(format!(
            "Glamour template `{identity}` contains unsupported node constructor `{name}`"
        ))),
    }
}

fn allocate_node(identity: &str, next_node: &mut u32) -> Result<u32, CodegenError> {
    let node = *next_node;
    if node == 0 {
        return Err(metadata_error(format!(
            "Glamour template `{identity}` exceeds the local node identity range"
        )));
    }
    *next_node = next_node.checked_add(1).ok_or_else(|| {
        metadata_error(format!(
            "Glamour template `{identity}` exceeds the local node identity range"
        ))
    })?;
    Ok(node)
}

fn template_attribute(
    identity: &str,
    node: u32,
    expression: &Expr,
    slots: &mut Vec<ObservedSlot>,
) -> Result<Option<GlamourTemplateAttributeMetadata>, CodegenError> {
    let Expr::Call { name, args } = expression else {
        return Err(metadata_error(format!(
            "Glamour template `{identity}` contains a non-constructor attribute"
        )));
    };
    if name == "glamour.static_class_attribute" {
        let [Expr::List(values)] = args.as_slice() else {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` has a non-static class attribute"
            )));
        };
        let classes = values
            .iter()
            .map(|value| match value {
                Expr::Str(value) => Ok(value.as_str()),
                _ => Err(metadata_error(format!(
                    "Glamour template `{identity}` has a non-static class token"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Some(GlamourTemplateAttributeMetadata {
            name: "class".into(),
            value: classes.join(" "),
        }));
    }
    if name == "glamour.class_attribute" {
        let [_value] = args.as_slice() else {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` has a malformed class slot"
            )));
        };
        slots.push(ObservedSlot {
            node,
            kind: "class".into(),
            name: "class".into(),
        });
        return Ok(None);
    }
    let [Expr::Str(attribute_name), value] = args.as_slice() else {
        return Err(metadata_error(format!(
            "Glamour template `{identity}` has malformed attribute constructor `{name}`"
        )));
    };
    if name == "glamour.on" {
        slots.push(ObservedSlot {
            node,
            kind: "event".into(),
            name: attribute_name.clone(),
        });
        return Ok(None);
    }
    let kind = match name.as_str() {
        "glamour.navigation_url_attribute"
        | "glamour.form_url_attribute"
        | "glamour.asset_url_attribute" => "url",
        "glamour.property" => "property",
        "glamour.boolean_attribute" => "boolean",
        "glamour.aria_attribute" => "aria",
        "glamour.attribute" => "attribute",
        "glamour.static_url_attribute"
        | "glamour.static_form_url_attribute"
        | "glamour.static_asset_url_attribute" => "url",
        _ => {
            return Err(metadata_error(format!(
                "Glamour template `{identity}` contains unsupported attribute constructor `{name}`"
            )))
        }
    };
    let static_value = match value {
        Expr::Str(value) => Some(value.clone()),
        Expr::Bool(value) if kind == "boolean" => {
            if *value {
                Some(String::new())
            } else {
                return Ok(None);
            }
        }
        _ => None,
    };
    if let Some(value) = static_value {
        return Ok(Some(GlamourTemplateAttributeMetadata {
            name: attribute_name.clone(),
            value,
        }));
    }
    slots.push(ObservedSlot {
        node,
        kind: kind.into(),
        name: attribute_name.clone(),
    });
    Ok(None)
}

fn valid_template_identity(identity: &str) -> bool {
    identity
        .strip_prefix("glamour-tp1-")
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn template_wire_id(identity: &str) -> Result<u32, CodegenError> {
    let digest = identity
        .strip_prefix("glamour-tp1-")
        .expect("validated template identity");
    let wire_id = u32::from_str_radix(&digest[..8], 16).map_err(|_| {
        metadata_error(format!(
            "Glamour template `{identity}` cannot produce a wire identity"
        ))
    })?;
    if wire_id == 0 {
        return Err(metadata_error(format!(
            "Glamour template `{identity}` produces reserved wire identity zero"
        )));
    }
    Ok(wire_id)
}

fn visit_function_interactive_calls(
    module: &Module,
    visitor: &mut impl FnMut(&str, usize, &Expr) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    for item in &module.items {
        let Item::Function(function) = item else {
            continue;
        };
        let mut ordinal = 0_usize;
        visit_block(&function.body, &mut |expression| {
            if matches!(expression, Expr::Call { name, .. } if matches!(name.as_str(), "glamour.interactive" | "glamour.client_region")) {
                visitor(&function.name, ordinal, expression)?;
                ordinal += 1;
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn rewrite_interactive_calls(
    module: &mut Module,
    catalog: &RuntimeDeclarationCatalog,
) -> Result<(), CodegenError> {
    for item in &mut module.items {
        let Item::Function(function) = item else {
            continue;
        };
        let mut contains_interactive = false;
        visit_block(&function.body, &mut |expression| {
            if matches!(expression, Expr::Call { name, .. } if matches!(name.as_str(), "glamour.interactive" | "glamour.client_region")) {
                contains_interactive = true;
            }
            Ok(())
        })?;
        if !contains_interactive {
            continue;
        }
        let owner = catalog
            .resolve(&function.name, DeclarationKind::Function)
            .ok_or_else(|| {
                island_metadata_error(format!(
                    "checked interactive owner `{}` has no authenticated declaration",
                    function.name,
                ))
            })?
            .clone();
        let mut ordinal = 0_usize;
        rewrite_block_interactive(&mut function.body, &owner, &mut ordinal)?;
    }
    Ok(())
}

fn rewrite_block_interactive(
    block: &mut Block,
    owner: &DeclarationIdentity,
    ordinal: &mut usize,
) -> Result<(), CodegenError> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => rewrite_expr_interactive(value, owner, ordinal)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn rewrite_expr_interactive(
    expression: &mut Expr,
    owner: &DeclarationIdentity,
    ordinal: &mut usize,
) -> Result<(), CodegenError> {
    if let Expr::Call { name, args } = expression
        && matches!(name.as_str(), "glamour.interactive" | "glamour.client_region")
    {
        let constructor = if name == "glamour.interactive" {
            "interactive"
        } else {
            "client_region"
        };
        if args.len() != 2 {
            return Err(island_metadata_error(format!(
                "`glamour.{constructor}` must use its checked two-argument positional form"
            )));
        }
        let source_identity = interactive_source_identity(owner, *ordinal, constructor);
        *ordinal += 1;
        *name = format!("glamour.{constructor}_with_origin");
        args.insert(0, Expr::Str(source_identity));
    }

    match expression {
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                rewrite_expr_interactive(value, owner, ordinal)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for argument in args {
                rewrite_expr_interactive(argument, owner, ordinal)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                rewrite_expr_interactive(argument, owner, ordinal)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            rewrite_expr_interactive(receiver, owner, ordinal)?;
            for (_, argument) in args {
                rewrite_expr_interactive(argument, owner, ordinal)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            rewrite_expr_interactive(receiver, owner, ordinal)?;
            for argument in args {
                rewrite_expr_interactive(argument, owner, ordinal)?;
            }
        }
        Expr::Apply { func, args } => {
            rewrite_expr_interactive(func, owner, ordinal)?;
            for argument in args {
                rewrite_expr_interactive(argument, owner, ordinal)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => {
            rewrite_expr_interactive(expr, owner, ordinal)?;
        }
        Expr::Field { base, .. } => rewrite_expr_interactive(base, owner, ordinal)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => {
            rewrite_block_interactive(body, owner, ordinal)?;
        }
        Expr::RecordUpdate { base, fields, .. } => {
            rewrite_expr_interactive(base, owner, ordinal)?;
            for (_, value) in fields {
                rewrite_expr_interactive(value, owner, ordinal)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                rewrite_expr_interactive(value, owner, ordinal)?;
            }
            if let Some(spread) = spread {
                rewrite_expr_interactive(spread, owner, ordinal)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            rewrite_expr_interactive(lhs, owner, ordinal)?;
            rewrite_expr_interactive(rhs, owner, ordinal)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            rewrite_expr_interactive(cond, owner, ordinal)?;
            rewrite_block_interactive(then_block, owner, ordinal)?;
            if let Some(else_block) = else_block {
                rewrite_block_interactive(else_block, owner, ordinal)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            rewrite_expr_interactive(scrutinee, owner, ordinal)?;
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    rewrite_expr_interactive(guard, owner, ordinal)?;
                }
                rewrite_expr_interactive(&mut arm.body, owner, ordinal)?;
            }
        }
        Expr::While { cond, body } => {
            rewrite_expr_interactive(cond, owner, ordinal)?;
            rewrite_block_interactive(body, owner, ordinal)?;
        }
        Expr::For { iter, body, .. } => {
            rewrite_expr_interactive(iter, owner, ordinal)?;
            rewrite_block_interactive(body, owner, ordinal)?;
        }
        Expr::Range { lo, hi, .. } => {
            rewrite_expr_interactive(lo, owner, ordinal)?;
            rewrite_expr_interactive(hi, owner, ordinal)?;
        }
        Expr::Index { base, index } => {
            rewrite_expr_interactive(base, owner, ordinal)?;
            rewrite_expr_interactive(index, owner, ordinal)?;
        }
        Expr::WhileLet {
            scrutinee, body, ..
        } => {
            rewrite_expr_interactive(scrutinee, owner, ordinal)?;
            rewrite_block_interactive(body, owner, ordinal)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

fn visit_module_exprs(
    module: &witchy_syntax::ast::Module,
    visitor: &mut impl FnMut(&Expr) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    for item in &module.items {
        match item {
            Item::Function(function) => visit_block(&function.body, visitor)?,
            Item::Impl(definition) => {
                for method in &definition.methods {
                    visit_block(&method.body, visitor)?;
                }
            }
            Item::Trait(definition) => {
                for method in &definition.methods {
                    if let Some(body) = &method.default {
                        visit_block(body, visitor)?;
                    }
                }
            }
            Item::Const { value, .. } => visit_expr(value, visitor)?,
            Item::Type(_) | Item::TypeAlias { .. } | Item::Comptime(_) => {}
        }
    }
    Ok(())
}

fn visit_block(
    block: &Block,
    visitor: &mut impl FnMut(&Expr) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    for statement in &block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_expr(value, visitor)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn visit_expr(
    expression: &Expr,
    visitor: &mut impl FnMut(&Expr) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    visitor(expression)?;
    match expression {
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                visit_expr(value, visitor)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr(receiver, visitor)?;
            for (_, argument) in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr(receiver, visitor)?;
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::Apply { func, args } => {
            visit_expr(func, visitor)?;
            for argument in args {
                visit_expr(argument, visitor)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => visit_expr(expr, visitor)?,
        Expr::Field { base, .. } => visit_expr(base, visitor)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_block(body, visitor)?,
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr(base, visitor)?;
            for (_, value) in fields {
                visit_expr(value, visitor)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr(value, visitor)?;
            }
            if let Some(spread) = spread {
                visit_expr(spread, visitor)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr(lhs, visitor)?;
            visit_expr(rhs, visitor)?;
        }
        Expr::If {
            cond,
            then_block,
            else_block,
        } => {
            visit_expr(cond, visitor)?;
            visit_block(then_block, visitor)?;
            if let Some(else_block) = else_block {
                visit_block(else_block, visitor)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr(scrutinee, visitor)?;
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    visit_expr(guard, visitor)?;
                }
                visit_expr(&arm.body, visitor)?;
            }
        }
        Expr::While { cond, body } => {
            visit_expr(cond, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::For { iter, body, .. } => {
            visit_expr(iter, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr(lo, visitor)?;
            visit_expr(hi, visitor)?;
        }
        Expr::Index { base, index } => {
            visit_expr(base, visitor)?;
            visit_expr(index, visitor)?;
        }
        Expr::WhileLet {
            scrutinee, body, ..
        } => {
            visit_expr(scrutinee, visitor)?;
            visit_block(body, visitor)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}

fn visit_block_mut(
    block: &mut Block,
    visitor: &mut impl FnMut(&mut Expr) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    for statement in &mut block.stmts {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::LetPattern { value, .. }
            | Stmt::Yield(value)
            | Stmt::Expr(value)
            | Stmt::Return(Some(value)) => visit_expr_mut(value, visitor)?,
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => {}
        }
    }
    Ok(())
}

fn visit_expr_mut(
    expression: &mut Expr,
    visitor: &mut impl FnMut(&mut Expr) -> Result<(), CodegenError>,
) -> Result<(), CodegenError> {
    visitor(expression)?;
    match expression {
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                visit_expr_mut(value, visitor)?;
            }
        }
        Expr::Call { args, .. } | Expr::Ctor { args, .. } | Expr::AnonCtor { args, .. } => {
            for argument in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::LabeledCall { args, .. } => {
            for (_, argument) in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::LabeledMethodCall { receiver, args, .. } => {
            visit_expr_mut(receiver, visitor)?;
            for (_, argument) in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::MethodCall { receiver, args, .. }
        | Expr::ExistentialCall { receiver, args, .. } => {
            visit_expr_mut(receiver, visitor)?;
            for argument in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::Apply { func, args } => {
            visit_expr_mut(func, visitor)?;
            for argument in args {
                visit_expr_mut(argument, visitor)?;
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Try(expr)
        | Expr::As { expr, .. }
        | Expr::ExistentialPack { expr, .. }
        | Expr::ExistentialUpcast { expr, .. } => visit_expr_mut(expr, visitor)?,
        Expr::Field { base, .. } => visit_expr_mut(base, visitor)?,
        Expr::Lambda { body, .. } | Expr::Block(body) => visit_block_mut(body, visitor)?,
        Expr::RecordUpdate { base, fields, .. } => {
            visit_expr_mut(base, visitor)?;
            for (_, value) in fields {
                visit_expr_mut(value, visitor)?;
            }
        }
        Expr::Record { fields, spread, .. } => {
            for (_, value) in fields {
                visit_expr_mut(value, visitor)?;
            }
            if let Some(spread) = spread {
                visit_expr_mut(spread, visitor)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr_mut(lhs, visitor)?;
            visit_expr_mut(rhs, visitor)?;
        }
        Expr::If { cond, then_block, else_block } => {
            visit_expr_mut(cond, visitor)?;
            visit_block_mut(then_block, visitor)?;
            if let Some(else_block) = else_block {
                visit_block_mut(else_block, visitor)?;
            }
        }
        Expr::Match { scrutinee, arms } => {
            visit_expr_mut(scrutinee, visitor)?;
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    visit_expr_mut(guard, visitor)?;
                }
                visit_expr_mut(&mut arm.body, visitor)?;
            }
        }
        Expr::While { cond, body } => {
            visit_expr_mut(cond, visitor)?;
            visit_block_mut(body, visitor)?;
        }
        Expr::For { iter, body, .. } => {
            visit_expr_mut(iter, visitor)?;
            visit_block_mut(body, visitor)?;
        }
        Expr::Range { lo, hi, .. } => {
            visit_expr_mut(lo, visitor)?;
            visit_expr_mut(hi, visitor)?;
        }
        Expr::Index { base, index } => {
            visit_expr_mut(base, visitor)?;
            visit_expr_mut(index, visitor)?;
        }
        Expr::WhileLet { scrutinee, body, .. } => {
            visit_expr_mut(scrutinee, visitor)?;
            visit_block_mut(body, visitor)?;
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Duration(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::TaggedLit { .. } => {}
    }
    Ok(())
}
