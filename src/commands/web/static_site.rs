//! Typed, capability-free static-site evaluation and zero-runtime publication.

use super::*;

mod resources;

use resources::{
    publish_static_assets, static_asset_sources, static_preloads_from_values,
    static_styles_from_values, validate_css_asset_bindings, validate_emitted_preload,
    rewrite_static_css_assets,
};

type EvaluatedStaticSite = (
    Vec<StaticPage>,
    Vec<StaticAction>,
    Vec<StaticStyle>,
    Vec<StaticPreload>,
    Vec<StaticAsset>,
    Vec<StaticIsland>,
);

const MAX_STATIC_CONTENT_FILES: usize = 4096;
const MAX_STATIC_CONTENT_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_STATIC_CONTENT_BYTES: usize = 64 * 1024 * 1024;

pub(super) fn check_static_project(project: Project) -> Result<CheckedStaticSite, WebCommandError> {
    let (checked, _) = crate::link_file_checked(path_text(&project.entry)?)
        .map_err(WebCommandError::failure)?;
    let entry = static_entry_name(&project)?;
    authenticate_static_entry(&checked, &entry, project.content.is_some())?;
    let packages = package_records(&checked)?;
    let (arguments, content_inputs) = static_content_arguments(&project)?;
    let evaluation = witchy_lower::codegen::checked_glamour_static_evaluation_module(&checked)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let (pages, actions, styles, preloads, assets, islands) = static_site_from_value(
        witchy_interp::interpreter::evaluate_compiler_module(&evaluation, &entry, arguments)
        .map_err(|error| WebCommandError::failure(error.message))?,
    )?;
    let grant = load_web_ui_grant(&project, !islands.is_empty())?;
    let mut island_metadata = witchy_lower::codegen::checked_glamour_islands(&checked)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    bind_interactive_metadata(&islands, &mut island_metadata)?;
    authenticate_static_islands(&islands, &island_metadata)?;
    let island_plans = compile_static_island_plans(&checked, &pages, &islands, &island_metadata)?;
    let island_publication =
        prepare_static_island_publication(
            &checked,
            &pages,
            &actions,
            &islands,
            &island_plans,
            &island_metadata,
            grant.as_ref(),
        )?;
    if pages.is_empty() {
        return Err(WebCommandError::failure(
            "static `web()` must declare at least one page",
        ));
    }
    Ok(CheckedStaticSite {
        project,
        pages,
        actions,
        styles,
        preloads,
        assets,
        islands,
        island_plans,
        island_publication,
        content_inputs,
        packages,
    })
}

fn bind_interactive_metadata(
    islands: &[StaticIsland],
    metadata: &mut Vec<witchy_lower::codegen::GlamourIslandMetadata>,
) -> Result<(), WebCommandError> {
    let interactive = islands
        .iter()
        .filter(|island| island.source_identity.starts_with("interactive-origin1-"))
        .collect::<Vec<_>>();
    let mut candidates = BTreeMap::new();
    for candidate in metadata
        .iter()
        .filter(|candidate| candidate.key.starts_with("interactive-candidate-"))
    {
        if candidates
            .insert(candidate.source_identity.clone(), candidate.clone())
            .is_some()
        {
            return Err(WebCommandError::failure(format!(
                "compiler authenticated interactive origin `{}` more than once",
                candidate.source_identity,
            )));
        }
    }
    let mut bound = Vec::with_capacity(interactive.len());
    for island in interactive {
        let Some(mut candidate) = candidates.get(&island.source_identity).cloned() else {
            return Err(WebCommandError::failure(format!(
                "interactive placement `{}` has no compiler-authenticated constructor origin",
                island.key,
            )));
        };
        candidate.key.clone_from(&island.key);
        candidate.mode.clone_from(&island.mode);
        candidate.activation.clone_from(&island.activation);
        candidate.media.clone_from(&island.media);
        candidate.prefetch.clone_from(&island.prefetch);
        candidate.prefetch_media.clone_from(&island.prefetch_media);
        candidate.diagnostic_name.clone_from(&island.diagnostic_name);
        bound.push(candidate);
    }
    metadata.retain(|candidate| !candidate.key.starts_with("interactive-candidate-"));
    metadata.extend(bound);
    metadata.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(())
}

fn authenticate_static_islands(
    islands: &[StaticIsland],
    metadata: &[witchy_lower::codegen::GlamourIslandMetadata],
) -> Result<(), WebCommandError> {
    if islands.len() != metadata.len() {
        return Err(WebCommandError::failure(format!(
            "static Site evaluated {} interactive plan(s), but the compiler authenticated {}; every plan must come from a direct checked `glamour.island`, `glamour.interactive`, or `glamour.client_region` declaration",
            islands.len(),
            metadata.len(),
        )));
    }
    for island in islands {
        let Some(checked) = metadata.iter().find(|candidate| candidate.key == island.key) else {
            return Err(WebCommandError::failure(format!(
                "static island `{}` has no compiler-authenticated declaration",
                island.key
            )));
        };
        if checked.mode != island.mode
            || checked.activation != island.activation
            || checked.media != island.media
            || checked.prefetch != island.prefetch
            || checked.prefetch_media != island.prefetch_media
            || checked.diagnostic_name != island.diagnostic_name
        {
            return Err(WebCommandError::failure(format!(
                "static island `{}` evaluated different delivery controls than its checked declaration",
                island.key
            )));
        }
    }
    Ok(())
}

fn compile_static_island_plans(
    module: &witchy_types::pipeline::CheckedModule,
    pages: &[StaticPage],
    islands: &[StaticIsland],
    metadata: &[witchy_lower::codegen::GlamourIslandMetadata],
) -> Result<Vec<StaticIslandCompiledPlan>, WebCommandError> {
    let mut plans = Vec::with_capacity(islands.len());
    for island in islands {
        let checked = metadata
            .iter()
            .find(|candidate| candidate.key == island.key)
            .expect("authenticated islands have matching compiler metadata");
        let live_island = if checked.mode == "fresh" {
            Some(evaluate_fresh_island(module, pages, island, checked)?)
        } else {
            None
        };
        let compiled_island = live_island.as_ref().unwrap_or(island);
        let mut plan = StaticIslandCompiledPlan {
            key: island.key.clone(),
            mode: checked.mode.clone(),
            artifact: checked.identity.clone(),
            wire_id: checked.wire_id,
            registry_id: checked.registry_id,
            program_name: checked.program_name.clone(),
            auth_type: witchy_syntax::format::type_str(&checked.auth_type),
            model_type: witchy_syntax::format::type_str(&checked.model_type),
            model_type_name: island_model_type_name(&checked.model_type),
            message_type: witchy_syntax::format::type_str(&checked.message_type),
            html: String::new(),
            nodes: Vec::new(),
            attributes: Vec::new(),
            regions: Vec::new(),
            text_nodes: Vec::new(),
            events: Vec::new(),
            frames: Vec::new(),
            effect_descriptors: checked
                .work
                .iter()
                .filter(|work| work.channel == "effect")
                .map(static_island_work_descriptor)
                .chain(
                    checked
                        .mapped_work
                        .iter()
                        .filter(|work| work.channel == "effect")
                        .map(static_island_mapped_work_descriptor),
                )
                .collect(),
            subscription_descriptors: checked
                .work
                .iter()
                .filter(|work| work.channel == "subscription")
                .map(static_island_work_descriptor)
                .chain(
                    checked
                        .mapped_work
                        .iter()
                        .filter(|work| work.channel == "subscription")
                        .map(static_island_mapped_work_descriptor),
                )
                .collect(),
            fresh: None,
        };
        let mut node_ids = BTreeMap::new();
        let mut event_plan_ids = BTreeMap::new();
        let mut event_class_ids = BTreeMap::new();
        let mut sink_ids = BTreeMap::new();
        let mut region_ids = BTreeMap::new();
        let mut key_ids = BTreeMap::new();
        let mut template_ids = BTreeMap::new();
        let mut slot_ids = BTreeMap::new();
        let mut event_pairs = BTreeSet::new();
        compile_static_island_node(
            compiled_island,
            checked,
            &compiled_island.resume,
            &compiled_island.template,
            &[0],
            &[0],
            true,
            "root",
            None,
            &mut plan,
            &mut node_ids,
            &mut event_plan_ids,
            &mut event_class_ids,
            &mut sink_ids,
            &mut region_ids,
            &mut key_ids,
            &mut template_ids,
            &mut slot_ids,
            &mut event_pairs,
        )?;
        if checked.mode == "fresh" {
            let material = format!(
                "witchy.glamour.island-fresh-root-template.v1|artifact={}",
                checked.identity,
            );
            let template = checked_wire_id(
                &island.key,
                "fresh root template",
                &material,
                &mut template_ids,
            )?;
            let slots = compile_static_island_template_slots(
                compiled_island,
                checked,
                template,
                &[0],
                &plan,
                &mut slot_ids,
            )?;
            let root = compile_static_island_live_template_root(
                &island.key,
                &compiled_island.resume,
                &compiled_island.template,
                &[0],
                &plan,
            )?;
            let events = json!(plan
                .events
                .iter()
                .filter(|event| plan.nodes.iter().any(|node| node.id == event.node && node.live))
                .map(|event| json!({
                    "node": event.node,
                    "eventClass": event.event_class,
                    "eventPlan": event.plan,
                }))
                .collect::<Vec<_>>());
            let page = pages
                .iter()
                .find(|page| page.island_keys.iter().any(|key| key == &island.key))
                .expect("authenticated fresh island has one page");
            plan.fresh = Some(StaticIslandFreshPlan {
                route: page.route.clone(),
                bootstrap: String::new(),
                template,
                instance: checked.registry_id,
                slots,
                root,
                regions: compile_static_island_live_template_regions(&plan, &[0]),
                events,
            });
        }
        plan.html = annotate_static_island_html(compiled_island, &plan)?;
        validate_static_fragment(&compiled_island.key, &plan.html)?;
        plans.push(plan);
    }
    plans.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(plans)
}

fn static_island_work_descriptor(
    work: &witchy_lower::codegen::GlamourWorkMetadata,
) -> StaticIslandWorkDescriptor {
    StaticIslandWorkDescriptor {
        id: work.descriptor_id,
        handler: work.handler.clone(),
        result_schema: work.result_schema_id,
        completion_id: work.completion_id,
        owner_scope: work.owner_scope_id,
        semantic: work.kind.clone(),
        policy: static_island_work_policy(&work.browser_policy),
        completion_source: work.completion_source.clone(),
        completion_captures: work
            .completion_captures
            .iter()
            .map(|capture| capture.name.clone())
            .collect(),
    }
}

fn static_island_mapped_work_descriptor(
    work: &witchy_lower::codegen::GlamourMappedWorkMetadata,
) -> StaticIslandWorkDescriptor {
    StaticIslandWorkDescriptor {
        id: work.descriptor_id,
        handler: work.handler.clone(),
        result_schema: work.result_schema_id,
        completion_id: work.completion_id,
        owner_scope: work.owner_scope_id,
        semantic: work.kind.clone(),
        policy: static_island_work_policy(&work.browser_policy),
        completion_source: format!("mapped({})", work.mapper_source),
        completion_captures: work
            .mapper_captures
            .iter()
            .map(|capture| capture.name.clone())
            .collect(),
    }
}

fn static_island_work_policy(
    policy: &witchy_lower::codegen::GlamourBrowserPolicyMetadata,
) -> Value {
    use witchy_lower::codegen::GlamourBrowserPolicyMetadata as Policy;

    match policy {
        Policy::Fetch { scope, methods, prefix } => json!({
            "kind": "fetch",
            "scope": scope,
            "methods": methods.split(',').collect::<Vec<_>>(),
            "prefix": prefix,
        }),
        Policy::Navigation { base, rights } => json!({
            "kind": "navigation",
            "base": base,
            "rights": rights,
        }),
        Policy::Timer { minimum } => json!({
            "kind": "timer",
            "minimum": minimum,
        }),
        Policy::Storage { provider, namespace, key_prefix, max_value_bytes } => json!({
            "kind": "storage",
            "provider": provider,
            "namespace": namespace,
            "keyPrefix": key_prefix,
            "maxValueBytes": max_value_bytes,
        }),
        Policy::Worker { name, max_request_bytes, max_result_bytes, max_concurrency, timeout_ms } => json!({
            "kind": "worker",
            "name": name,
            "maxRequestBytes": max_request_bytes,
            "maxResultBytes": max_result_bytes,
            "maxConcurrency": max_concurrency,
            "timeoutMs": timeout_ms,
        }),
        Policy::HostPort { adapter, endpoint, max_request_bytes, max_result_bytes } => json!({
            "kind": "host-port",
            "adapter": adapter,
            "endpoint": endpoint,
            "maxRequestBytes": max_request_bytes,
            "maxResultBytes": max_result_bytes,
        }),
        Policy::Port { name } => json!({
            "kind": "port",
            "name": name,
        }),
        Policy::SecretField { form, field } => json!({
            "kind": "secret-field",
            "form": form,
            "field": field,
        }),
    }
}

fn static_island_event_owner(
    plan: &StaticIslandCompiledPlan,
    event: &StaticIslandEventRecord,
) -> (u32, u32) {
    let mut owner = (0_usize, plan.registry_id, plan.registry_id);
    for region in &plan.regions {
        match region.kind {
            StaticIslandRegionKind::List => {
                for key in &region.keys {
                    if key.nodes.contains(&event.node) && region.path.len() >= owner.0 {
                        owner = (region.path.len() + 1, region.id, key.id);
                    }
                }
            }
            StaticIslandRegionKind::Branch | StaticIslandRegionKind::Child => {
                if region
                    .child
                    .as_ref()
                    .is_some_and(|child| child.nodes.contains(&event.node))
                    && region.path.len() >= owner.0
                {
                    owner = (region.path.len() + 1, region.id, region.id);
                }
            }
        }
    }
    (owner.1, owner.2)
}

fn insert_static_island_owner_instance(
    plan: &StaticIslandCompiledPlan,
    owners: &mut BTreeMap<String, Value>,
    instance: u32,
    declaration: u32,
    kind: &str,
) -> Result<(), WebCommandError> {
    if owners
        .insert(
            instance.to_string(),
            json!({
                "declaration": declaration,
                "kind": kind,
            }),
        )
        .is_some()
    {
        return Err(WebCommandError::failure(format!(
            "static island `{}` has an owner-instance identity collision at {instance}",
            plan.key,
        )));
    }
    Ok(())
}

fn static_island_owner_instances(
    plan: &StaticIslandCompiledPlan,
) -> Result<BTreeMap<String, Value>, WebCommandError> {
    let mut owners = BTreeMap::new();
    insert_static_island_owner_instance(
        plan,
        &mut owners,
        plan.registry_id,
        plan.registry_id,
        "root",
    )?;
    for region in &plan.regions {
        match region.kind {
            StaticIslandRegionKind::List => {
                for key in &region.keys {
                    insert_static_island_owner_instance(
                        plan,
                        &mut owners,
                        key.id,
                        region.id,
                        "key",
                    )?;
                }
            }
            StaticIslandRegionKind::Branch => {
                insert_static_island_owner_instance(
                    plan,
                    &mut owners,
                    region.id,
                    region.id,
                    "branch",
                )?;
            }
            StaticIslandRegionKind::Child => {
                insert_static_island_owner_instance(
                    plan,
                    &mut owners,
                    region.id,
                    region.id,
                    "child",
                )?;
            }
        }
    }
    Ok(owners)
}

fn evaluate_fresh_island(
    module: &witchy_types::pipeline::CheckedModule,
    pages: &[StaticPage],
    fallback: &StaticIsland,
    metadata: &witchy_lower::codegen::GlamourIslandMetadata,
) -> Result<StaticIsland, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let page = pages
        .iter()
        .find(|page| page.island_keys.iter().any(|key| key == &fallback.key))
        .expect("authenticated fresh island has one page");
    let evaluation = witchy_lower::codegen::checked_glamour_static_evaluation_module(module)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let start = CompilerValue::Constructor {
        name: "glamour.Start".into(),
        fields: vec![
            CompilerValue::String(page.route.clone()),
            CompilerValue::String(String::new()),
        ],
    };
    let model = witchy_interp::interpreter::evaluate_compiler_module(
        &evaluation,
        &metadata.initial_name,
        vec![start],
    )
    .map_err(|error| {
        WebCommandError::failure(format!(
            "fresh client region `{}` could not derive its capability-free model: {}",
            fallback.key, error.message,
        ))
    })?;
    let rendered = witchy_interp::interpreter::evaluate_compiler_module(
        &evaluation,
        &metadata.view_name,
        vec![model],
    )
    .map_err(|error| {
        WebCommandError::failure(format!(
            "fresh client region `{}` could not derive its capability-free live view: {}",
            fallback.key, error.message,
        ))
    })?;
    let value = witchy_interp::interpreter::evaluate_compiler_module(
        &evaluation,
        "glamour.client_region_live_graph",
        vec![CompilerValue::String(fallback.key.clone()), rendered],
    )
    .map_err(|error| {
        WebCommandError::failure(format!(
            "fresh client region `{}` could not authenticate its live graph: {}",
            fallback.key, error.message,
        ))
    })?;
    let CompilerValue::Tuple(fields) = value else {
        return Err(WebCommandError::failure(format!(
            "fresh client region `{}` produced no authenticated live markup",
            fallback.key,
        )));
    };
    if fields.len() != 4 {
        return Err(WebCommandError::failure(format!(
            "fresh client region `{}` produced malformed live markup",
            fallback.key,
        )));
    }
    let mut fields = fields.into_iter();
    let Some(CompilerValue::String(key)) = fields.next() else {
        return Err(WebCommandError::failure("fresh client-region live key is not text"));
    };
    let Some(CompilerValue::String(html)) = fields.next() else {
        return Err(WebCommandError::failure("fresh client-region live HTML is not text"));
    };
    let Some(CompilerValue::String(resume_json)) = fields.next() else {
        return Err(WebCommandError::failure("fresh client-region live graph is not text"));
    };
    let Some(CompilerValue::String(template_json)) = fields.next() else {
        return Err(WebCommandError::failure("fresh client-region live template is not text"));
    };
    if key != fallback.key {
        return Err(WebCommandError::failure(format!(
            "fresh client region `{}` changed its compiler-owned live key",
            fallback.key,
        )));
    }
    validate_static_fragment(&key, &html)?;
    let resume = static_island_resume(&key, &resume_json)?;
    let template = static_island_template(&key, &template_json)?;
    validate_static_island_template(&key, &resume, &template)?;
    let mut live = fallback.clone();
    live.html = html;
    live.resume = resume;
    live.template = template;
    Ok(live)
}

fn annotate_static_island_html(
    island: &StaticIsland,
    plan: &StaticIslandCompiledPlan,
) -> Result<String, WebCommandError> {
    let mut by_node: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for event in plan.events.iter().filter(|event| {
        event.name != "glamour-frame"
            && plan.nodes.iter().any(|node| node.id == event.node && node.live)
    }) {
        by_node.entry(event.node).or_default().push(&event.id);
    }
    let mut html = island.html.clone();
    for frame in &plan.frames {
        let event = plan
            .events
            .iter()
            .find(|event| event.plan == frame.event_plan)
            .expect("frame has an authenticated event plan");
        let marker = format!(
            " data-glamour-frame-renderer=\"{}\" data-glamour-frame-event=\"{}\"",
            frame.renderer, event.id,
        );
        if html.matches(&marker).count() != 1 {
            return Err(WebCommandError::failure(format!(
                "static island `{}` frame marker does not match its checked render graph",
                island.key
            )));
        }
        html = html.replacen(
            &marker,
            &format!("{marker} data-glamour-node=\"{}\"", frame.node),
            1,
        );
    }
    for (node, event_ids) in by_node {
        let marker = format!(" data-glamour-events=\"{}\"", event_ids.join(" "));
        if html.matches(&marker).count() != 1 {
            return Err(WebCommandError::failure(format!(
                "static island `{}` event marker does not match its checked render graph",
                island.key
            )));
        }
        html = html.replacen(
            &marker,
            &format!(" data-glamour-node=\"{node}\""),
            1,
        );
    }
    if html.contains("data-glamour-events=") {
        return Err(WebCommandError::failure(format!(
            "static island `{}` contains an unauthenticated event marker",
            island.key
        )));
    }
    Ok(html)
}

fn static_actions_in_html(html: &str, actions: &[StaticAction]) -> Vec<StaticAction> {
    actions
        .iter()
        .filter(|action| {
            html.contains(&format!(
                "data-glamour-form=\"{}\"",
                action.id,
            ))
        })
        .cloned()
        .collect()
}

fn static_page_actions(page: &StaticPage, actions: &[StaticAction]) -> Vec<StaticAction> {
    static_actions_in_html(&page.html, actions)
}

fn static_control_projection(actions: &[StaticAction]) -> Value {
    json!({
        "schema": "witchy.glamour.static-controls.v1",
        "actions": actions,
    })
}

fn static_island_browser_policy(
    plan: &StaticIslandCompiledPlan,
    actions: &[StaticAction],
) -> Value {
    let mut fetch = BTreeMap::new();
    let mut navigation = BTreeMap::new();
    let mut timers = BTreeMap::new();
    let mut ports = BTreeMap::new();
    let mut storage = BTreeMap::new();
    let mut workers = BTreeMap::new();
    let mut frames = BTreeMap::new();
    let mut secret_fields = BTreeMap::<String, Value>::new();
    for action in actions {
        for field in action.fields.iter().filter(|field| field.kind == "secret") {
            secret_fields.insert(
                format!("{}\0{}", action.id, field.name),
                json!({"form": action.id, "field": field.name}),
            );
        }
    }
    for frame in &plan.frames {
        let entry = json!({
            "renderer": frame.renderer,
            "maxGrantBytes": frame.max_grant_bytes,
            "maxEventBytes": frame.max_event_bytes,
            "artifact": frame.artifact,
            "url": frame.url,
        });
        frames.insert(
            format!(
                "{}\0{}\0{}\0{}",
                frame.renderer, frame.max_grant_bytes, frame.max_event_bytes, frame.artifact,
            ),
            entry,
        );
    }
    for descriptor in plan
        .effect_descriptors
        .iter()
        .chain(&plan.subscription_descriptors)
    {
        let policy = &descriptor.policy;
        let entry = match policy["kind"].as_str() {
            Some("fetch") => json!({
                "scope": policy["scope"],
                "methods": policy["methods"],
                "prefix": policy["prefix"],
            }),
            Some("navigation") => json!({
                "base": policy["base"],
                "rights": policy["rights"],
            }),
            Some("timer") => json!({"minimum": policy["minimum"]}),
            Some("port") => json!(policy["name"]),
            Some("storage") => json!({
                "provider": policy["provider"],
                "namespace": policy["namespace"],
                "keyPrefix": policy["keyPrefix"],
                "maxValueBytes": policy["maxValueBytes"],
            }),
            Some("worker") => json!({
                "name": policy["name"],
                "maxRequestBytes": policy["maxRequestBytes"],
                "maxResultBytes": policy["maxResultBytes"],
                "maxConcurrency": policy["maxConcurrency"],
                "timeoutMs": policy["timeoutMs"],
                "artifact": policy["artifact"],
                "url": policy["url"],
                "export": policy["export"],
            }),
            Some("host-port") => json!(policy["adapter"]),
            _ => continue,
        };
        match policy["kind"].as_str() {
            Some("fetch") => {
                let key = format!(
                    "{}\0{}\0{}",
                    policy["scope"].as_str().unwrap_or_default(),
                    policy["methods"].as_array().map(|methods| methods.iter()
                        .filter_map(Value::as_str).collect::<Vec<_>>().join(","))
                        .unwrap_or_default(),
                    policy["prefix"].as_str().unwrap_or_default(),
                );
                fetch.insert(key, entry);
            }
            Some("navigation") => {
                let key = format!(
                    "{}\0{}",
                    policy["base"].as_str().unwrap_or_default(),
                    policy["rights"].as_str().unwrap_or_default(),
                );
                navigation.insert(key, entry);
            }
            Some("timer") => {
                let key = format!("{:020}", policy["minimum"].as_i64().unwrap_or_default());
                timers.insert(key, entry);
            }
            Some("port") => {
                ports.insert(policy["name"].as_str().unwrap_or_default().to_string(), entry);
            }
            Some("host-port") => {
                ports.insert(policy["adapter"].as_str().unwrap_or_default().to_string(), entry);
            }
            Some("storage") => {
                let key = format!(
                    "{}\0{}\0{}\0{:020}",
                    policy["provider"].as_str().unwrap_or_default(),
                    policy["namespace"].as_str().unwrap_or_default(),
                    policy["keyPrefix"].as_str().unwrap_or_default(),
                    policy["maxValueBytes"].as_i64().unwrap_or_default(),
                );
                storage.insert(key, entry);
            }
            Some("worker") => {
                let key = format!(
                    "{}\0{}\0{}\0{}\0{}\0{}",
                    policy["name"].as_str().unwrap_or_default(),
                    policy["maxRequestBytes"].as_i64().unwrap_or_default(),
                    policy["maxResultBytes"].as_i64().unwrap_or_default(),
                    policy["maxConcurrency"].as_i64().unwrap_or_default(),
                    policy["timeoutMs"].as_i64().unwrap_or_default(),
                    policy["artifact"].as_str().unwrap_or_default(),
                );
                workers.insert(key, entry);
            }
            _ => {}
        }
    }
    json!({
        "schema": "witchy.glamour.browser-policy.v1",
        "fetch": fetch.into_values().collect::<Vec<_>>(),
        "navigation": navigation.into_values().collect::<Vec<_>>(),
        "timers": timers.into_values().collect::<Vec<_>>(),
        "ports": ports.into_values().collect::<Vec<_>>(),
        "secretFields": secret_fields.into_values().collect::<Vec<_>>(),
        "frames": frames.into_values().collect::<Vec<_>>(),
        "workers": workers.into_values().collect::<Vec<_>>(),
        "storage": storage.into_values().collect::<Vec<_>>(),
    })
}

fn validate_static_island_browser_policy(
    plan: &StaticIslandCompiledPlan,
    actions: &[StaticAction],
    policy: &Value,
) -> Result<(), WebCommandError> {
    for descriptor in &plan.effect_descriptors {
        let permitted = matches!(
            (descriptor.semantic.as_str(), descriptor.policy["kind"].as_str()),
            ("timer", Some("timer"))
            | ("http", Some("fetch"))
            | ("navigation", Some("navigation"))
            | ("port" | "secret", Some("port"))
            | ("host-port", Some("host-port"))
            | ("storage-get" | "storage-set" | "storage-remove", Some("storage"))
            | ("worker", Some("worker"))
        );
        if !permitted {
            return Err(WebCommandError::failure(format!(
                "Glamour island `{}` effect descriptor {} has no compiler-authenticated browser policy",
                plan.key, descriptor.id,
            )));
        }
    }
    for descriptor in &plan.subscription_descriptors {
        if descriptor.semantic != "interval" || descriptor.policy["kind"] != "timer" {
            return Err(WebCommandError::failure(format!(
                "Glamour island `{}` subscription descriptor {} has no compiler-authenticated browser policy",
                plan.key, descriptor.id,
            )));
        }
    }
    if policy["schema"] != "witchy.glamour.browser-policy.v1" {
        return Err(WebCommandError::failure(format!(
            "Glamour island `{}` has an invalid compiler-authenticated browser policy projection",
            plan.key,
        )));
    }
    let expected_secret_fields = actions
        .iter()
        .flat_map(|action| {
            action.fields.iter().filter(|field| field.kind == "secret").map(|field| (
                format!("{}\0{}", action.id, field.name),
                json!({"form": action.id, "field": field.name}),
            ))
        })
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    if policy["secretFields"] != json!(expected_secret_fields) {
        return Err(WebCommandError::failure(format!(
            "Glamour island `{}` static secret-control policy differs from its checked route actions",
            plan.key,
        )));
    }
    Ok(())
}

fn prepare_static_island_publication(
    checked: &witchy_types::pipeline::CheckedModule,
    pages: &[StaticPage],
    actions: &[StaticAction],
    islands: &[StaticIsland],
    plans: &[StaticIslandCompiledPlan],
    metadata: &[witchy_lower::codegen::GlamourIslandMetadata],
    grant: Option<&WebUiGrant>,
) -> Result<Option<StaticIslandPublication>, WebCommandError> {
    if islands.is_empty() {
        return Ok(None);
    }
    let grant = grant.expect("runnable static output has a checked UiRoot grant");
    let identity_input = serde_json::to_vec(&json!({
        "schema": "witchy.glamour.island-publication-input.v1",
        "routes": pages.iter().map(|page| &page.route).collect::<Vec<_>>(),
        "actions": actions,
        "islands": islands,
        "plans": plans,
        "mountGrant": grant,
    }))
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let build_identity = sha256(&identity_input);
    let mut manifest_islands = Vec::with_capacity(islands.len());
    let mut artifact_records = Vec::with_capacity(islands.len());
    let mut artifact_record_identities = BTreeMap::<String, usize>::new();
    let mut artifacts: Vec<StaticIslandArtifact> = Vec::with_capacity(islands.len());
    let mut artifact_files: BTreeMap<String, usize> = BTreeMap::new();
    let mut compiled_artifacts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut workers: Vec<StaticWorkerArtifact> = Vec::new();
    let mut worker_files: BTreeMap<String, usize> = BTreeMap::new();
    let mut compiled_workers: BTreeMap<(String, u32), Vec<u8>> = BTreeMap::new();
    let frames = if plans.iter().any(|plan| !plan.frames.is_empty()) {
        let html = static_frame_document();
        vec![StaticFrameArtifact {
            identity: format!("glamour-frame1-{}", sha256(&html)),
            file: content_name("frame", "html", &html),
            html,
        }]
    } else {
        Vec::new()
    };
    let mut rewritten_pages = pages
        .iter()
        .map(|page| (page.route.clone(), page.html.clone()))
        .collect::<BTreeMap<_, _>>();
    for island in islands {
        let plan = plans
            .iter()
            .find(|candidate| candidate.key == island.key)
            .expect("every checked island has a compiled plan");
        let compiler_metadata = metadata
            .iter()
            .find(|candidate| candidate.key == island.key)
            .expect("every checked island has compiler metadata");
        let mut granted_plan = plan.clone();
        let mut worker_bindings = BTreeMap::<u32, Value>::new();
        for work in compiler_metadata.work.iter().filter(|work| work.kind == "worker") {
            let worker_key = (compiler_metadata.identity.clone(), work.descriptor_id);
            let wasm = if let Some(wasm) = compiled_workers.get(&worker_key) {
                wasm.clone()
            } else {
                let wasm = compile_static_worker_artifact(checked, compiler_metadata, work.descriptor_id)?;
                compiled_workers.insert(worker_key, wasm.clone());
                wasm
            };
            let file = content_name("worker", "wasm", &wasm);
            let identity = format!("glamour-worker1-{}", sha256(&wasm));
            if let Some(existing) = worker_files.get(&file).copied() {
                if workers[existing].wasm != wasm {
                    return Err(WebCommandError::failure(format!(
                        "Glamour worker descriptor {} collides at executable file `{file}`",
                        work.descriptor_id,
                    )));
                }
            } else {
                worker_files.insert(file.clone(), workers.len());
                workers.push(StaticWorkerArtifact {
                    identity: identity.clone(),
                    file: file.clone(),
                    wasm,
                });
            }
            worker_bindings.insert(work.descriptor_id, json!({
                "artifact": identity,
                "url": format!("/assets/{file}"),
                "export": "__export_export_glamour_worker_execute",
            }));
        }
        let mut unresolved = compiler_metadata
            .mapped_work
            .iter()
            .filter(|work| work.kind == "worker")
            .collect::<Vec<_>>();
        while !unresolved.is_empty() {
            let before = unresolved.len();
            unresolved.retain(|work| {
                if let Some(binding) = worker_bindings.get(&work.previous_descriptor_id).cloned() {
                    worker_bindings.insert(work.descriptor_id, binding);
                    false
                } else {
                    true
                }
            });
            if unresolved.len() == before {
                return Err(WebCommandError::failure(format!(
                    "Glamour island `{}` has a mapped worker descriptor with no authenticated task root",
                    plan.key,
                )));
            }
        }
        for descriptor in &mut granted_plan.effect_descriptors {
            if let Some(binding) = worker_bindings.get(&descriptor.id) {
                let object = descriptor.policy.as_object_mut().ok_or_else(|| {
                    WebCommandError::failure("worker descriptor policy is not an object")
                })?;
                object.insert("artifact".into(), binding["artifact"].clone());
                object.insert("url".into(), binding["url"].clone());
                object.insert("export".into(), binding["export"].clone());
            }
        }
        if let Some(frame_artifact) = frames.first() {
            for frame in &mut granted_plan.frames {
                frame.artifact.clone_from(&frame_artifact.identity);
                frame.url = format!("/assets/{}", frame_artifact.file);
            }
        }
        let plan = &granted_plan;
        let page = pages
            .iter()
            .find(|page| page.island_keys.iter().any(|key| key == &island.key))
            .expect("authenticated island placement has one page");
        let page_actions = static_actions_in_html(&island.html, actions);
        let browser_policy = static_island_browser_policy(plan, &page_actions);
        validate_static_island_browser_policy(plan, &page_actions, &browser_policy)?;
        let descriptor_projection = |descriptor: &StaticIslandWorkDescriptor| json!({
            "semantic": descriptor.semantic,
            "policy": descriptor.policy,
        });
        let grant_projection = json!({
            "schema": "witchy.glamour.artifact-grant.v1",
            "projectGrantDigest": grant.digest,
            "effects": plan.effect_descriptors.iter().map(|descriptor| (
                descriptor.id.to_string(),
                descriptor_projection(descriptor),
            )).collect::<BTreeMap<_, _>>(),
            "subscriptions": plan.subscription_descriptors.iter().map(|descriptor| (
                descriptor.id.to_string(),
                descriptor_projection(descriptor),
            )).collect::<BTreeMap<_, _>>(),
            "staticControls": static_control_projection(&page_actions),
            "browserPolicy": browser_policy,
        });
        let artifact_identity = format!(
            "glamour-island1-{}",
            sha256(
                format!(
                    "witchy.glamour.granted-island.v1|base={}|grant={}|projection={}",
                    plan.artifact,
                    grant.digest,
                    sha256(&serde_json::to_vec(&grant_projection)
                        .map_err(|error| WebCommandError::failure(error.to_string()))?),
                )
                .as_bytes(),
            ),
        );
        let instance_digest = sha256(
            format!(
                "witchy.glamour.island-instance.v1|build={build_identity}|route={}|key={}|artifact={}",
                page.route, island.key, artifact_identity,
            )
            .as_bytes(),
        );
        let instance = format!("glamour-instance1-{instance_digest}");
        let instance_grant_input = json!({
            "schema": "witchy.glamour.instance-grant.v1",
            "instance": instance,
            "projectGrantDigest": grant.digest,
            "artifactGrant": grant_projection,
        });
        let instance_grant_digest = sha256(
            &serde_json::to_vec(&instance_grant_input)
                .map_err(|error| WebCommandError::failure(error.to_string()))?,
        );
        // Placement keys and source spans are intentionally excluded: the executable
        // is determined by the authenticated program shape, not by where an identical
        // island was placed in the generated book.
        let mut artifact_plan = plan.clone();
        artifact_plan.key.clear();
        artifact_plan.artifact.clear();
        artifact_plan.html.clear();
        let artifact_key = format!(
            "{:?}|mode={}|wire={}|registry={}|program={}|auth={}|model={}|message={}|work={:?}|mapped={:?}|maps={:?}",
            artifact_plan,
            compiler_metadata.mode,
            compiler_metadata.wire_id,
            compiler_metadata.registry_id,
            compiler_metadata.program_name,
            witchy_syntax::format::type_str(&compiler_metadata.auth_type),
            witchy_syntax::format::type_str(&compiler_metadata.model_type),
            witchy_syntax::format::type_str(&compiler_metadata.message_type),
            compiler_metadata.work,
            compiler_metadata.mapped_work,
            compiler_metadata.work_maps,
        );
        let base_wasm = if let Some(wasm) = compiled_artifacts.get(&artifact_key) {
            wasm.clone()
        } else {
            let wasm = compile_static_island_artifact(
                checked,
                plan,
                compiler_metadata,
                &build_identity,
            )?;
            compiled_artifacts.insert(artifact_key, wasm.clone());
            wasm
        };
        let wasm = embed_web_mount_grant(
            base_wasm,
            grant,
            Some(&artifact_identity),
            Some(&grant_projection),
        )?;
        let file = content_name("island", "wasm", &wasm);
        if let Some(existing) = artifact_files.get(&file).copied() {
            if artifacts[existing].wasm != wasm {
                return Err(WebCommandError::failure(format!(
                    "Glamour island `{}` collides at executable file `{file}`",
                    island.key
                )));
            }
        } else {
            artifact_files.insert(file.clone(), artifacts.len());
            artifacts.push(StaticIslandArtifact {
                identity: artifact_identity.clone(),
                file: file.clone(),
                wasm,
            });
        }
        let events = if island.mode == "fresh" {
            Vec::new()
        } else {
            plan.events
                .iter()
                .map(|event| {
                    json!({
                        "name": event.name,
                        "node": event.node,
                        "plan": event.plan,
                        "preventDefault": event.prevent_default,
                        "stopPropagation": event.stop_propagation,
                        "readValue": event.read_value,
                        "readChecked": event.read_checked,
                        "readKey": event.read_key,
                        "fallback": event.fallback,
                    })
                })
                .collect::<Vec<_>>()
        };
        manifest_islands.push(json!({
            "id": instance,
            "artifact": artifact_identity.clone(),
            "key": island.key,
            "mode": island.mode,
            "activation": island.activation,
            "media": island.media,
            "prefetch": island.prefetch,
            "prefetchMedia": island.prefetch_media,
            "name": island.diagnostic_name,
            "state": island.state,
            "events": events,
            "grantDigest": instance_grant_digest,
        }));
        let event_classes = plan
            .events
            .iter()
            .map(|event| (event.event_class, event.name.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut templates = plan
            .regions
            .iter()
            .flat_map(|region| {
                let keyed = region.keys.iter().filter(|key| key.template != 0).map(|key| {
                    json!({
                        "id": key.template,
                        "root": key.template_root,
                        "slots": key.slots,
                        "regions": key.template_regions,
                        "events": key.template_events,
                    })
                });
                let child = region.child.iter().filter(|child| child.template != 0).map(|child| {
                    json!({
                        "id": child.template,
                        "root": child.template_root,
                        "slots": child.slots,
                        "regions": child.template_regions,
                        "events": child.template_events,
                    })
                });
                keyed.chain(child)
            })
            .collect::<Vec<_>>();
        if let Some(fresh) = &plan.fresh {
            templates.push(json!({
                "id": fresh.template,
                "root": fresh.root,
                "slots": fresh.slots,
                "regions": fresh.regions,
                "events": fresh.events,
            }));
        }
        let resume = json!({
            "version": 1,
            "sequence": 1,
            "inputSequence": 0,
            "nodes": plan.nodes.iter().filter(|node| node.live).map(|node| json!({
                "id": node.id,
                "path": node.dom_path,
            })).collect::<Vec<_>>(),
            "regions": plan.regions.iter().filter(|region| region.live).map(|region| json!({
                "id": region.id,
                "parent": region.parent,
                "before": region.before,
                "keys": region.keys.iter().map(|key| json!({
                    "key": key.id,
                    "source": key.source,
                    "root": key.root,
                    "nodes": key.nodes,
                })).collect::<Vec<_>>(),
                "child": region.child.as_ref().filter(|child| child.mounted).map(|child| json!({
                    "root": child.root,
                    "nodes": child.nodes,
                })),
            })).collect::<Vec<_>>(),
            "events": plan.events.iter().filter(|event| {
                plan.nodes.iter().any(|node| node.id == event.node && node.live)
            }).map(|event| json!({
                "node": event.node,
                "eventClass": event.event_class,
                "eventPlan": event.plan,
            })).collect::<Vec<_>>(),
            "subscriptions": [],
        });
        let artifact_record = json!({
            "artifact": artifact_identity.clone(),
            "wireId": plan.wire_id,
            "registryId": plan.registry_id,
            "buildIdentity": build_identity,
            "grantDigest": grant.digest,
            "grantProjection": grant_projection,
            "browserPolicy": browser_policy,
            "actions": page_actions,
            "appId": plan.wire_id,
            "buildId": format!("0x{}", &build_identity[..16]),
            "features": {
                "mode": "production",
                "startupBarrier": true,
            },
            "limits": {
                "maxCompletionBytes": 60 * 1024,
                "maxCaptureBytes": 64 * 1024,
                "maxCaptureDepth": 32,
                "maxPendingEffects": 128,
                "maxSubscriptions": 64,
                "maxPrivateCallbackBytes": 1024 * 1024,
            },
            "url": format!("/assets/{file}"),
            "moduleGroup": file,
            "programTypes": {
                "auth": plan.auth_type,
                "model": plan.model_type,
                "message": plan.message_type,
            },
            "templates": templates,
            "nodes": plan.nodes,
            "regions": plan.regions.iter().map(|region| json!({
                "id": region.id,
                "parent": region.parent,
                "kind": region.kind,
                "nodes": region.child.as_ref().map(|child| child.nodes.clone()).unwrap_or_default(),
                "template": region.child.as_ref().map(|child| child.template).unwrap_or(0),
                "dynamicTemplate": region.dynamic.as_ref().map(|key| key.template).unwrap_or(0),
            })).collect::<Vec<_>>(),
            "attributeBindings": plan.attributes,
            "properties": island_sink_registry(&plan.attributes, "property"),
            "attributes": island_sink_registry(&plan.attributes, "attribute"),
            "aria": island_sink_registry(&plan.attributes, "aria"),
            "customProperties": island_custom_property_registry(&plan.attributes),
            "ownerInstances": static_island_owner_instances(plan)?,
            "eventClasses": event_classes.into_iter().map(|(id, name)| json!({
                "id": id,
                "name": name,
                "capture": false,
            })).collect::<Vec<_>>(),
            "eventPlans": plan.events.iter().map(|event| json!({
                "id": event.plan,
                "node": event.node,
                "eventClass": event.event_class,
                "ownerScope": static_island_event_owner(plan, event).0,
                "instance": static_island_event_owner(plan, event).1,
                "preventDefault": event.prevent_default,
                "stopPropagation": event.stop_propagation,
                "readValue": event.read_value,
                "readChecked": event.read_checked,
                "readKey": event.read_key,
            })).collect::<Vec<_>>(),
            "effectDescriptors": plan.effect_descriptors.iter().map(|descriptor| (
                descriptor.id.to_string(),
                json!({
                    "handler": descriptor.handler,
                    "resultSchema": descriptor.result_schema,
                    "completion": descriptor.completion_id,
                    "ownerScope": descriptor.owner_scope,
                    "semantic": descriptor.semantic,
                    "policy": descriptor.policy,
                }),
            )).collect::<BTreeMap<_, _>>(),
            "subscriptionDescriptors": plan.subscription_descriptors.iter().map(|descriptor| (
                descriptor.id.to_string(),
                json!({
                    "handler": descriptor.handler,
                    "resultSchema": descriptor.result_schema,
                    "completion": descriptor.completion_id,
                    "ownerScope": descriptor.owner_scope,
                    "semantic": descriptor.semantic,
                    "policy": descriptor.policy,
                }),
            )).collect::<BTreeMap<_, _>>(),
            "frames": plan.frames.iter().map(|frame| {
                let nonce = format!("glamour-frame-nonce1-{}", sha256(format!(
                    "witchy.glamour.frame-instance.v1|instance={instance}|node={}|artifact={}",
                    frame.node, frame.artifact,
                ).as_bytes()));
                json!({
                    "node": frame.node,
                    "eventPlan": frame.event_plan,
                    "renderer": frame.renderer,
                    "maxGrantBytes": frame.max_grant_bytes,
                    "maxEventBytes": frame.max_event_bytes,
                    "grant": frame.grant,
                    "artifact": frame.artifact,
                    "url": frame.url,
                    "nonce": nonce,
                })
            }).collect::<Vec<_>>(),
            "fresh": plan.fresh.as_ref().map(|fresh| json!({
                "route": fresh.route,
                "bootstrap": fresh.bootstrap,
                "template": fresh.template,
                "instance": fresh.instance,
            })),
            "resume": resume,
        });
        if let Some(existing) = artifact_record_identities.get(&artifact_identity).copied() {
            if artifact_records[existing] != artifact_record {
                return Err(WebCommandError::failure(format!(
                    "shared Glamour artifact `{artifact_identity}` has inconsistent authenticated metadata",
                )));
            }
        } else {
            artifact_record_identities.insert(artifact_identity, artifact_records.len());
            artifact_records.push(artifact_record);
        }
        let source = format!(
            "<div data-glamour-island-key=\"{}\">{}</div>",
            island.key, island.html
        );
        let mut published_html = if island.mode == "fresh" {
            island.html.clone()
        } else {
            plan.html.clone()
        };
        for frame in &plan.frames {
            let event = plan
                .events
                .iter()
                .find(|event| event.plan == frame.event_plan)
                .expect("frame has an authenticated event plan");
            let nonce = format!("glamour-frame-nonce1-{}", sha256(format!(
                "witchy.glamour.frame-instance.v1|instance={instance}|node={}|artifact={}",
                frame.node, frame.artifact,
            ).as_bytes()));
            let marker = format!(
                " data-glamour-frame-renderer=\"{}\" data-glamour-frame-event=\"{}\"",
                frame.renderer, event.id,
            );
            if published_html.matches(&marker).count() != 1 {
                return Err(WebCommandError::failure(format!(
                    "static island `{}` frame placement differs from its authenticated graph",
                    island.key,
                )));
            }
            published_html = published_html.replacen(
                &marker,
                &format!(
                    "{marker} data-glamour-frame-artifact=\"{}\" data-glamour-frame-nonce=\"{nonce}\" src=\"{}\"",
                    frame.artifact, frame.url,
                ),
                1,
            );
        }
        let replacement = format!(
            "<div data-glamour-island=\"{instance}\" data-glamour-build=\"{build_identity}\">{}</div>",
            published_html
        );
        let html = rewritten_pages
            .get_mut(&page.route)
            .expect("island page exists in publication map");
        if html.matches(&source).count() != 1 {
            return Err(WebCommandError::failure(format!(
                "static island `{}` placement bytes changed after authentication",
                island.key
            )));
        }
        *html = html.replacen(&source, &replacement, 1);
    }
    for (route, html) in &rewritten_pages {
        validate_static_document(route, html)?;
    }
    let mut route_manifests = BTreeMap::new();
    for page in pages.iter().filter(|page| !page.island_keys.is_empty()) {
        let route_islands = manifest_islands
            .iter()
            .filter(|record| {
                record["key"]
                    .as_str()
                    .is_some_and(|key| page.island_keys.iter().any(|candidate| candidate == key))
            })
            .cloned()
            .collect::<Vec<_>>();
        if route_islands.len() != page.island_keys.len() {
            return Err(WebCommandError::failure(format!(
                "static route `{}` does not have one authenticated record per island placement",
                page.route,
            )));
        }
        let manifest = json!({
            "schema": "witchy.glamour.islands.v1",
            "buildIdentity": build_identity.clone(),
            "mountGrant": grant,
            "islands": route_islands,
        });
        let file = content_name("islands", "json", &pretty_json(&manifest)?);
        route_manifests.insert(page.route.clone(), StaticIslandRouteManifest { file, manifest });
    }
    Ok(Some(StaticIslandPublication {
        build_identity: build_identity.clone(),
        manifest: json!({
            "schema": "witchy.glamour.islands.v1",
            "buildIdentity": build_identity.clone(),
            "mountGrant": grant,
            "islands": manifest_islands,
        }),
        artifact_manifest: json!({
            "schema": "witchy.glamour.island-artifacts.v1",
            "buildIdentity": build_identity,
            "grantDigest": grant.digest,
            "artifacts": artifact_records,
            "workers": workers.iter().map(|worker| json!({
                "artifact": worker.identity,
                "url": format!("/assets/{}", worker.file),
                "export": "__export_export_glamour_worker_execute",
            })).collect::<Vec<_>>(),
            "frames": frames.iter().map(|frame| json!({
                "artifact": frame.identity,
                "url": format!("/assets/{}", frame.file),
            })).collect::<Vec<_>>(),
        }),
        route_manifests,
        pages: rewritten_pages,
        artifacts,
        workers,
        frames,
    }))
}

fn static_frame_document() -> Vec<u8> {
    br#"<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'none'; img-src 'none'; media-src 'none'; font-src 'none'; form-action 'none'"><style>html,body{margin:0;font:inherit;color:inherit;background:transparent}#document{white-space:pre-wrap;overflow-wrap:anywhere;cursor:pointer}</style></head><body><div id="document" role="button" tabindex="0"></div><script>(()=>{'use strict';const enc=new TextEncoder();let port=null,nonce='',renderer='';const exact=(v,k)=>v&&typeof v==='object'&&!Array.isArray(v)&&Object.keys(v).sort().join(',')===k.slice().sort().join(',');const init=(event)=>{const m=event.data;if(event.source!==parent||event.ports.length!==1||!exact(m,['schema','renderer','nonce'])||m.schema!=='witchy.glamour.frame-init.v1'||m.renderer!=='document.v1'||typeof m.nonce!=='string'||!/^glamour-frame-nonce1-[0-9a-f]{64}$/.test(m.nonce))return;window.removeEventListener('message',init);nonce=m.nonce;renderer=m.renderer;port=event.ports[0];port.onmessage=(next)=>{const value=next.data;if(!exact(value,['schema','renderer','nonce','grant'])||value.schema!=='witchy.glamour.frame-grant.v1'||value.renderer!==renderer||value.nonce!==nonce||typeof value.grant!=='string'||enc.encode(value.grant).byteLength>65536){port.close();port=null;return}document.getElementById('document').textContent=value.grant};port.start?.();port.postMessage({schema:'witchy.glamour.frame-ready.v1',renderer,nonce})};window.addEventListener('message',init);const emit=()=>{if(port)port.postMessage({schema:'witchy.glamour.frame-event.v1',renderer,nonce,value:'activate'})};document.getElementById('document').addEventListener('click',emit);document.getElementById('document').addEventListener('keydown',(event)=>{if(event.key==='Enter'||event.key===' '){event.preventDefault();emit()}})})()</script></body></html>"#.to_vec()
}

fn compile_static_worker_artifact(
    checked: &witchy_types::pipeline::CheckedModule,
    metadata: &witchy_lower::codegen::GlamourIslandMetadata,
    descriptor_id: u32,
) -> Result<Vec<u8>, WebCommandError> {
    let execution = witchy_lower::codegen::checked_glamour_worker_execution_module(
        checked,
        metadata,
        descriptor_id,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let generated = witchy_syntax::parser::parse_module("")
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    match witchy_lower::codegen::compile_checked_glamour_island_execution_binary(
        checked,
        &execution,
        &generated,
    ) {
        witchy_lower::codegen::LoweringOutcome::Lowered(wasm) => Ok(wasm),
        witchy_lower::codegen::LoweringOutcome::Unsupported(reason) => Err(
            WebCommandError::failure(format!(
                "cannot lower Glamour worker descriptor {descriptor_id}: {reason}",
            )),
        ),
        witchy_lower::codegen::LoweringOutcome::Rejected(error) => Err(
            WebCommandError::failure(format!(
                "cannot compile Glamour worker descriptor {descriptor_id}: {error}",
            )),
        ),
    }
}

fn compile_static_island_artifact(
    checked: &witchy_types::pipeline::CheckedModule,
    plan: &StaticIslandCompiledPlan,
    metadata: &witchy_lower::codegen::GlamourIslandMetadata,
    build_identity: &str,
) -> Result<Vec<u8>, WebCommandError> {
    let build = u64::from_str_radix(&build_identity[..16], 16)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let build_low = build & 0xffff_ffff;
    let build_high = build >> 32;
    let adapter_module = checked.module().linked_entry.as_deref().ok_or_else(|| {
        WebCommandError::failure("Glamour island adapter has no authenticated entry module")
    })?;
    let state_name = format!("GlamourIslandState{}", plan.wire_id);
    let text_nodes = plan
        .text_nodes
        .iter()
        .map(|node| {
            let path = node
                .path
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("glamour.island_text_node([{path}], {})", node.id)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let attribute_sinks = plan
        .attributes
        .iter()
        .map(|attribute| {
            let path = plan
                .nodes
                .iter()
                .find(|node| node.id == attribute.node)
                .expect("attribute sink has a checked node")
                .path
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "glamour.island_attribute_sink([{path}], {}, {}, \"{}\", \"{}\", {})",
                attribute.node,
                attribute.index,
                attribute.kind,
                attribute.name,
                attribute.id,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let checked_event_plan = plan
        .events
        .iter()
        .rev()
        .fold(
            format!("{adapter_module}.glamour_island_abort_event()"),
            |tail, event| {
                let (owner_scope, owner_instance) = static_island_event_owner(plan, event);
                let value = if event.read_value {
                    "true"
                } else {
                    "value == \"\""
                };
                let checked = if event.read_checked {
                    "true"
                } else {
                    "!checked"
                };
                let key = if event.read_key { "true" } else { "key == \"\"" };
                format!(
                    "if event_plan == {} && event_instance == {} && event_class == {} && {value} && {checked} && {key}: (event_plan, {}, {}) else: {tail}",
                    event.plan, owner_instance, event.event_class, owner_scope, owner_instance,
                )
            },
        );
    let event_descriptors = plan
        .events
        .iter()
        .map(|event| {
            let kind = if event.read_value {
                "value"
            } else if event.read_checked {
                "checked"
            } else if event.read_key {
                "key"
            } else {
                "msg"
            };
            format!(
                "glamour.island_event_descriptor({}, {}, {}, \"{}\", \"{}\", \"{kind}\", {}, {})",
                event.plan,
                event.node,
                event.event_class,
                event.id,
                event.name,
                event.prevent_default,
                event.stop_propagation,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let regions = plan
        .regions
        .iter()
        .map(|region| {
            let path = region
                .path
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            match region.kind {
                StaticIslandRegionKind::List => {
                    let keys = region
                        .keys
                        .iter()
                        .map(|key| {
                            let key_path = key
                                .path
                                .iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join(", ");
                            let slots = key
                                .slots
                                .iter()
                                .map(static_island_template_slot_source)
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!(
                                "glamour.island_region_key({}, {}, {}, [{key_path}], [{slots}])",
                                witchy_source_string(&key.source),
                                key.id,
                                key.template,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let dynamic = region.dynamic.as_ref().map_or_else(
                        || "glamour.island_no_dynamic_key()".to_string(),
                        |key| {
                            let key_path = key
                                .path
                                .iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join(", ");
                            let slots = key
                                .slots
                                .iter()
                                .map(static_island_template_slot_source)
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!(
                                "glamour.island_region_key({}, {}, {}, [{key_path}], [{slots}])",
                                witchy_source_string(&key.source),
                                key.id,
                                key.template,
                            )
                        },
                    );
                    format!(
                        "glamour.island_region([{path}], {}, {}, [{keys}], {dynamic})",
                        region.id, region.parent,
                    )
                }
                StaticIslandRegionKind::Branch | StaticIslandRegionKind::Child => {
                    let child = region
                        .child
                        .as_ref()
                        .expect("structural regions have an authenticated child");
                    let child_path = child
                        .path
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let slots = child
                        .slots
                        .iter()
                        .map(static_island_template_slot_source)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let constructor = match region.kind {
                        StaticIslandRegionKind::Branch => "island_branch_region",
                        StaticIslandRegionKind::Child => "island_child_region",
                        StaticIslandRegionKind::List => unreachable!(),
                    };
                    format!(
                        "glamour.{constructor}([{path}], {}, {}, {}, {}, [{child_path}], [{slots}])",
                        region.id, region.parent, child.template, region.id,
                    )
                }
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let pending_effect_name = format!("GlamourIslandPendingEffect{}", plan.wire_id);
    let pending_subscription_name =
        format!("GlamourIslandPendingSubscription{}", plan.wire_id);
    let effect_schemas = metadata
        .work
        .iter()
        .filter(|work| work.channel == "effect")
        .map(|work| {
            let kind = match work.kind.as_str() {
                "timer" => 1,
                "http" => 2,
                "navigation" => 3,
                "port" => 4,
                "secret" => 5,
                "storage-get" => 6,
                "storage-set" => 7,
                "storage-remove" => 8,
                "worker" => 9,
                "host-port" => 10,
                other => {
                    return Err(WebCommandError::failure(format!(
                        "authenticated effect kind `{other}` has no production adapter"
                    )));
                }
            };
            Ok(format!(
                "if descriptor == {} && completion == {} && kind == {kind}: ({}, {})",
                work.descriptor_id,
                work.completion_id,
                work.result_schema_id,
                work.owner_scope_id,
            ))
        })
        .chain(
            metadata
                .mapped_work
                .iter()
                .filter(|work| work.channel == "effect")
                .map(|work| {
                    let kind = match work.kind.as_str() {
                        "timer" => 1,
                        "http" => 2,
                        "navigation" => 3,
                        "port" => 4,
                        "secret" => 5,
                        "storage-get" => 6,
                        "storage-set" => 7,
                        "storage-remove" => 8,
                        "worker" => 9,
                        "host-port" => 10,
                        other => {
                            return Err(WebCommandError::failure(format!(
                                "authenticated mapped effect kind `{other}` has no production adapter"
                            )));
                        }
                    };
                    Ok(format!(
                        "if descriptor == {} && completion == {} && kind == {kind}: ({}, {})",
                        work.descriptor_id,
                        work.completion_id,
                        work.result_schema_id,
                        work.owner_scope_id,
                    ))
                }),
        )
        .collect::<Result<Vec<_>, WebCommandError>>()?
        .join("\n    else ");
    let effect_schemas = if effect_schemas.is_empty() {
        "fail(\"glamour island: effect descriptor does not match its authenticated callback\")\n    (0, 0)".to_string()
    } else {
        format!("{effect_schemas}\n    else:\n        fail(\"glamour island: effect descriptor does not match its authenticated callback\")\n        (0, 0)")
    };
    let subscription_schemas = metadata
        .work
        .iter()
        .filter(|work| work.channel == "subscription")
        .map(|work| {
            format!(
                "if descriptor == {} && completion == {}: ({}, {})",
                work.descriptor_id,
                work.completion_id,
                work.result_schema_id,
                work.owner_scope_id,
            )
        })
        .chain(
            metadata
                .mapped_work
                .iter()
                .filter(|work| work.channel == "subscription")
                .map(|work| {
                    format!(
                        "if descriptor == {} && completion == {}: ({}, {})",
                        work.descriptor_id,
                        work.completion_id,
                        work.result_schema_id,
                        work.owner_scope_id,
                    )
                }),
        )
        .collect::<Vec<_>>()
        .join("\n    else ");
    let subscription_schemas = if subscription_schemas.is_empty() {
        "fail(\"glamour island: subscription descriptor does not match its authenticated callback\")\n    (0, 0)".to_string()
    } else {
        format!("{subscription_schemas}\n    else:\n        fail(\"glamour island: subscription descriptor does not match its authenticated callback\")\n        (0, 0)")
    };
    let public_json = format!(
        "glamour.island_public_json(input, {}, {build_low}, {build_high})",
        plan.wire_id
    );
    let retains_auth = plan.auth_type != "Nil";
    let state_auth_type = if retains_auth {
        format!("{}, ", plan.auth_type)
    } else {
        String::new()
    };
    let state_auth_value = if retains_auth { "auth, " } else { "" };
    let state_auth_binding = if retains_auth { "auth, " } else { "" };
    let state_auth_ignored = if retains_auth { "_auth, " } else { "" };
    let dispatch_auth = if retains_auth { "auth" } else { "Nil" };
    let program_import = plan
        .program_name
        .rsplit_once('.')
        .and_then(|(module, _)| (module != adapter_module).then_some(module))
        .map_or_else(String::new, |module| format!("import {module}\n"));
    let mut decoded_model_type = None;
    let model_decoder = if plan.mode == "fresh" {
        format!(
            "match {program}():\n        glamour.Program(_authorize, initial, _start, _update, _render, _subscriptions) -> initial(glamour.island_start_input(input, {app_id}, {build_low}, {build_high}))",
            program = plan.program_name,
            app_id = plan.wire_id,
        )
    } else {
        match plan.model_type.as_str() {
        "Int" => format!("match {public_json}:\n        json.JsonInt(model) -> model\n        _ ->\n            fail(\"glamour island: public model does not match authenticated Int\")\n            0"),
        "String" => format!("match {public_json}:\n        json.JsonString(model) -> model\n        _ ->\n            fail(\"glamour island: public model does not match authenticated String\")\n            \"\""),
        "Bool" => format!("match {public_json}:\n        json.JsonBool(model) -> model\n        _ ->\n            fail(\"glamour island: public model does not match authenticated Bool\")\n            false"),
        "Float" => format!("match {public_json}:\n        json.JsonFloat(model) -> model\n        _ ->\n            fail(\"glamour island: public model does not match authenticated Float\")\n            0.0"),
        _ => {
            let model_type = plan.model_type_name.as_deref().ok_or_else(|| {
                WebCommandError::failure(format!(
                    "Glamour island `{}` model `{}` has no compiler-owned public-state decoder",
                    plan.key, plan.model_type
                ))
            })?;
            decoded_model_type = Some(model_type);
            format!(
                "match glamour_island_model_from_json({public_json}):\n        Ok(model) -> model\n        Err(_) -> {adapter_module}.glamour_island_abort_model()"
            )
        }
        }
    };
    let emit = if let Some(fresh) = &plan.fresh {
        let slots = fresh
            .slots
            .iter()
            .map(static_island_template_slot_source)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "if output_sequence == 1:\n                        glamour.island_mount_with_work(render(model), {}, {}, [{slots}], {adapter_module}.glamour_island_events(), work, {app_id}, {build_low}, {build_high})\n                    else:\n                        {adapter_module}.glamour_island_patch(render(baseline_model), render(previous_model), render(model), output_sequence - 1, work)",
            fresh.template,
            fresh.instance,
            app_id = plan.wire_id,
        )
    } else {
        format!(
            "{adapter_module}.glamour_island_patch(render(baseline_model), render(previous_model), render(model), output_sequence - 1, work)"
        )
    };
    let source = format!(
        r#"import glamour
import json
import {adapter_module}
{program_import}

type {state_name}:
    {state_name}({state_auth_type}{model}, {model}, {model}, Int, Int, Int, Int, List({pending_effect_name}), List({pending_subscription_name}), Bool, List(glamour.IslandHostWork))

type {pending_effect_name}:
    {pending_effect_name}(Int, Int, Int, Int, Int, Int, String, glamour.IslandCapture, Int)

type {pending_subscription_name}:
    {pending_subscription_name}(String, Int, Int, Int, Int, Int, Int, String, glamour.IslandCapture, Int)

fn glamour_island_effect_schema(descriptor: Int, completion: Int, kind: Int) -> (Int, Int):
    {effect_schemas}

fn glamour_island_subscription_schema(descriptor: Int, completion: Int) -> (Int, Int):
    {subscription_schemas}

fn glamour_island_without_staged_effect(work: List(glamour.IslandHostWork), instance: Int) -> (List(glamour.IslandHostWork), Bool):
    var retained: List(glamour.IslandHostWork) = []
    var removed = false
    for operation in work:
        match operation:
            glamour.IslandStartEffect(candidate, _key, _descriptor, _request) ->
                if candidate == instance:
                    removed = true
                else:
                    retained.push(operation)
            _ -> retained.push(operation)
    (retained, removed)

fn glamour_island_startup_effect_work(work: List(glamour.IslandHostWork)) -> List(glamour.IslandHostWork):
    var retained: List(glamour.IslandHostWork) = []
    for operation in work:
        match operation:
            glamour.IslandStartEffect(_instance, _key, _descriptor, _request) -> retained.push(operation)
            glamour.IslandCancelEffect(_key) -> retained.push(operation)
            _ -> Nil
    retained

fn glamour_island_without_effect_key(effects: List({pending_effect_name}), key: String, owner_instance: Int, work: List(glamour.IslandHostWork)) -> (List({pending_effect_name}), List(glamour.IslandHostWork)):
    var retained: List({pending_effect_name}) = []
    var staged = work
    for entry in effects:
        match entry:
            {pending_effect_name}(instance, _descriptor, _schema, _completion, _owner_scope, candidate_owner, candidate, _environment, _size) ->
                if key != "" && candidate == key && candidate_owner == owner_instance:
                    let removed = {adapter_module}.glamour_island_without_staged_effect(staged, instance)
                    staged = removed.0
                    if !removed.1:
                        staged.push(glamour.island_cancel_effect(instance))
                else:
                    retained.push(entry)
    (retained, staged)

fn glamour_island_stage_command(command: glamour.Cmd({message}), owner_scope: Int, owner_instance: Int, next_instance: Int, effects: List({pending_effect_name}), work: List(glamour.IslandHostWork)) -> (Int, List({pending_effect_name}), List(glamour.IslandHostWork)):
    var next = next_instance
    var pending = effects
    var staged = work
    match command:
        glamour.NoCmd -> Nil
        glamour.CancelCmd(key) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
        glamour.Batch(commands) ->
            for child in commands:
                let advanced = {adapter_module}.glamour_island_stage_command(child, owner_scope, owner_instance, next, pending, staged)
                next = advanced.0
                pending = advanced.1
                staged = advanced.2
        glamour.DetachedCmd(child) ->
            let advanced = {adapter_module}.glamour_island_stage_command(child, {registry_id}, {registry_id}, next, pending, staged)
            next = advanced.0
            pending = advanced.1
            staged = advanced.2
        glamour.IslandAfter(descriptor, completion, _timer, milliseconds, environment) ->
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 1)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, "", environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, "${{milliseconds}}"))
            next = next + 1
        glamour.IslandAfterStable(descriptor, completion, key, _timer, milliseconds, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 1)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, "${{milliseconds}}"))
            next = next + 1
        glamour.IslandHttpTask(descriptor, completion, declared_owner, key, fetch, method, url, body, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 2)
            if authenticated.1 != declared_owner:
                fail("glamour island: resource command owner differs from its authenticated declaration")
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, declared_owner, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_http_request(fetch, method, url, body)))
            next = next + 1
        glamour.IslandNavigationTask(descriptor, completion, declared_owner, key, route, path, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 3)
            if authenticated.1 != declared_owner:
                fail("glamour island: route command owner differs from its authenticated declaration")
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, declared_owner, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_navigation_request(route, path)))
            next = next + 1
        glamour.IslandPortTask(descriptor, completion, key, credential, argument, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 4)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_port_request(credential, argument)))
            next = next + 1
        glamour.IslandHostPortTask(descriptor, completion, key, adapter, endpoint, maximum_request_bytes, _maximum_result_bytes, request, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 10)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_host_port_request(adapter, endpoint, maximum_request_bytes, request)))
            next = next + 1
        glamour.IslandSecretTask(descriptor, completion, key, secret, credential, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 5)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_secret_request(secret, credential)))
            next = next + 1
        glamour.IslandStorageGetTask(descriptor, completion, key, storage, storage_key, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 6)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_storage_request(storage, storage_key, None)))
            next = next + 1
        glamour.IslandStorageSetTask(descriptor, completion, key, storage, storage_key, value, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 7)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_storage_request(storage, storage_key, Some(value))))
            next = next + 1
        glamour.IslandStorageRemoveTask(descriptor, completion, key, storage, storage_key, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 8)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_storage_request(storage, storage_key, None)))
            next = next + 1
        glamour.IslandWorkerTask(descriptor, completion, key, worker, request, environment) ->
            let cleaned = {adapter_module}.glamour_island_without_effect_key(pending, key, owner_instance, staged)
            pending = cleaned.0
            staged = cleaned.1
            let authenticated = {adapter_module}.glamour_island_effect_schema(descriptor, completion, 9)
            let schema = authenticated.0
            let size = glamour.island_validate_capture(environment, 65536, 32)
            pending.push({pending_effect_name}(next, descriptor, schema, completion, owner_scope, owner_instance, key, environment, size))
            staged.push(glamour.island_start_effect(next, next, descriptor, glamour.island_worker_request(worker, request)))
            next = next + 1
        _ -> fail("glamour island: command was not compiler-specialized")
    if next > 4294967295:
        fail("glamour island: effect instance space is exhausted")
    (next, pending, staged)

fn glamour_island_retain_live_effect_owners(effects: List({pending_effect_name}), live: List(Int), work: List(glamour.IslandHostWork)) -> (List({pending_effect_name}), List(glamour.IslandHostWork)):
    var retained: List({pending_effect_name}) = []
    var staged = work
    for entry in effects:
        match entry:
            {pending_effect_name}(instance, _descriptor, _schema, _completion, _owner_scope, owner_instance, _key, _environment, _size) ->
                if live.contains(owner_instance):
                    retained.push(entry)
                else:
                    let removed = {adapter_module}.glamour_island_without_staged_effect(staged, instance)
                    staged = removed.0
                    if !removed.1:
                        staged.push(glamour.island_cancel_effect(instance))
    (retained, staged)

fn glamour_island_subscription_contains(subscriptions: List({pending_subscription_name}), key: String, descriptor: Int) -> Bool:
    var found = false
    for entry in subscriptions:
        match entry:
            {pending_subscription_name}(candidate, _instance, candidate_descriptor, _schema, _completion, _owner_scope, _owner_instance, _request, _environment, _size) ->
                if candidate == key && candidate_descriptor == descriptor:
                    found = true
    found

fn glamour_island_subscription_instance(subscriptions: List({pending_subscription_name}), key: String, descriptor: Int) -> Int:
    var found = 0
    for entry in subscriptions:
        match entry:
            {pending_subscription_name}(candidate, instance, candidate_descriptor, _schema, _completion, _owner_scope, _owner_instance, _request, _environment, _size) ->
                if candidate == key && candidate_descriptor == descriptor:
                    if found != 0:
                        fail("glamour island: live subscription identity is duplicated")
                    found = instance
    found

fn glamour_island_collect_subscriptions(subscription: glamour.Sub({message}), old: List({pending_subscription_name}), next_instance: Int, desired: List({pending_subscription_name})) -> (Int, List({pending_subscription_name})):
    var next = next_instance
    var collected = desired
    match subscription:
        glamour.NoSub -> Nil
        glamour.BatchSub(subscriptions) ->
            for child in subscriptions:
                let advanced = {adapter_module}.glamour_island_collect_subscriptions(child, old, next, collected)
                next = advanced.0
                collected = advanced.1
        glamour.IslandEvery(descriptor, completion, key, _timer, milliseconds, environment) ->
            let authenticated = {adapter_module}.glamour_island_subscription_schema(descriptor, completion)
            if {adapter_module}.glamour_island_subscription_contains(collected, key, descriptor):
                fail("glamour island: subscription identity is duplicated")
            let existing = {adapter_module}.glamour_island_subscription_instance(old, key, descriptor)
            let instance = if existing == 0: next else: existing
            if existing == 0:
                next = next + 1
            let schema = authenticated.0
            let owner_scope = authenticated.1
            let size = glamour.island_validate_capture(environment, 65536, 32)
            collected.push({pending_subscription_name}(key, instance, descriptor, schema, completion, owner_scope, instance, "${{milliseconds}}", environment, size))
        _ -> fail("glamour island: subscription was not compiler-specialized")
    if next > 4294967295:
        fail("glamour island: subscription instance space is exhausted")
    (next, collected)

fn glamour_island_reconcile_subscriptions(old: List({pending_subscription_name}), desired: List({pending_subscription_name}), work: List(glamour.IslandHostWork)) -> List(glamour.IslandHostWork):
    var staged = work
    for entry in old:
        match entry:
            {pending_subscription_name}(key, instance, descriptor, _schema, _completion, _owner_scope, _owner_instance, _request, _environment, _size) ->
                if !{adapter_module}.glamour_island_subscription_contains(desired, key, descriptor):
                    staged.push(glamour.island_remove_subscription(instance))
    for entry in desired:
        match entry:
            {pending_subscription_name}(_key, instance, descriptor, _schema, _completion, _owner_scope, _owner_instance, request, _environment, _size) -> staged.push(glamour.island_sync_subscription(instance, descriptor, request))
    staged

fn glamour_island_render_live_owner_instances(rendered: glamour.Ui({message})) -> List(Int):
    glamour.island_live_owner_instances(rendered, {adapter_module}.glamour_island_regions(), {registry_id})

fn glamour_island_collect_live_owner_instances(rendered: glamour.Ui({message}), subscriptions: List({pending_subscription_name})) -> List(Int):
    var owners: List(Int) = {adapter_module}.glamour_island_render_live_owner_instances(rendered)
    for entry in subscriptions:
        match entry:
            {pending_subscription_name}(_key, _instance, _descriptor, _schema, _completion, _owner_scope, owner_instance, _request, _environment, _size) -> owners.push(owner_instance)
    owners

fn glamour_island_validate_pending(effects: List({pending_effect_name}), subscriptions: List({pending_subscription_name})):
    if effects.length() > 128 || subscriptions.length() > 64:
        fail("glamour island: pending callback count exceeds its authenticated limit")
    var total = 0
    for entry in effects:
        match entry:
            {pending_effect_name}(_instance, _descriptor, _schema, _completion, _owner_scope, _owner_instance, _key, _environment, size) -> total = total + size
    for entry in subscriptions:
        match entry:
            {pending_subscription_name}(_key, _instance, _descriptor, _schema, _completion, _owner_scope, _owner_instance, _request, _environment, size) -> total = total + size
    if total > 1048576:
        fail("glamour island: pending callback bytes exceed their authenticated limit")

fn glamour_island_effect_at(effects: List({pending_effect_name}), instance: Int, descriptor: Int, schema: Int) -> {pending_effect_name}:
    for entry in effects:
        match entry:
            {pending_effect_name}(candidate, expected_descriptor, expected_schema, _completion, _owner_scope, _owner_instance, _key, _environment, _size) ->
                if candidate == instance:
                    if expected_descriptor != descriptor || expected_schema != schema:
                        fail("glamour island: effect completion does not match its pending entry")
                    return entry
    fail("glamour island: effect completion has no pending entry")
    {adapter_module}.glamour_island_effect_at(effects, instance, descriptor, schema)

fn glamour_island_without_effect_instance(effects: List({pending_effect_name}), instance: Int) -> List({pending_effect_name}):
    var retained: List({pending_effect_name}) = []
    for entry in effects:
        match entry:
            {pending_effect_name}(candidate, _descriptor, _schema, _completion, _owner_scope, _owner_instance, _key, _environment, _size) ->
                if candidate != instance:
                    retained.push(entry)
    retained

fn glamour_island_subscription_at(subscriptions: List({pending_subscription_name}), instance: Int, descriptor: Int, schema: Int) -> {pending_subscription_name}:
    for entry in subscriptions:
        match entry:
            {pending_subscription_name}(_key, candidate, expected_descriptor, expected_schema, _completion, _owner_scope, _owner_instance, _request, _environment, _size) ->
                if candidate == instance:
                    if expected_descriptor != descriptor || expected_schema != schema:
                        fail("glamour island: subscription completion does not match its pending entry")
                    return entry
    fail("glamour island: subscription completion has no pending entry")
    {adapter_module}.glamour_island_subscription_at(subscriptions, instance, descriptor, schema)

fn glamour_island_model(input: Bytes) -> {model}:
    {model_decoder}

fn glamour_island_abort_model() -> {model}:
    fail("glamour island: public model does not match its authenticated type")
    {adapter_module}.glamour_island_abort_model()

fn glamour_island_abort_event() -> (Int, Int, Int):
    fail("glamour island: event does not match its authenticated plan")
    (0, 0, 0)

fn glamour_island_nodes() -> List(glamour.IslandTextNode):
    [{text_nodes}]

fn glamour_island_attributes() -> List(glamour.IslandAttributeSink):
    [{attribute_sinks}]

fn glamour_island_events() -> List(glamour.IslandEventDescriptor):
    [{event_descriptors}]

fn glamour_island_regions() -> List(glamour.IslandRegion):
    [{regions}]

fn glamour_island_decode(rendered: glamour.Ui({message}), event_plan: Int, value: String, checked: Bool, key: String) -> Option({message}):
    glamour.island_decode_event(rendered, {adapter_module}.glamour_island_events(), event_plan, value, checked, key)

fn glamour_island_patch(baseline: glamour.Ui({message}), old: glamour.Ui({message}), new: glamour.Ui({message}), sequence: Int, work: List(glamour.IslandHostWork)) -> Bytes:
    glamour.island_patch_with_work(baseline, old, new, {adapter_module}.glamour_island_nodes(), {adapter_module}.glamour_island_attributes(), {adapter_module}.glamour_island_events(), {adapter_module}.glamour_island_regions(), work, {app_id}, {build_low}, {build_high}, sequence)

@browser
pub fn glamour_init(root: glamour.UiRoot, input: Bytes) -> {state_name}:
    match {program}():
        glamour.Program(authorize, _initial, start, _update, _render, subscriptions) ->
            let auth = authorize(root)
            let model = {adapter_module}.glamour_island_model(input)
            let staged_command = {adapter_module}.glamour_island_stage_command(start(auth, model), {registry_id}, {registry_id}, 1, [], [])
            let staged_subscriptions = {adapter_module}.glamour_island_collect_subscriptions(subscriptions(auth, model), [], 1, [])
            let work = {adapter_module}.glamour_island_reconcile_subscriptions([], staged_subscriptions.1, staged_command.2)
            {adapter_module}.glamour_island_validate_pending(staged_command.1, staged_subscriptions.1)
            {state_name}({state_auth_value}model, model, model, 1, 0, staged_command.0, staged_subscriptions.0, staged_command.1, staged_subscriptions.1, glamour.island_resumed_start(input), work)

@browser
pub fn glamour_dispatch(state: {state_name}, input: Bytes) -> {state_name}:
    match state:
        {state_name}({state_auth_binding}baseline_model, previous_model, model, output_sequence, input_sequence, next_effect, next_subscription, pending_effects, pending_subscriptions, activation_pending, old_work) ->
            if input.length() > 8 && input.at(8) == 3:
                if activation_pending:
                    fail("glamour island: completion cannot overtake activation")
                match glamour.island_completion_input(input, {app_id}, {build_low}, {build_high}, input_sequence):
                    glamour.IslandCompletionInput(source, instance, _generation, descriptor, schema, status, payload) ->
                        let completed = if source == 1:
                            match {adapter_module}.glamour_island_effect_at(pending_effects, instance, descriptor, schema):
                                {pending_effect_name}(_instance, _descriptor, _schema, completion, owner_scope, owner_instance, _key, environment, _size) -> ({adapter_module}.glamour_island_complete(completion, environment, status, payload), {adapter_module}.glamour_island_without_effect_instance(pending_effects, instance), owner_scope, owner_instance)
                        else:
                            match {adapter_module}.glamour_island_subscription_at(pending_subscriptions, instance, descriptor, schema):
                                {pending_subscription_name}(_key, _instance, _descriptor, _schema, completion, owner_scope, owner_instance, _request, environment, _size) -> ({adapter_module}.glamour_island_complete(completion, environment, status, payload), pending_effects, owner_scope, owner_instance)
                        match {program}():
                            glamour.Program(_authorize, _initial, _start, update, render, subscriptions) ->
                                let stepped = update({dispatch_auth}, model, completed.0)
                                let next_model = stepped.0
                                let staged_command = {adapter_module}.glamour_island_stage_command(stepped.1, completed.2, completed.3, next_effect, completed.1, [])
                                let staged_subscriptions = {adapter_module}.glamour_island_collect_subscriptions(subscriptions({dispatch_auth}, next_model), pending_subscriptions, next_subscription, [])
                                let live_owners = {adapter_module}.glamour_island_collect_live_owner_instances(render(next_model), staged_subscriptions.1)
                                let live_effects = {adapter_module}.glamour_island_retain_live_effect_owners(staged_command.1, live_owners, staged_command.2)
                                let work = {adapter_module}.glamour_island_reconcile_subscriptions(pending_subscriptions, staged_subscriptions.1, live_effects.1)
                                {adapter_module}.glamour_island_validate_pending(live_effects.0, staged_subscriptions.1)
                                {state_name}({state_auth_value}baseline_model, model, next_model, output_sequence + 1, input_sequence + 1, staged_command.0, staged_subscriptions.0, live_effects.0, staged_subscriptions.1, false, work)
            else if input.length() > 8 && input.at(8) == 6:
                if !activation_pending:
                    fail("glamour island: activation is already committed")
                glamour.island_activation_input(input, {app_id}, {build_low}, {build_high}, input_sequence)
                {state_name}({state_auth_value}baseline_model, previous_model, model, output_sequence + 1, input_sequence + 1, next_effect, next_subscription, pending_effects, pending_subscriptions, false, old_work)
            else:
                match {program}():
                    glamour.Program(_authorize, _initial, _start, update, render, subscriptions) ->
                        let old_rendered = render(model)
                        match glamour.island_event_input(input, {app_id}, {build_low}, {build_high}, input_sequence):
                            glamour.IslandEventInput(event_plan, event_instance, event_class, value, checked, key) ->
                                let event_owner = {checked_event_plan}
                                let event_plan = event_owner.0
                                let stepped = match {adapter_module}.glamour_island_decode(old_rendered, event_plan, value, checked, key):
                                    Some(message) -> update({dispatch_auth}, model, message)
                                    None -> (model, NoCmd)
                                let next_model = stepped.0
                                let base_work = if activation_pending: {adapter_module}.glamour_island_startup_effect_work(old_work) else: []
                                let staged_command = {adapter_module}.glamour_island_stage_command(stepped.1, event_owner.1, event_owner.2, next_effect, pending_effects, base_work)
                                let staged_subscriptions = {adapter_module}.glamour_island_collect_subscriptions(subscriptions({dispatch_auth}, next_model), pending_subscriptions, next_subscription, [])
                                let live_owners = {adapter_module}.glamour_island_collect_live_owner_instances(render(next_model), staged_subscriptions.1)
                                let live_effects = {adapter_module}.glamour_island_retain_live_effect_owners(staged_command.1, live_owners, staged_command.2)
                                let previous_subscriptions = if activation_pending: [] else: pending_subscriptions
                                let work = {adapter_module}.glamour_island_reconcile_subscriptions(previous_subscriptions, staged_subscriptions.1, live_effects.1)
                                {adapter_module}.glamour_island_validate_pending(live_effects.0, staged_subscriptions.1)
                                {state_name}({state_auth_value}baseline_model, model, next_model, output_sequence + 1, input_sequence + 1, staged_command.0, staged_subscriptions.0, live_effects.0, staged_subscriptions.1, false, work)

@browser
pub fn glamour_emit(state: {state_name}) -> Bytes:
    match state:
        {state_name}({state_auth_ignored}baseline_model, previous_model, model, output_sequence, _input_sequence, _next_effect, _next_subscription, _pending_effects, _pending_subscriptions, _activation_pending, work) ->
            match {program}():
                glamour.Program(_authorize, _initial, _start, _update, render, _subscriptions) ->
                    {emit}

@browser
pub fn glamour_release(own state: {state_name}):
    match state:
        {state_name}({state_auth_ignored}_baseline_model, _previous_model, _model, _output_sequence, _input_sequence, _next_effect, _next_subscription, _pending_effects, _pending_subscriptions, _activation_pending, _work) -> Nil
"#,
        model = plan.model_type,
        message = plan.message_type,
        program = plan.program_name,
        app_id = plan.wire_id,
        registry_id = plan.registry_id,
        pending_effect_name = pending_effect_name,
        pending_subscription_name = pending_subscription_name,
        effect_schemas = effect_schemas,
        subscription_schemas = subscription_schemas,
        program_import = program_import,
    );
    let mut generated = witchy_syntax::parser::parse_module(&source).map_err(|error| {
        WebCommandError::failure(format!(
            "cannot synthesize Glamour island `{}` adapter: {error}",
            plan.key
        ))
    })?;
    witchy_syntax::linker::reclassify_parsed_module_members(&mut generated);
    if let Some(model_type) = decoded_model_type {
        let model_function = generated
            .items
            .iter_mut()
            .find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function)
                    if function.name == "glamour_island_model" =>
                {
                    Some(function)
                }
                _ => None,
            })
            .expect("generated adapter has a model decoder");
        let Some(witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Match {
            scrutinee,
            ..
        })) = model_function.body.stmts.first_mut()
        else {
            return Err(WebCommandError::failure(
                "compiler-generated Glamour island model decoder has an invalid shape",
            ));
        };
        let witchy_syntax::ast::Expr::Call { args, .. } = std::mem::replace(
            scrutinee.as_mut(),
            witchy_syntax::ast::Expr::Bool(false),
        ) else {
            return Err(WebCommandError::failure(
                "compiler-generated Glamour island model decoder has no direct implementation call",
            ));
        };
        *scrutinee.as_mut() = witchy_syntax::ast::Expr::MethodCall {
            receiver: Box::new(witchy_syntax::ast::Expr::Ctor {
                name: model_type.to_string(),
                args: Vec::new(),
            }),
            method: "from_json".into(),
            args,
        };
    }
    for item in &mut generated.items {
        if let witchy_syntax::ast::Item::Function(function) = item {
            function.name = format!("{adapter_module}.{}", function.name);
        }
    }
    generated.imports.clear();
    generated.import_lines.clear();
    let execution = witchy_lower::codegen::checked_glamour_island_execution_module(
        checked,
        metadata,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    match witchy_lower::codegen::compile_checked_glamour_island_execution_binary(
        checked,
        &execution,
        &generated,
    ) {
        witchy_lower::codegen::LoweringOutcome::Lowered(wasm) => Ok(wasm),
        witchy_lower::codegen::LoweringOutcome::Unsupported(reason) => Err(WebCommandError::failure(
            format!("cannot lower Glamour island `{}` adapter: {reason}", plan.key),
        )),
        witchy_lower::codegen::LoweringOutcome::Rejected(error) => Err(
            WebCommandError::failure(format!(
                "cannot compile Glamour island `{}` adapter: {error}",
                plan.key
            )),
        ),
    }
}

fn island_model_type_name(model: &witchy_syntax::ast::Type) -> Option<String> {
    let witchy_syntax::ast::Type::Named(name, _) = model else {
        return None;
    };
    (!matches!(name.as_str(), "Int" | "String" | "Bool" | "Float"))
        .then(|| name.clone())
}

#[allow(clippy::too_many_arguments)]
fn compile_static_island_node(
    island: &StaticIsland,
    checked: &witchy_lower::codegen::GlamourIslandMetadata,
    node: &StaticIslandResumeNode,
    template_node: &StaticIslandTemplateNode,
    path: &[u32],
    dom_path: &[u32],
    live: bool,
    scope: &str,
    progressive_form: Option<&StaticIslandProgressiveFallback>,
    plan: &mut StaticIslandCompiledPlan,
    node_ids: &mut BTreeMap<u32, String>,
    event_plan_ids: &mut BTreeMap<u32, String>,
    event_class_ids: &mut BTreeMap<u32, String>,
    sink_ids: &mut BTreeMap<u32, String>,
    region_ids: &mut BTreeMap<u32, String>,
    key_ids: &mut BTreeMap<u32, String>,
    template_ids: &mut BTreeMap<u32, String>,
    slot_ids: &mut BTreeMap<u32, String>,
    event_pairs: &mut BTreeSet<(u32, String)>,
) -> Result<(), WebCommandError> {
    if let StaticIslandResumeNode::Keyed { key, child } = node {
        let StaticIslandTemplateNode::Keyed {
            key: template_key,
            child: template_child,
        } = template_node
        else {
            unreachable!("validated template graph matches its resume graph")
        };
        debug_assert_eq!(key, template_key);
        return compile_static_island_node(
            island,
            checked,
            child,
            template_child,
            path,
            dom_path,
            live,
            &format!("{scope}|key={key}"),
            progressive_form,
            plan,
            node_ids,
            event_plan_ids,
            event_class_ids,
            sink_ids,
            region_ids,
            key_ids,
            template_ids,
            slot_ids,
            event_pairs,
        );
    }
    if matches!(
        node,
        StaticIslandResumeNode::Branch { .. } | StaticIslandResumeNode::Child { .. }
    ) {
        return Err(WebCommandError::failure(format!(
            "static island `{}` structural regions must be direct element children",
            island.key
        )));
    }
    let kind = match node {
        StaticIslandResumeNode::Text => "text".to_string(),
        StaticIslandResumeNode::Frame { renderer, .. } => format!("frame:{renderer}"),
        StaticIslandResumeNode::Element { tag, .. } => format!("element:{tag}"),
        StaticIslandResumeNode::Keyed { .. } => unreachable!("keyed nodes return above"),
        StaticIslandResumeNode::Branch { .. } => unreachable!("branch nodes return above"),
        StaticIslandResumeNode::Child { .. } => unreachable!("child nodes return above"),
    };
    let path_text = path.iter().map(u32::to_string).collect::<Vec<_>>().join("/");
    let node_material = format!(
        "witchy.glamour.island-node.v1|artifact={}|scope={scope}|path={path_text}|kind={kind}",
        checked.identity,
    );
    let node_id = checked_wire_id(&island.key, "node", &node_material, node_ids)?;
    plan.nodes.push(StaticIslandNodeRecord {
        id: node_id,
        path: path.to_vec(),
        dom_path: dom_path.to_vec(),
        live,
    });
    if matches!(node, StaticIslandResumeNode::Text) {
        plan.text_nodes.push(StaticIslandTextNodeRecord {
            id: node_id,
            path: path.to_vec(),
        });
    }
    if let StaticIslandResumeNode::Frame {
        renderer,
        max_grant_bytes,
        max_event_bytes,
        grant,
        fallback,
        id,
        decoder,
    } = node
    {
        debug_assert_eq!(decoder, "value");
        if !event_pairs.insert((node_id, "glamour-frame".into())) {
            return Err(WebCommandError::failure(format!(
                "static island `{}` repeats its frame event on one node",
                island.key
            )));
        }
        let class_material = format!(
            "witchy.glamour.event-class.v1|artifact={}|event=glamour-frame",
            checked.identity,
        );
        let event_class = checked_wire_id(
            &island.key,
            "event class",
            &class_material,
            event_class_ids,
        )?;
        let plan_material = format!(
            "witchy.glamour.event-plan.v1|artifact={}|node={node_id}|id={id}|event=glamour-frame|kind=value|prevent=false|stop=false",
            checked.identity,
        );
        let event_plan = checked_wire_id(
            &island.key,
            "event plan",
            &plan_material,
            event_plan_ids,
        )?;
        plan.events.push(StaticIslandEventRecord {
            id: id.clone(),
            name: "glamour-frame".into(),
            node: node_id,
            plan: event_plan,
            event_class,
            prevent_default: false,
            stop_propagation: false,
            read_value: true,
            read_checked: false,
            read_key: false,
            fallback: None,
        });
        plan.frames.push(StaticIslandFrameRecord {
            node: node_id,
            event_plan,
            renderer: renderer.clone(),
            max_grant_bytes: *max_grant_bytes,
            max_event_bytes: *max_event_bytes,
            grant: grant.clone(),
            fallback: fallback.clone(),
            artifact: String::new(),
            url: String::new(),
            nonce: String::new(),
        });
        return Ok(());
    }
    let StaticIslandResumeNode::Element {
        tag,
        attributes,
        events,
        children,
    } = node
    else {
        return Ok(());
    };
    let StaticIslandTemplateNode::Element {
        attributes: template_attributes,
        children: template_children,
        ..
    } = template_node
    else {
        unreachable!("validated template graph matches its resume graph")
    };
    let owned_progressive_form = if tag == "form" {
        Some(static_island_form_fallback(&island.key, template_attributes)?)
    } else {
        None
    };
    let progressive_form = owned_progressive_form.as_ref().or(progressive_form);
    for (index, attribute) in attributes.iter().enumerate() {
        let category = island_attribute_category(&attribute.kind);
        let id = if category == "class" {
            0
        } else {
            let material = format!(
                "witchy.glamour.island-sink.v1|artifact={}|category={category}|name={}",
                checked.identity, attribute.name,
            );
            checked_wire_id(&island.key, "attribute sink", &material, sink_ids)?
        };
        plan.attributes.push(StaticIslandAttributeRecord {
            id,
            node: node_id,
            index: u32::try_from(index).map_err(|_| {
                WebCommandError::failure(format!(
                    "static island `{}` has too many attributes",
                    island.key
                ))
            })?,
            kind: attribute.kind.clone(),
            name: attribute.name.clone(),
        });
    }
    for event in events {
        if !event_pairs.insert((node_id, event.event.clone())) {
            return Err(WebCommandError::failure(format!(
                "static island `{}` repeats event `{}` on one node",
                island.key, event.event
            )));
        }
        let class_material = format!(
            "witchy.glamour.event-class.v1|artifact={}|event={}",
            checked.identity, event.event,
        );
        let event_class = checked_wire_id(
            &island.key,
            "event class",
            &class_material,
            event_class_ids,
        )?;
        let fallback = static_island_progressive_fallback(
            &island.key,
            tag,
            template_attributes,
            event,
            progressive_form,
        )?;
        let plan_material = format!(
            "witchy.glamour.event-plan.v1|artifact={}|node={node_id}|id={}|event={}|kind={}|prevent={}|stop={}",
            checked.identity,
            event.id,
            event.event,
            event.kind,
            event.prevent_default,
            event.stop_propagation,
        );
        let event_plan = checked_wire_id(
            &island.key,
            "event plan",
            &plan_material,
            event_plan_ids,
        )?;
        plan.events.push(StaticIslandEventRecord {
            id: event.id.clone(),
            name: event.event.clone(),
            node: node_id,
            plan: event_plan,
            event_class,
            prevent_default: event.prevent_default,
            stop_propagation: event.stop_propagation,
            read_value: event.kind == "value",
            read_checked: event.kind == "checked",
            read_key: event.kind == "key",
            fallback,
        });
    }
    let has_keyed_children = children
        .iter()
        .any(|child| matches!(child, StaticIslandResumeNode::Keyed { .. }));
    if has_keyed_children
        && children
            .iter()
            .any(|child| !matches!(child, StaticIslandResumeNode::Keyed { .. }))
    {
        return Err(WebCommandError::failure(format!(
            "static island `{}` mixes keyed and unkeyed children in one region",
            island.key
        )));
    }
    let region_id = if has_keyed_children {
        let path_text = path.iter().map(u32::to_string).collect::<Vec<_>>().join("/");
        let material = format!(
            "witchy.glamour.island-region.v1|artifact={}|scope={scope}|parent={node_id}|path={path_text}",
            checked.identity,
        );
        Some(checked_wire_id(
            &island.key,
            "region",
            &material,
            region_ids,
        )?)
    } else {
        None
    };
    let mut region_keys = Vec::new();
    let mut source_keys = BTreeSet::new();
    let mut structural_ids = BTreeSet::new();
    let mut dom_index = 0_u32;
    for (index, (child, template_child)) in children.iter().zip(template_children).enumerate() {
        let child_index = u32::try_from(index).map_err(|_| {
            WebCommandError::failure(format!(
                "static island `{}` has too many children",
                island.key
            ))
        })?;
        let mut child_path = path.to_vec();
        child_path.push(child_index);
        let mut child_dom_path = dom_path.to_vec();
        child_dom_path.push(dom_index);
        let structural = match (child, template_child) {
            (
                StaticIslandResumeNode::Branch { id, active, child },
                StaticIslandTemplateNode::Branch {
                    id: template_id,
                    child: template_child,
                },
            ) => {
                debug_assert_eq!(id, template_id);
                Some((
                    StaticIslandRegionKind::Branch,
                    "branch",
                    id,
                    child.as_ref(),
                    template_child.as_ref(),
                    *active,
                ))
            }
            (
                StaticIslandResumeNode::Child { id, mounted, child },
                StaticIslandTemplateNode::Child {
                    id: template_id,
                    child: template_child,
                },
            ) => {
                debug_assert_eq!(id, template_id);
                Some((
                    StaticIslandRegionKind::Child,
                    "child",
                    id,
                    child.as_ref(),
                    template_child.as_ref(),
                    *mounted,
                ))
            }
            _ => None,
        };
        if let Some((
            structural_kind,
            kind_label,
            structural_id,
            structural_child,
            template_structural_child,
            mounted,
        )) = structural
        {
            if !structural_ids.insert(structural_id.clone()) {
                return Err(WebCommandError::failure(format!(
                    "static island `{}` repeats structural child `{structural_id}` under one parent",
                    island.key
                )));
            }
            let node_start = plan.nodes.len();
            let event_start = plan.events.len();
            let region_start = plan.regions.len();
            let structural_scope = format!("{scope}|{kind_label}={structural_id}");
            compile_static_island_node(
                island,
                checked,
                structural_child,
                template_structural_child,
                &child_path,
                &child_dom_path,
                live && mounted,
                &structural_scope,
                progressive_form,
                plan,
                node_ids,
                event_plan_ids,
                event_class_ids,
                sink_ids,
                region_ids,
                key_ids,
                template_ids,
                slot_ids,
                event_pairs,
            )?;
            if !mounted && plan.regions.len() != region_start {
                return Err(WebCommandError::failure(format!(
                    "static island `{}` initially inactive {kind_label} `{structural_id}` contains a nested region",
                    island.key
                )));
            }
            let root = plan.nodes.get(node_start).ok_or_else(|| {
                WebCommandError::failure(format!(
                    "static island `{}` {kind_label} `{structural_id}` has no resumable root",
                    island.key
                ))
            })?;
            let path_text = child_path
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("/");
            let region_material = format!(
                "witchy.glamour.island-{kind_label}-region.v1|artifact={}|scope={scope}|parent={node_id}|path={path_text}|{kind_label}={structural_id}",
                checked.identity,
            );
            let region = checked_wire_id(
                &island.key,
                &format!("{kind_label} region"),
                &region_material,
                region_ids,
            )?;
            let template_material = format!(
                "witchy.glamour.island-{kind_label}-template.v1|artifact={}|region={region}|{kind_label}={structural_id}",
                checked.identity,
            );
            let template = checked_wire_id(
                &island.key,
                &format!("{kind_label} template"),
                &template_material,
                template_ids,
            )?;
            let template_root = compile_static_island_template_root(
                &island.key,
                template_structural_child,
                &child_path,
                plan,
            )?;
            let template_regions = compile_static_island_template_regions(plan, &child_path);
            let slots = compile_static_island_template_slots(
                island,
                checked,
                template,
                &child_path,
                plan,
                slot_ids,
            )?;
            let nodes = plan.nodes[node_start..]
                .iter()
                .map(|node| node.id)
                .collect::<Vec<_>>();
            let template_events = json!(plan.events[event_start..]
                .iter()
                .map(|event| json!({
                    "node": event.node,
                    "eventClass": event.event_class,
                    "eventPlan": event.plan,
                }))
                .collect::<Vec<_>>());
            plan.regions.push(StaticIslandRegionRecord {
                id: region,
                parent: node_id,
                kind: structural_kind,
                before: Vec::new(),
                live,
                path: child_path.clone(),
                keys: Vec::new(),
                dynamic: None,
                child: Some(StaticIslandRegionChildRecord {
                    root: root.id,
                    nodes,
                    template,
                    slots,
                    mounted,
                    template_root,
                    template_regions,
                    template_events,
                    path: child_path,
                }),
            });
            if live && mounted {
                dom_index += 1;
            }
            continue;
        }
        let node_start = plan.nodes.len();
        compile_static_island_node(
            island,
            checked,
            child,
            template_child,
            &child_path,
            &child_dom_path,
            live,
            scope,
            progressive_form,
            plan,
            node_ids,
            event_plan_ids,
            event_class_ids,
            sink_ids,
            region_ids,
            key_ids,
            template_ids,
            slot_ids,
            event_pairs,
        )?;
        if live {
            dom_index += 1;
        }
        if let (Some(region), StaticIslandResumeNode::Keyed { key, .. }) = (region_id, child) {
            if !source_keys.insert(key.clone()) {
                return Err(WebCommandError::failure(format!(
                    "static island `{}` repeats keyed child `{key}`",
                    island.key
                )));
            }
            let root = plan.nodes.get(node_start).ok_or_else(|| {
                WebCommandError::failure(format!(
                    "static island `{}` keyed child `{key}` has no resumable root",
                    island.key
                ))
            })?;
            let material = format!(
                "witchy.glamour.island-key.v1|artifact={}|region={region}|key={key}",
                checked.identity,
            );
            let id = checked_wire_id(&island.key, "region key", &material, key_ids)?;
            let material = format!(
                "witchy.glamour.island-template.v1|artifact={}|region={region}|key={key}",
                checked.identity,
            );
            let template = checked_wire_id(
                &island.key,
                "region template",
                &material,
                template_ids,
            )?;
            let template_root = compile_static_island_template_root(
                &island.key,
                template_child,
                &child_path,
                plan,
            )?;
            let template_regions = compile_static_island_template_regions(plan, &child_path);
            let slots = compile_static_island_template_slots(
                island,
                checked,
                template,
                &child_path,
                plan,
                slot_ids,
            )?;
            let nodes = plan.nodes[node_start..]
                .iter()
                .map(|node| node.id)
                .collect::<Vec<_>>();
            let template_events = json!(plan
                .events
                .iter()
                .filter(|event| nodes.contains(&event.node))
                .map(|event| json!({
                    "node": event.node,
                    "eventClass": event.event_class,
                    "eventPlan": event.plan,
                }))
                .collect::<Vec<_>>());
            region_keys.push(StaticIslandRegionKeyRecord {
                id,
                root: root.id,
                nodes,
                template,
                slots,
                template_root,
                template_regions,
                template_events,
                source: key.clone(),
                path: child_path,
            });
        }
    }
    let direct_child_roots = children
        .iter()
        .enumerate()
        .map(|(index, _child)| {
            let mut child_path = path.to_vec();
            child_path.push(u32::try_from(index).expect("child count was checked"));
            plan.nodes
                .iter()
                .find(|node| node.path == child_path)
                .map(|node| node.id)
                .expect("every checked child has a root node")
        })
        .collect::<Vec<_>>();
    for region in plan.regions.iter_mut().filter(|region| {
        region.parent == node_id
            && region.path.len() == path.len() + 1
            && region.path.starts_with(path)
    }) {
        let index = usize::try_from(*region.path.last().expect("direct child path"))
            .expect("child index fits usize");
        region.before = direct_child_roots[index + 1..].to_vec();
    }
    if let Some(id) = region_id {
        let dynamic = static_island_dynamic_key_prototype(&region_keys);
        plan.regions.push(StaticIslandRegionRecord {
            id,
            parent: node_id,
            kind: StaticIslandRegionKind::List,
            before: Vec::new(),
            live,
            path: path.to_vec(),
            keys: region_keys,
            dynamic,
            child: None,
        });
    }
    Ok(())
}

fn static_island_template_attribute_value<'a>(
    attributes: &'a [StaticIslandTemplateAttribute],
    name: &str,
) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name && attribute.enabled)
        .map(|attribute| attribute.value.as_str())
}

fn static_island_form_fallback(
    key: &str,
    attributes: &[StaticIslandTemplateAttribute],
) -> Result<StaticIslandProgressiveFallback, WebCommandError> {
    let action = static_island_template_attribute_value(attributes, "action")
        .unwrap_or("")
        .to_string();
    if !action.is_empty() && !valid_static_form_url(&action) {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` cannot authenticate progressive form action `{action}`",
        )));
    }
    let method = static_island_template_attribute_value(attributes, "method")
        .unwrap_or("get")
        .to_ascii_lowercase();
    if !matches!(method.as_str(), "get" | "post") {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` cannot authenticate progressive form method `{method}`",
        )));
    }
    Ok(StaticIslandProgressiveFallback::Submit { action, method })
}

fn static_island_progressive_fallback(
    key: &str,
    tag: &str,
    attributes: &[StaticIslandTemplateAttribute],
    event: &StaticIslandResumeEvent,
    progressive_form: Option<&StaticIslandProgressiveFallback>,
) -> Result<Option<StaticIslandProgressiveFallback>, WebCommandError> {
    if !event.prevent_default {
        return Ok(None);
    }
    if event.event == "click" && tag == "a" {
        let Some(href) = static_island_template_attribute_value(attributes, "href") else {
            return Ok(None);
        };
        if !valid_static_form_url(href) {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` cannot authenticate progressive navigation `{href}`",
            )));
        }
        return Ok(Some(StaticIslandProgressiveFallback::Navigate {
            href: href.to_string(),
        }));
    }
    let submit_control = event.event == "click"
        && (tag == "button"
            && static_island_template_attribute_value(attributes, "type")
                .is_none_or(|kind| kind.eq_ignore_ascii_case("submit"))
            || tag == "input"
                && static_island_template_attribute_value(attributes, "type")
                    .is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("submit")
                            || kind.eq_ignore_ascii_case("image")
                    }));
    if event.event == "submit" && tag == "form" || submit_control {
        if let Some(fallback) = progressive_form {
            return Ok(Some(fallback.clone()));
        }
        if submit_control && static_island_template_attribute_value(attributes, "form").is_some() {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` cannot authenticate a deferred submit control for a non-ancestor form",
            )));
        }
    }
    Ok(None)
}

fn static_island_dynamic_key_prototype(
    keys: &[StaticIslandRegionKeyRecord],
) -> Option<StaticIslandRegionKeyRecord> {
    let first = keys.first()?;
    let shape = static_island_dynamic_root_shape(&first.template_root)?;
    let slots = static_island_dynamic_slot_shape(first);
    if !static_island_dynamic_template_is_flat(first) {
        return None;
    }
    keys.iter()
        .all(|key| {
            static_island_dynamic_template_is_flat(key)
                && static_island_dynamic_root_shape(&key.template_root).as_ref() == Some(&shape)
                && static_island_dynamic_slot_shape(key) == slots
        })
        .then(|| first.clone())
}

fn static_island_dynamic_template_is_flat(key: &StaticIslandRegionKeyRecord) -> bool {
    key.template_regions
        .as_object()
        .is_some_and(serde_json::Map::is_empty)
        && key
            .template_events
            .as_array()
            .is_some_and(Vec::is_empty)
}

fn static_island_dynamic_root_shape(node: &Value) -> Option<Value> {
    match node.get("kind")?.as_str()? {
        "text" => Some(json!({"kind": "text"})),
        "element" => Some(json!({
            "kind": "element",
            "tag": node.get("tag")?.as_str()?,
            "children": node.get("children")?.as_array()?.iter()
                .map(static_island_dynamic_root_shape)
                .collect::<Option<Vec<_>>>()?,
        })),
        _ => None,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct StaticIslandDynamicSlotShape {
    path: Vec<u32>,
    index: Option<u32>,
    kind: String,
    source_kind: String,
    name: String,
}

fn static_island_dynamic_slot_shape(
    key: &StaticIslandRegionKeyRecord,
) -> Vec<StaticIslandDynamicSlotShape> {
    key.slots
        .iter()
        .map(|slot| {
            let relative = slot
                .path
                .strip_prefix(key.path.as_slice())
                .unwrap_or(slot.path.as_slice())
                .to_vec();
            StaticIslandDynamicSlotShape {
                path: relative,
                index: slot.index,
                kind: slot.kind.clone(),
                source_kind: slot.source_kind.clone(),
                name: slot.name.clone(),
            }
        })
        .collect()
}

fn compile_static_island_template_regions(
    plan: &StaticIslandCompiledPlan,
    root_path: &[u32],
) -> Value {
    let mut regions = serde_json::Map::new();
    for region in plan
        .regions
        .iter()
        .filter(|region| region.path.starts_with(root_path))
    {
        regions.insert(
            region.id.to_string(),
            json!({
                "parent": region.parent,
                "kind": region.kind,
                "before": region.before,
                "template": region.child.as_ref().map(|child| child.template).unwrap_or(0),
                "dynamicTemplate": region.dynamic.as_ref().map(|key| key.template).unwrap_or(0),
                "keys": region.keys.iter().map(|key| json!({
                    "key": key.id,
                    "source": key.source,
                    "root": key.root,
                    "nodes": key.nodes,
                })).collect::<Vec<_>>(),
                "child": region.child.as_ref().map(|child| json!({
                    "root": child.root,
                    "nodes": child.nodes,
                })),
            }),
        );
    }
    Value::Object(regions)
}

fn compile_static_island_template_slots(
    island: &StaticIsland,
    checked: &witchy_lower::codegen::GlamourIslandMetadata,
    template: u32,
    root_path: &[u32],
    plan: &StaticIslandCompiledPlan,
    slot_ids: &mut BTreeMap<u32, String>,
) -> Result<Vec<StaticIslandTemplateSlotRecord>, WebCommandError> {
    let mut unstable_paths = Vec::new();
    for region in plan
        .regions
        .iter()
        .filter(|region| region.path.starts_with(root_path))
    {
        match region.kind {
            StaticIslandRegionKind::List => unstable_paths.extend(
                region
                    .keys
                    .iter()
                    .map(|key| key.path.clone()),
            ),
            StaticIslandRegionKind::Branch | StaticIslandRegionKind::Child => {
                unstable_paths.push(
                    region
                        .child
                        .as_ref()
                        .expect("structural region has a child")
                        .path
                        .clone(),
                );
            }
        }
    }
    let is_stable = |path: &[u32]| {
        !unstable_paths
            .iter()
            .any(|unstable| path.starts_with(unstable))
    };
    let mut slots = Vec::new();
    for text in plan
        .text_nodes
        .iter()
        .filter(|text| text.path.starts_with(root_path) && is_stable(&text.path))
    {
        let path = text
            .path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("/");
        let material = format!(
            "witchy.glamour.island-template-slot.v1|artifact={}|template={template}|path={path}|kind=text",
            checked.identity,
        );
        let id = checked_wire_id(&island.key, "template slot", &material, slot_ids)?;
        slots.push(StaticIslandTemplateSlotRecord {
            id,
            node: text.id,
            kind: "text".into(),
            sink: 0,
            source_kind: "text".into(),
            path: text.path.clone(),
            index: None,
            name: String::new(),
        });
    }
    for attribute in plan.attributes.iter().filter(|attribute| {
        plan.nodes
            .iter()
            .find(|node| node.id == attribute.node)
            .is_some_and(|node| node.path.starts_with(root_path) && is_stable(&node.path))
    }) {
        let node = plan
            .nodes
            .iter()
            .find(|node| node.id == attribute.node)
            .expect("attribute node was checked above");
        let path = node
            .path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("/");
        let material = format!(
            "witchy.glamour.island-template-slot.v1|artifact={}|template={template}|path={path}|index={}|kind={}|name={}",
            checked.identity, attribute.index, attribute.kind, attribute.name,
        );
        let id = checked_wire_id(&island.key, "template slot", &material, slot_ids)?;
        let kind = if attribute.kind.starts_with("css-") {
            "custom-property"
        } else {
            match attribute.kind.as_str() {
                "property" => "property",
                "aria" => "aria",
                "class" => "class",
                "boolean" => "boolean",
                "attribute" | "url" | "prop" => "attribute",
                _ => unreachable!("checked scalar template attribute"),
            }
        };
        slots.push(StaticIslandTemplateSlotRecord {
            id,
            node: attribute.node,
            kind: kind.into(),
            sink: attribute.id,
            source_kind: attribute.kind.clone(),
            path: node.path.clone(),
            index: Some(attribute.index),
            name: attribute.name.clone(),
        });
    }
    Ok(slots)
}

fn compile_static_island_template_root(
    island_key: &str,
    node: &StaticIslandTemplateNode,
    path: &[u32],
    plan: &StaticIslandCompiledPlan,
) -> Result<Value, WebCommandError> {
    if let StaticIslandTemplateNode::Keyed { child, .. } = node {
        return compile_static_island_template_root(island_key, child, path, plan);
    }
    if let StaticIslandTemplateNode::Branch { child, .. } = node {
        return compile_static_island_template_root(island_key, child, path, plan);
    }
    if let StaticIslandTemplateNode::Child { child, .. } = node {
        return compile_static_island_template_root(island_key, child, path, plan);
    }
    let id = plan
        .nodes
        .iter()
        .find(|node| node.path == path)
        .map(|node| node.id)
        .ok_or_else(|| {
            WebCommandError::failure(format!(
                "static island `{island_key}` template node has no authenticated identity"
            ))
        })?;
    match node {
        StaticIslandTemplateNode::Text { value } => Ok(json!({
            "kind": "text",
            "node": id,
            "text": value,
        })),
        StaticIslandTemplateNode::Frame { .. } => Ok(json!({
            "kind": "element",
            "tag": "iframe",
            "node": id,
            "attributes": {
                "sandbox": "allow-scripts",
                "class": "glamour-compartment",
            },
            "children": [],
        })),
        StaticIslandTemplateNode::Element {
            tag,
            attributes,
            children,
        } => {
            let static_attributes = compile_static_island_template_attributes(attributes);
            let children = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let mut child_path = path.to_vec();
                    child_path.push(u32::try_from(index).map_err(|_| {
                        WebCommandError::failure(format!(
                            "static island `{island_key}` template has too many children"
                        ))
                    })?);
                    compile_static_island_template_root(island_key, child, &child_path, plan)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({
                "kind": "element",
                "tag": tag,
                "node": id,
                "attributes": static_attributes,
                "children": children,
            }))
        }
        StaticIslandTemplateNode::Keyed { .. } => {
            unreachable!("keyed template nodes return above")
        }
        StaticIslandTemplateNode::Branch { .. } => {
            unreachable!("branch template nodes return above")
        }
        StaticIslandTemplateNode::Child { .. } => {
            unreachable!("child template nodes return above")
        }
    }
}

fn compile_static_island_live_template_root(
    island_key: &str,
    resume: &StaticIslandResumeNode,
    template: &StaticIslandTemplateNode,
    path: &[u32],
    plan: &StaticIslandCompiledPlan,
) -> Result<Value, WebCommandError> {
    fn live_node(
        island_key: &str,
        resume: &StaticIslandResumeNode,
        template: &StaticIslandTemplateNode,
        path: &[u32],
        plan: &StaticIslandCompiledPlan,
    ) -> Result<Option<Value>, WebCommandError> {
        match (resume, template) {
            (
                StaticIslandResumeNode::Keyed { child: resume, .. },
                StaticIslandTemplateNode::Keyed { child: template, .. },
            ) => live_node(island_key, resume, template, path, plan),
            (
                StaticIslandResumeNode::Branch { active, child: resume, .. },
                StaticIslandTemplateNode::Branch { child: template, .. },
            ) => {
                if *active {
                    live_node(island_key, resume, template, path, plan)
                } else {
                    Ok(None)
                }
            }
            (
                StaticIslandResumeNode::Child { mounted, child: resume, .. },
                StaticIslandTemplateNode::Child { child: template, .. },
            ) => {
                if *mounted {
                    live_node(island_key, resume, template, path, plan)
                } else {
                    Ok(None)
                }
            }
            (StaticIslandResumeNode::Text, StaticIslandTemplateNode::Text { value }) => {
                let id = plan
                    .nodes
                    .iter()
                    .find(|node| node.path == path && node.live)
                    .map(|node| node.id)
                    .ok_or_else(|| {
                        WebCommandError::failure(format!(
                            "fresh client region `{island_key}` live text has no authenticated identity"
                        ))
                    })?;
                Ok(Some(json!({"kind": "text", "node": id, "text": value})))
            }
            (StaticIslandResumeNode::Frame { .. }, StaticIslandTemplateNode::Frame { .. }) => {
                let id = plan
                    .nodes
                    .iter()
                    .find(|node| node.path == path && node.live)
                    .map(|node| node.id)
                    .ok_or_else(|| {
                        WebCommandError::failure(format!(
                            "fresh client region `{island_key}` live frame has no authenticated identity"
                        ))
                    })?;
                Ok(Some(json!({
                    "kind": "element",
                    "tag": "iframe",
                    "node": id,
                    "attributes": {
                        "sandbox": "allow-scripts",
                        "class": "glamour-compartment",
                    },
                    "children": [],
                })))
            }
            (
                StaticIslandResumeNode::Element { children: resume_children, .. },
                StaticIslandTemplateNode::Element { tag, attributes, children },
            ) => {
                if resume_children.len() != children.len() {
                    return Err(WebCommandError::failure(format!(
                        "fresh client region `{island_key}` live template changed child shape"
                    )));
                }
                let id = plan
                    .nodes
                    .iter()
                    .find(|node| node.path == path && node.live)
                    .map(|node| node.id)
                    .ok_or_else(|| {
                        WebCommandError::failure(format!(
                            "fresh client region `{island_key}` live element has no authenticated identity"
                        ))
                    })?;
                let static_attributes = compile_static_island_template_attributes(attributes);
                let mut live_children = Vec::new();
                for (index, (resume_child, template_child)) in
                    resume_children.iter().zip(children).enumerate()
                {
                    let mut child_path = path.to_vec();
                    child_path.push(u32::try_from(index).map_err(|_| {
                        WebCommandError::failure(format!(
                            "fresh client region `{island_key}` has too many children"
                        ))
                    })?);
                    if let Some(child) =
                        live_node(island_key, resume_child, template_child, &child_path, plan)?
                    {
                        live_children.push(child);
                    }
                }
                Ok(Some(json!({
                    "kind": "element",
                    "tag": tag,
                    "node": id,
                    "attributes": static_attributes,
                    "children": live_children,
                })))
            }
            _ => Err(WebCommandError::failure(format!(
                "fresh client region `{island_key}` live and template graphs disagree"
            ))),
        }
    }

    live_node(island_key, resume, template, path, plan)?.ok_or_else(|| {
        WebCommandError::failure(format!(
            "fresh client region `{island_key}` requires one live root node"
        ))
    })
}

fn compile_static_island_template_attributes(
    attributes: &[StaticIslandTemplateAttribute],
) -> serde_json::Map<String, Value> {
    let mut static_attributes = serde_json::Map::new();
    let mut custom_properties = Vec::new();
    for attribute in attributes {
        if attribute.kind.starts_with("css-") {
            custom_properties.push(format!("{}:{}", attribute.name, attribute.value));
        } else if attribute.kind != "boolean" || attribute.enabled {
            static_attributes.insert(
                attribute.name.clone(),
                Value::String(attribute.value.clone()),
            );
        }
    }
    if !custom_properties.is_empty() {
        static_attributes.insert(
            "style".into(),
            Value::String(custom_properties.join(";")),
        );
    }
    static_attributes
}

fn compile_static_island_live_template_regions(
    plan: &StaticIslandCompiledPlan,
    root_path: &[u32],
) -> Value {
    let mut regions = serde_json::Map::new();
    for region in plan
        .regions
        .iter()
        .filter(|region| region.live && region.path.starts_with(root_path))
    {
        regions.insert(
            region.id.to_string(),
            json!({
                "parent": region.parent,
                "kind": region.kind,
                "before": region.before,
                "template": region.child.as_ref().map(|child| child.template).unwrap_or(0),
                "dynamicTemplate": region.dynamic.as_ref().map(|key| key.template).unwrap_or(0),
                "keys": region.keys.iter().map(|key| json!({
                    "key": key.id,
                    "source": key.source,
                    "root": key.root,
                    "nodes": key.nodes,
                })).collect::<Vec<_>>(),
                "child": region.child.as_ref().filter(|child| child.mounted).map(|child| json!({
                    "root": child.root,
                    "nodes": child.nodes,
                })),
            }),
        );
    }
    Value::Object(regions)
}

fn island_attribute_category(kind: &str) -> &str {
    if kind.starts_with("css-") {
        return "custom-property";
    }
    match kind {
        "property" => "property",
        "aria" => "aria",
        "class" => "class",
        "attribute" | "boolean" | "url" | "prop" => "attribute",
        _ => unreachable!("checked resume attribute kind"),
    }
}

fn witchy_source_string(value: &str) -> String {
    serde_json::to_string(value).expect("UTF-8 string has a Witchy-compatible literal")
}

fn static_island_template_slot_source(slot: &StaticIslandTemplateSlotRecord) -> String {
    let path = slot
        .path
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match slot.index {
        Some(index) => format!(
            "glamour.island_template_attribute_slot({}, [{path}], {index}, {}, {})",
            slot.id,
            witchy_source_string(&slot.source_kind),
            witchy_source_string(&slot.name),
        ),
        None => format!("glamour.island_template_text_slot({}, [{path}])", slot.id),
    }
}

fn island_sink_registry(attributes: &[StaticIslandAttributeRecord], category: &str) -> Vec<Value> {
    attributes
        .iter()
        .filter(|attribute| island_attribute_category(&attribute.kind) == category)
        .map(|attribute| (attribute.id, attribute.name.clone()))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .map(|(id, name)| json!({"id": id, "name": name}))
        .collect()
}

fn island_custom_property_registry(attributes: &[StaticIslandAttributeRecord]) -> Vec<Value> {
    attributes
        .iter()
        .filter_map(|attribute| {
            attribute.kind.strip_prefix("css-").map(|category| {
                (
                    attribute.id,
                    (attribute.name.clone(), category.to_string()),
                )
            })
        })
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .map(|(id, (name, category))| json!({"id": id, "name": name, "category": category}))
        .collect()
}

fn checked_wire_id(
    island: &str,
    category: &str,
    material: &str,
    assigned: &mut BTreeMap<u32, String>,
) -> Result<u32, WebCommandError> {
    let digest = Sha256::digest(material.as_bytes());
    let id = u32::from_be_bytes(digest[..4].try_into().expect("SHA-256 prefix"));
    if id == 0 {
        return Err(WebCommandError::failure(format!(
            "static island `{island}` produces reserved {category} identity zero"
        )));
    }
    if let Some(existing) = assigned.insert(id, material.to_string()) {
        if existing != material {
            return Err(WebCommandError::failure(format!(
                "static island `{island}` has a {category} identity collision at {id}"
            )));
        }
    }
    Ok(id)
}

pub(super) fn evaluate_static_site(
    project: &Project,
) -> Result<EvaluatedStaticSite, WebCommandError> {
    let (checked, _) = crate::link_file_checked(path_text(&project.entry)?)
        .map_err(WebCommandError::failure)?;
    let entry = static_entry_name(project)?;
    authenticate_static_entry(&checked, &entry, project.content.is_some())?;
    let (arguments, _content_inputs) = static_content_arguments(project)?;
    let evaluation = witchy_lower::codegen::checked_glamour_static_evaluation_module(&checked)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let value = witchy_interp::interpreter::evaluate_compiler_module(
        &evaluation,
        &entry,
        arguments,
    )
    .map_err(|error| WebCommandError::failure(error.message))?;
    static_site_from_value(value)
}

fn static_entry_name(project: &Project) -> Result<String, WebCommandError> {
    let module = project
        .entry
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| WebCommandError::failure("static web entry filename is not UTF-8"))?;
    Ok(format!("{module}.web"))
}

fn authenticate_static_entry(
    checked: &witchy_interp::pipeline::CheckedModule,
    entry: &str,
    has_content: bool,
) -> Result<(), WebCommandError> {
    use witchy_syntax::ast::{Item, Type};

    let function = checked
        .module()
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == entry => Some(function),
            _ => None,
        })
        .ok_or_else(|| {
            WebCommandError::failure(format!(
                "static entry must export `pub fn {entry}() -> glamour.Site`"
            ))
        })?;
    let expected_parameters = usize::from(has_content);
    if !function.public || function.params.len() != expected_parameters {
        return Err(WebCommandError::failure(format!(
            "static entry must be `pub fn {entry}() -> glamour.Site` or, with web.content, `pub fn {entry}(content: glamour.StaticContent) -> glamour.Site`"
        )));
    }
    let Some(Type::Named(return_name, arguments)) = &function.ret else {
        return Err(WebCommandError::failure(
            "static `web()` must return the authenticated `glamour.Site` type",
        ));
    };
    if !arguments.is_empty() {
        return Err(WebCommandError::failure(
            "static `web()` returned an unsupported generic Site shape",
        ));
    }
    let catalog = checked
        .runtime_declaration_catalog()
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if !is_toolchain_glamour_type(&catalog, return_name, "Site") {
        return Err(WebCommandError::failure(format!(
            "static `web()` must return toolchain `glamour.Site`, not `{return_name}`"
        )));
    }
    if let Some(parameter) = function.params.first() {
        let Some(Type::Named(name, arguments)) = &parameter.ty else {
            return Err(WebCommandError::failure(
                "static content entry parameter must be toolchain `glamour.StaticContent`",
            ));
        };
        if !arguments.is_empty()
            || !is_toolchain_glamour_type(&catalog, name, "StaticContent")
        {
            return Err(WebCommandError::failure(format!(
                "static content entry parameter must be toolchain `glamour.StaticContent`, not `{name}`"
            )));
        }
    }
    Ok(())
}

fn is_toolchain_glamour_type(
    catalog: &witchy_types::runtime_type::RuntimeDeclarationCatalog,
    name: &str,
    expected: &str,
) -> bool {
    use witchy_types::runtime_type::{DeclarationKind, PackageSource};

    catalog
        .resolve(name, DeclarationKind::Type)
        .is_some_and(|identity| {
            identity.package().source() == &PackageSource::Toolchain
                && identity.package().name() == "witchy/glamour"
                && identity.module() == ["src", "glamour"]
                && identity.name() == expected
        })
}

fn static_content_arguments(
    project: &Project,
) -> Result<(Vec<witchy_interp::interpreter::CompilerValue>, Vec<ArtifactRecord>), WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let Some(root) = &project.content else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut files = Vec::new();
    collect_static_content(root, root, &mut files)?;
    if files.len() > MAX_STATIC_CONTENT_FILES {
        return Err(WebCommandError::failure(format!(
            "web content contains {} files; the limit is {MAX_STATIC_CONTENT_FILES}",
            files.len()
        )));
    }
    let mut total = 0usize;
    let mut values = Vec::with_capacity(files.len());
    let mut records = Vec::with_capacity(files.len());
    for path in files {
        let relative = path.strip_prefix(root).map_err(|error| {
            WebCommandError::failure(format!("cannot relativize static content: {error}"))
        })?;
        let relative = slash_path(relative);
        if relative.is_empty() {
            return Err(WebCommandError::failure(
                "static content contains an empty relative path",
            ));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot read static content `{}`: {error}",
                path.display()
            ))
        })?;
        if bytes.len() > MAX_STATIC_CONTENT_FILE_BYTES {
            return Err(WebCommandError::failure(format!(
                "static content `{relative}` is {} bytes; the per-file limit is {MAX_STATIC_CONTENT_FILE_BYTES}",
                bytes.len()
            )));
        }
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            WebCommandError::failure("static content byte count overflowed")
        })?;
        if total > MAX_STATIC_CONTENT_BYTES {
            return Err(WebCommandError::failure(format!(
                "static content is {total} bytes; the total limit is {MAX_STATIC_CONTENT_BYTES}"
            )));
        }
        let text = String::from_utf8(bytes.clone()).map_err(|_| {
            WebCommandError::failure(format!(
                "static content `{relative}` is not UTF-8 text"
            ))
        })?;
        records.push(ArtifactRecord {
            path: relative.clone(),
            bytes: bytes.len(),
            sha256: sha256(&bytes),
        });
        values.push(CompilerValue::Constructor {
            name: "glamour.StaticContentFile".into(),
            fields: vec![
                CompilerValue::String(relative),
                CompilerValue::String(text),
            ],
        });
    }
    Ok((
        vec![CompilerValue::Constructor {
            name: "glamour.StaticContent".into(),
            fields: vec![CompilerValue::List(values)],
        }],
        records,
    ))
}

fn collect_static_content(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), WebCommandError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| {
            WebCommandError::failure(format!(
                "cannot list static content `{}`: {error}",
                directory.display()
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot inspect static content `{}`: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            return Err(WebCommandError::failure(format!(
                "static content `{}` is a symlink; declared inputs must be direct files",
                relative.display()
            )));
        }
        if metadata.is_dir() {
            collect_static_content(root, &path, files)?;
        } else if metadata.is_file() {
            files.push(path);
            if files.len() > MAX_STATIC_CONTENT_FILES {
                return Err(WebCommandError::failure(format!(
                    "web content exceeds the {MAX_STATIC_CONTENT_FILES}-file limit"
                )));
            }
        } else {
            return Err(WebCommandError::failure(format!(
                "static content `{}` is not a regular file",
                path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn static_site_from_value(
    value: witchy_interp::interpreter::CompilerValue,
) -> Result<EvaluatedStaticSite, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let CompilerValue::Constructor { name, fields } = value else {
        return Err(WebCommandError::failure(
            "static `web()` must return `glamour.Site`",
        ));
    };
    if name != "glamour.Site" || fields.len() != 6 {
        return Err(WebCommandError::failure(format!(
            "static `web()` returned `{name}` instead of the authenticated `glamour.Site` shape"
        )));
    }
    let mut fields = fields.into_iter();
    let Some(CompilerValue::List(values)) = fields.next() else {
        return Err(WebCommandError::failure(
            "authenticated `glamour.Site` contains an invalid page registry",
        ));
    };
    let Some(CompilerValue::List(action_values)) = fields.next() else {
        return Err(WebCommandError::failure(
            "authenticated `glamour.Site` contains an invalid action registry",
        ));
    };
    let Some(CompilerValue::List(style_values)) = fields.next() else {
        return Err(WebCommandError::failure(
            "authenticated `glamour.Site` contains an invalid style registry",
        ));
    };
    let Some(CompilerValue::List(preload_values)) = fields.next() else {
        return Err(WebCommandError::failure(
            "authenticated `glamour.Site` contains an invalid preload registry",
        ));
    };
    let Some(CompilerValue::List(asset_values)) = fields.next() else {
        return Err(WebCommandError::failure(
            "authenticated `glamour.Site` contains an invalid asset registry",
        ));
    };
    let Some(CompilerValue::List(island_values)) = fields.next() else {
        return Err(WebCommandError::failure(
            "authenticated `glamour.Site` contains an invalid island registry",
        ));
    };
    let mut pages = Vec::with_capacity(values.len());
    let mut routes = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for value in values {
        let CompilerValue::Constructor { name, fields } = value else {
            return Err(WebCommandError::failure(
                "authenticated `glamour.Site` contains a non-page value",
            ));
        };
        if name != "glamour.StaticPage" || fields.len() != 4 {
            return Err(WebCommandError::failure(format!(
                "authenticated `glamour.Site` contains `{name}` instead of `glamour.StaticPage`"
            )));
        }
        let mut fields = fields.into_iter();
        let Some(CompilerValue::String(route)) = fields.next() else {
            return Err(WebCommandError::failure(
                "static page route is not a String",
            ));
        };
        let Some(CompilerValue::String(html)) = fields.next() else {
            return Err(WebCommandError::failure(
                "static page HTML is not a String",
            ));
        };
        let Some(CompilerValue::List(island_key_values)) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` has an invalid authenticated island placement registry"
            )));
        };
        let island_keys = island_key_values
            .into_iter()
            .map(|value| match value {
                CompilerValue::String(key) if valid_island_key(&key) => Ok(key),
                _ => Err(WebCommandError::failure(format!(
                    "static route `{route}` has an invalid authenticated island placement"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some(CompilerValue::List(interactive_values)) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` has an invalid interactive plan registry"
            )));
        };
        let interactive_keys = island_keys
            .iter()
            .filter(|key| key.starts_with("interactive-"))
            .count();
        if interactive_values.len() != interactive_keys {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` has {interactive_keys} interactive placement(s) but {} embedded plan(s)",
                interactive_values.len()
            )));
        }
        let output = static_route_output(&route)?;
        if !routes.insert(route.clone()) {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` is declared more than once"
            )));
        }
        if !outputs.insert(output.clone()) {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` collides with another output path"
            )));
        }
        if !html.starts_with("<!doctype html>") {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` did not use Glamour's document renderer"
            )));
        }
        validate_static_document(&route, &html)?;
        pages.push(StaticPage {
            route,
            output,
            html,
            island_keys,
        });
    }
    pages.sort_by(|left, right| left.route.cmp(&right.route));
    let actions = static_actions_from_values(action_values)?;
    validate_action_bindings(&pages, &actions)?;
    let styles = static_styles_from_values(style_values, &pages)?;
    let preloads = static_preloads_from_values(preload_values, &pages)?;
    let assets = resources::static_assets_from_values(asset_values)?;
    validate_css_asset_bindings(&styles, &assets)?;
    let islands = static_islands_from_values(island_values, &pages)?;
    Ok((pages, actions, styles, preloads, assets, islands))
}

fn static_islands_from_values(
    values: Vec<witchy_interp::interpreter::CompilerValue>,
    pages: &[StaticPage],
) -> Result<Vec<StaticIsland>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let mut islands = Vec::with_capacity(values.len());
    let mut keys = BTreeSet::new();
    for value in values {
        let CompilerValue::Constructor { name, fields } = value else {
            return Err(WebCommandError::failure(
                "static island registry contains a non-island value",
            ));
        };
        if name != "glamour.IslandPlan" || fields.len() != 7 {
            return Err(WebCommandError::failure(format!(
                "static island registry contains `{name}` instead of `glamour.IslandPlan`"
            )));
        }
        let mut fields = fields.into_iter();
        let Some(CompilerValue::String(key)) = fields.next() else {
            return Err(WebCommandError::failure("static island key is not text"));
        };
        if !valid_island_key(&key) || !keys.insert(key.clone()) {
            return Err(WebCommandError::failure(format!(
                "static island has invalid or duplicate key `{key}`"
            )));
        }
        let Some(CompilerValue::String(source_identity)) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` compiler origin is not text"
            )));
        };
        if !valid_island_source_identity(&key, &source_identity) {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` has invalid compiler origin `{source_identity}`"
            )));
        }
        let Some(activation_value) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` has no activation policy"
            )));
        };
        let (activation, media) = static_island_activation(&key, activation_value)?;
        let Some(prefetch_value) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` has no prefetch policy"
            )));
        };
        let (prefetch, prefetch_media) = static_island_prefetch(&key, prefetch_value)?;
        let Some(name_value) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` has no diagnostic-name field"
            )));
        };
        let diagnostic_name = static_island_diagnostic_name(&key, name_value)?;
        let Some(CompilerValue::Constructor {
            name: start_name,
            fields: start_fields,
        }) = fields.next()
        else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` has no closed start mode"
            )));
        };
        let (mode, state) = match (start_name.as_str(), start_fields.as_slice()) {
            ("glamour.Resumable", [CompilerValue::String(state)]) => {
                if state.len() > 1024 * 1024 || serde_json::from_str::<Value>(state).is_err() {
                    return Err(WebCommandError::failure(format!(
                        "static island `{key}` has invalid or oversized public state"
                    )));
                }
                ("resume".to_string(), Some(state.clone()))
            }
            ("glamour.Fresh", []) => ("fresh".to_string(), None),
            _ => {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` has unsupported start mode `{start_name}`"
                )))
            }
        };
        let Some(CompilerValue::Constructor { name, fields }) = fields.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` has invalid rendered markup"
            )));
        };
        if name != "glamour.IslandMarkup" || fields.len() != 4 {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` contains `{name}` instead of authenticated Glamour markup"
            )));
        }
        let mut markup = fields.into_iter();
        let Some(CompilerValue::String(markup_key)) = markup.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` markup key is not text"
            )));
        };
        let Some(CompilerValue::String(html)) = markup.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` markup is not text"
            )));
        };
        let Some(CompilerValue::String(resume_json)) = markup.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` resume graph is not text"
            )));
        };
        let Some(CompilerValue::String(template_json)) = markup.next() else {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` template graph is not text"
            )));
        };
        if markup_key != key {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` disagrees with its rendered markup key `{markup_key}`"
            )));
        }
        validate_static_fragment(&key, &html)?;
        let resume = static_island_resume(&key, &resume_json)?;
        let template = static_island_template(&key, &template_json)?;
        validate_static_island_template(&key, &resume, &template)?;
        let marker = format!("<div data-glamour-island-key=\"{key}\">{html}</div>");
        let authenticated_uses = pages
            .iter()
            .map(|page| page.island_keys.iter().filter(|candidate| *candidate == &key).count())
            .sum::<usize>();
        let rendered_uses = pages
            .iter()
            .map(|page| page.html.matches(&marker).count())
            .sum::<usize>();
        if authenticated_uses != 1 || rendered_uses != 1 {
            return Err(WebCommandError::failure(format!(
                "static island `{key}` must occur exactly once through `glamour.island_node`; found {authenticated_uses} authenticated and {rendered_uses} rendered placements"
            )));
        }
        islands.push(StaticIsland {
            key,
            source_identity,
            mode,
            activation,
            media,
            prefetch,
            prefetch_media,
            diagnostic_name,
            state,
            html,
            resume,
            template,
        });
    }
    if let Some(undeclared) = pages
        .iter()
        .flat_map(|page| page.island_keys.iter())
        .find(|key| !keys.contains(*key))
    {
        return Err(WebCommandError::failure(format!(
            "static island placement `{undeclared}` is absent from the Site island registry"
        )));
    }
    islands.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(islands)
}

fn static_island_resume(
    key: &str,
    encoded: &str,
) -> Result<StaticIslandResumeNode, WebCommandError> {
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` resume graph is oversized"
        )));
    }
    let value: Value = serde_json::from_str(encoded).map_err(|_| {
        WebCommandError::failure(format!("static island `{key}` resume graph is invalid JSON"))
    })?;
    let mut nodes = 0_usize;
    let mut event_ids = BTreeSet::new();
    static_island_resume_node(key, value, 0, &mut nodes, &mut event_ids)
}

fn static_island_template(
    key: &str,
    encoded: &str,
) -> Result<StaticIslandTemplateNode, WebCommandError> {
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` template graph is oversized"
        )));
    }
    let value: Value = serde_json::from_str(encoded).map_err(|_| {
        WebCommandError::failure(format!(
            "static island `{key}` template graph is invalid JSON"
        ))
    })?;
    let mut nodes = 0_usize;
    static_island_template_node(key, value, 0, &mut nodes)
}

fn static_island_template_node(
    key: &str,
    value: Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<StaticIslandTemplateNode, WebCommandError> {
    if depth > 64 || *nodes >= 100_000 {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` template graph exceeds its structural limit"
        )));
    }
    *nodes += 1;
    let Value::Object(mut object) = value else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` template node is not an object"
        )));
    };
    let Some(Value::String(kind)) = object.remove("kind") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` template node has no kind"
        )));
    };
    match kind.as_str() {
        "text" => {
            let Some(Value::String(value)) = object.remove("value") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template text has no value"
                )));
            };
            if !object.is_empty() || value.len() > 1024 * 1024 {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template text is malformed"
                )));
            }
            Ok(StaticIslandTemplateNode::Text { value })
        }
        "frame" => {
            let StaticIslandFrameFields {
                renderer,
                max_grant_bytes,
                max_event_bytes,
                grant,
                fallback,
                id,
                decoder,
            } = static_island_frame_fields(key, &mut object)?;
            if !object.is_empty() {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` frame template is malformed"
                )));
            }
            Ok(StaticIslandTemplateNode::Frame {
                renderer,
                max_grant_bytes,
                max_event_bytes,
                grant,
                fallback,
                id,
                decoder,
            })
        }
        "keyed" => {
            let Some(Value::String(item_key)) = object.remove("key") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` keyed template node has no key"
                )));
            };
            let Some(child) = object.remove("child") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` keyed template node has no child"
                )));
            };
            if !object.is_empty() || item_key.is_empty() || item_key.len() > 1024 {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` keyed template node is malformed"
                )));
            }
            Ok(StaticIslandTemplateNode::Keyed {
                key: item_key,
                child: Box::new(static_island_template_node(
                    key,
                    child,
                    depth + 1,
                    nodes,
                )?),
            })
        }
        "branch" => {
            let Some(Value::String(id)) = object.remove("id") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch template node has no identity"
                )));
            };
            let Some(child) = object.remove("child") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch template node has no child"
                )));
            };
            if !object.is_empty() || !valid_island_key(&id) {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch template node is malformed"
                )));
            }
            Ok(StaticIslandTemplateNode::Branch {
                id,
                child: Box::new(static_island_template_node(
                    key,
                    child,
                    depth + 1,
                    nodes,
                )?),
            })
        }
        "child" => {
            let Some(Value::String(id)) = object.remove("id") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child template node has no identity"
                )));
            };
            let Some(child) = object.remove("child") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child template node has no child field"
                )));
            };
            if child.is_null() || !object.is_empty() || !valid_island_key(&id) {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child template node is malformed"
                )));
            }
            let child = Box::new(static_island_template_node(
                key,
                child,
                depth + 1,
                nodes,
            )?);
            Ok(StaticIslandTemplateNode::Child { id, child })
        }
        "element" => {
            let Some(Value::String(tag)) = object.remove("tag") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template element has no tag"
                )));
            };
            let Some(Value::Array(attribute_values)) = object.remove("attributes") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template element has no attribute list"
                )));
            };
            let Some(Value::Array(child_values)) = object.remove("children") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template element has no child list"
                )));
            };
            if !object.is_empty()
                || !valid_resume_name(&tag)
                || attribute_values.len() > 256
                || child_values.len() > 100_000
            {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template element is malformed"
                )));
            }
            let attributes = attribute_values
                .into_iter()
                .map(|value| static_island_template_attribute(key, value))
                .collect::<Result<Vec<_>, _>>()?;
            let mut attribute_identities = BTreeSet::new();
            if attributes
                .iter()
                .any(|attribute| !attribute_identities.insert(attribute.name.clone()))
            {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` template element has duplicate attributes"
                )));
            }
            let children = child_values
                .into_iter()
                .map(|value| static_island_template_node(key, value, depth + 1, nodes))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(StaticIslandTemplateNode::Element {
                tag,
                attributes,
                children,
            })
        }
        _ => Err(WebCommandError::failure(format!(
            "static island `{key}` template node kind `{kind}` is unsupported"
        ))),
    }
}

struct StaticIslandFrameFields {
    renderer: String,
    max_grant_bytes: usize,
    max_event_bytes: usize,
    grant: String,
    fallback: String,
    id: String,
    decoder: String,
}

fn static_island_frame_fields(
    key: &str,
    object: &mut serde_json::Map<String, Value>,
) -> Result<StaticIslandFrameFields, WebCommandError> {
    let Some(Value::String(renderer)) = object.remove("renderer") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` frame has no renderer"
        )));
    };
    let max_grant_bytes = object
        .remove("maxGrantBytes")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok());
    let max_event_bytes = object
        .remove("maxEventBytes")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok());
    let Some(Value::String(grant)) = object.remove("grant") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` frame has no typed grant"
        )));
    };
    let Some(Value::String(fallback)) = object.remove("fallback") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` frame has no static fallback"
        )));
    };
    let Some(Value::String(id)) = object.remove("id") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` frame has no event identity"
        )));
    };
    let Some(Value::String(decoder)) = object.remove("decoder") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` frame has no event decoder"
        )));
    };
    if renderer != "document.v1"
        || max_grant_bytes != Some(65_536)
        || max_event_bytes != Some(4_096)
        || grant.len() > 65_536
        || fallback.len() > 65_536
        || !valid_resume_identity(&id)
        || decoder != "value"
    {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` frame exceeds the sealed document renderer contract"
        )));
    }
    validate_static_fragment(key, &fallback)?;
    Ok(StaticIslandFrameFields {
        renderer,
        max_grant_bytes: max_grant_bytes.expect("checked frame grant limit"),
        max_event_bytes: max_event_bytes.expect("checked frame event limit"),
        grant,
        fallback,
        id,
        decoder,
    })
}

fn static_island_template_attribute(
    key: &str,
    value: Value,
) -> Result<StaticIslandTemplateAttribute, WebCommandError> {
    let parsed: StaticIslandTemplateAttributeWire = serde_json::from_value(value).map_err(|_| {
        WebCommandError::failure(format!(
            "static island `{key}` template attribute is malformed"
        ))
    })?;
    if !valid_static_resume_attribute_kind(&parsed.kind)
        || !valid_static_resume_attribute_name(&parsed.kind, &parsed.name)
        || parsed.value.len() > 1024 * 1024
        || (parsed.kind != "boolean" && !parsed.enabled)
        || (parsed.kind.starts_with("css-")
            && !valid_static_css_category_token(&parsed.kind, &parsed.value))
    {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` template attribute is invalid"
        )));
    }
    Ok(StaticIslandTemplateAttribute {
        kind: parsed.kind,
        name: parsed.name,
        value: parsed.value,
        enabled: parsed.enabled,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticIslandTemplateAttributeWire {
    kind: String,
    name: String,
    value: String,
    enabled: bool,
}

fn validate_static_island_template(
    key: &str,
    resume: &StaticIslandResumeNode,
    template: &StaticIslandTemplateNode,
) -> Result<(), WebCommandError> {
    let valid = match (resume, template) {
        (StaticIslandResumeNode::Text, StaticIslandTemplateNode::Text { .. }) => true,
        (
            StaticIslandResumeNode::Frame {
                renderer: resume_renderer,
                max_grant_bytes: resume_max_grant,
                max_event_bytes: resume_max_event,
                grant: resume_grant,
                fallback: resume_fallback,
                id: resume_id,
                decoder: resume_decoder,
            },
            StaticIslandTemplateNode::Frame {
                renderer: template_renderer,
                max_grant_bytes: template_max_grant,
                max_event_bytes: template_max_event,
                grant: template_grant,
                fallback: template_fallback,
                id: template_id,
                decoder: template_decoder,
            },
        ) => {
            resume_renderer == template_renderer
                && resume_max_grant == template_max_grant
                && resume_max_event == template_max_event
                && resume_grant == template_grant
                && resume_fallback == template_fallback
                && resume_id == template_id
                && resume_decoder == template_decoder
        }
        (
            StaticIslandResumeNode::Keyed {
                key: resume_key,
                child: resume_child,
            },
            StaticIslandTemplateNode::Keyed {
                key: template_key,
                child: template_child,
            },
        ) => {
            resume_key == template_key
                && validate_static_island_template(key, resume_child, template_child).is_ok()
        }
        (
            StaticIslandResumeNode::Branch {
                id: resume_id,
                child: resume_child,
                ..
            },
            StaticIslandTemplateNode::Branch {
                id: template_id,
                child: template_child,
            },
        ) => {
            resume_id == template_id
                && validate_static_island_template(key, resume_child, template_child).is_ok()
        }
        (
            StaticIslandResumeNode::Child {
                id: resume_id,
                child: resume_child,
                ..
            },
            StaticIslandTemplateNode::Child {
                id: template_id,
                child: template_child,
            },
        ) => {
            resume_id == template_id
                && validate_static_island_template(key, resume_child, template_child).is_ok()
        }
        (
            StaticIslandResumeNode::Element {
                tag: resume_tag,
                attributes: resume_attributes,
                children: resume_children,
                ..
            },
            StaticIslandTemplateNode::Element {
                tag: template_tag,
                attributes: template_attributes,
                children: template_children,
            },
        ) => {
            resume_tag == template_tag
                && resume_attributes.len() == template_attributes.len()
                && resume_attributes.iter().zip(template_attributes).all(
                    |(resume_attribute, template_attribute)| {
                        resume_attribute.kind == template_attribute.kind
                            && resume_attribute.name == template_attribute.name
                    },
                )
                && resume_children.len() == template_children.len()
                && resume_children
                    .iter()
                    .zip(template_children)
                    .all(|(resume_child, template_child)| {
                        validate_static_island_template(key, resume_child, template_child).is_ok()
                    })
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(WebCommandError::failure(format!(
            "static island `{key}` template graph disagrees with its authenticated resume graph"
        )))
    }
}

fn static_island_resume_node(
    key: &str,
    value: Value,
    depth: usize,
    nodes: &mut usize,
    event_ids: &mut BTreeSet<String>,
) -> Result<StaticIslandResumeNode, WebCommandError> {
    if depth > 64 || *nodes >= 100_000 {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` resume graph exceeds its structural limit"
        )));
    }
    *nodes += 1;
    let Value::Object(mut object) = value else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` resume node is not an object"
        )));
    };
    let Some(Value::String(kind)) = object.remove("kind") else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` resume node has no kind"
        )));
    };
    match kind.as_str() {
        "text" if object.is_empty() => Ok(StaticIslandResumeNode::Text),
        "frame" => {
            let StaticIslandFrameFields {
                renderer,
                max_grant_bytes,
                max_event_bytes,
                grant,
                fallback,
                id,
                decoder,
            } = static_island_frame_fields(key, &mut object)?;
            if !object.is_empty() || !event_ids.insert(id.clone()) {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` frame resume node is malformed or repeats its event identity"
                )));
            }
            Ok(StaticIslandResumeNode::Frame {
                renderer,
                max_grant_bytes,
                max_event_bytes,
                grant,
                fallback,
                id,
                decoder,
            })
        }
        "keyed" => {
            let Some(Value::String(item_key)) = object.remove("key") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` keyed resume node has no key"
                )));
            };
            let Some(child) = object.remove("child") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` keyed resume node has no child"
                )));
            };
            if !object.is_empty() || item_key.is_empty() || item_key.len() > 1024 {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` keyed resume node is malformed"
                )));
            }
            Ok(StaticIslandResumeNode::Keyed {
                key: item_key,
                child: Box::new(static_island_resume_node(
                    key,
                    child,
                    depth + 1,
                    nodes,
                    event_ids,
                )?),
            })
        }
        "branch" => {
            let Some(Value::String(id)) = object.remove("id") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch resume node has no identity"
                )));
            };
            let Some(Value::Bool(active)) = object.remove("active") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch resume node has no active state"
                )));
            };
            let Some(child) = object.remove("child") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch resume node has no child"
                )));
            };
            if !object.is_empty() || !valid_island_key(&id) {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` branch resume node is malformed"
                )));
            }
            Ok(StaticIslandResumeNode::Branch {
                id,
                active,
                child: Box::new(static_island_resume_node(
                    key,
                    child,
                    depth + 1,
                    nodes,
                    event_ids,
                )?),
            })
        }
        "child" => {
            let Some(Value::String(id)) = object.remove("id") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child resume node has no identity"
                )));
            };
            let Some(child) = object.remove("child") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child resume node has no child field"
                )));
            };
            let Some(Value::Bool(mounted)) = object.remove("mounted") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child resume node has no mounted state"
                )));
            };
            if child.is_null() || !object.is_empty() || !valid_island_key(&id) {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` optional-child resume node is malformed"
                )));
            }
            let child = Box::new(static_island_resume_node(
                key,
                child,
                depth + 1,
                nodes,
                event_ids,
            )?);
            Ok(StaticIslandResumeNode::Child { id, mounted, child })
        }
        "element" => {
            let Some(Value::String(tag)) = object.remove("tag") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` resume element has no tag"
                )));
            };
            let Some(Value::Array(event_values)) = object.remove("events") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` resume element has no event list"
                )));
            };
            let Some(Value::Array(attribute_values)) = object.remove("attributes") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` resume element has no attribute list"
                )));
            };
            let Some(Value::Array(child_values)) = object.remove("children") else {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` resume element has no child list"
                )));
            };
            if !object.is_empty()
                || !valid_resume_name(&tag)
                || attribute_values.len() > 256
                || event_values.len() > 64
                || child_values.len() > 100_000
            {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` resume element is malformed"
                )));
            }
            let attributes = attribute_values
                .into_iter()
                .map(|value| static_island_resume_attribute(key, value))
                .collect::<Result<Vec<_>, _>>()?;
            let mut attribute_identities = BTreeSet::new();
            if attributes.iter().any(|attribute| {
                !attribute_identities.insert(attribute.name.clone())
            }) {
                return Err(WebCommandError::failure(format!(
                    "static island `{key}` resume element has duplicate attributes"
                )));
            }
            let events = event_values
                .into_iter()
                .map(|value| static_island_resume_event(key, value, event_ids))
                .collect::<Result<Vec<_>, _>>()?;
            let children = child_values
                .into_iter()
                .map(|value| {
                    static_island_resume_node(
                        key,
                        value,
                        depth + 1,
                        nodes,
                        event_ids,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(StaticIslandResumeNode::Element {
                tag,
                attributes,
                events,
                children,
            })
        }
        _ => Err(WebCommandError::failure(format!(
            "static island `{key}` resume node kind `{kind}` is unsupported"
        ))),
    }
}

fn static_island_resume_attribute(
    key: &str,
    value: Value,
) -> Result<StaticIslandResumeAttribute, WebCommandError> {
    let parsed: StaticIslandResumeAttributeWire = serde_json::from_value(value).map_err(|_| {
        WebCommandError::failure(format!(
            "static island `{key}` resume attribute is malformed"
        ))
    })?;
    if !valid_static_resume_attribute_kind(&parsed.kind)
        || !valid_static_resume_attribute_name(&parsed.kind, &parsed.name)
    {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` resume attribute is invalid"
        )));
    }
    Ok(StaticIslandResumeAttribute {
        kind: parsed.kind,
        name: parsed.name,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StaticIslandResumeAttributeWire {
    kind: String,
    name: String,
}

fn valid_static_resume_attribute_name(kind: &str, name: &str) -> bool {
    if kind.starts_with("css-") {
        let Some(local) = name.strip_prefix("--glamour-") else {
            return false;
        };
        return !local.is_empty()
            && local
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && local
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    }
    let valid = name.bytes().next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
        });
    let url = matches!(
        name,
        "href" | "src" | "action" | "poster" | "xlink:href"
    );
    valid
        && !name.starts_with("on")
        && !matches!(
            name,
            "srcdoc" | "formaction" | "style" | "data-glamour-events"
        )
        && (kind != "class" || name == "class")
        && (kind != "aria" || name.starts_with("aria-"))
        && (kind != "property" || matches!(name, "value" | "checked" | "selected"))
        && ((kind == "url") == url)
}

fn valid_static_resume_attribute_kind(kind: &str) -> bool {
    matches!(
        kind,
        "attribute"
            | "boolean"
            | "property"
            | "url"
            | "class"
            | "aria"
            | "prop"
            | "css-color"
            | "css-length"
            | "css-number"
            | "css-percentage"
            | "css-angle"
            | "css-time"
    )
}

fn static_island_resume_event(
    key: &str,
    value: Value,
    event_ids: &mut BTreeSet<String>,
) -> Result<StaticIslandResumeEvent, WebCommandError> {
    let parsed: StaticIslandResumeEventWire = serde_json::from_value(value).map_err(|_| {
        WebCommandError::failure(format!("static island `{key}` resume event is malformed"))
    })?;
    if !valid_resume_identity(&parsed.id)
        || !event_ids.insert(parsed.id.clone())
        || !valid_resume_name(&parsed.event)
        || !matches!(parsed.kind.as_str(), "msg" | "value" | "checked" | "key")
    {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` resume event is invalid or duplicated"
        )));
    }
    Ok(StaticIslandResumeEvent {
        id: parsed.id,
        event: parsed.event,
        kind: parsed.kind,
        prevent_default: parsed.prevent_default,
        stop_propagation: parsed.stop_propagation,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StaticIslandResumeEventWire {
    id: String,
    event: String,
    kind: String,
    prevent_default: bool,
    stop_propagation: bool,
}

fn valid_resume_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_resume_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
}

fn static_island_activation(
    key: &str,
    value: witchy_interp::interpreter::CompilerValue,
) -> Result<(String, Option<String>), WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let CompilerValue::Constructor { name, fields } = value else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` activation policy is not a Glamour constructor"
        )));
    };
    match (name.as_str(), fields.as_slice()) {
        ("glamour.OnLoad", []) => Ok(("load".into(), None)),
        ("glamour.OnIdle", []) => Ok(("idle".into(), None)),
        ("glamour.OnVisible", []) => Ok(("visible".into(), None)),
        ("glamour.OnInteraction", []) => Ok(("interaction".into(), None)),
        ("glamour.OnMedia", [query]) => Ok((
            "media".into(),
            Some(static_media_query_value(key, "activation", query)?),
        )),
        _ => Err(WebCommandError::failure(format!(
            "static island `{key}` has an invalid activation policy `{name}`"
        ))),
    }
}

fn static_island_prefetch(
    key: &str,
    value: witchy_interp::interpreter::CompilerValue,
) -> Result<(String, Option<String>), WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let CompilerValue::Constructor { name, fields } = value else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` prefetch policy is not a Glamour constructor"
        )));
    };
    match (name.as_str(), fields.as_slice()) {
        ("glamour.NoPrefetch", []) => Ok(("none".into(), None)),
        ("glamour.PrefetchIdle", []) => Ok(("idle".into(), None)),
        ("glamour.PrefetchVisible", []) => Ok(("visible".into(), None)),
        ("glamour.PrefetchIntent", []) => Ok(("intent".into(), None)),
        ("glamour.PrefetchMedia", [query]) => Ok((
            "media".into(),
            Some(static_media_query_value(key, "prefetch", query)?),
        )),
        _ => Err(WebCommandError::failure(format!(
            "static island `{key}` has an invalid prefetch policy `{name}`"
        ))),
    }
}

fn static_media_query_value(
    key: &str,
    policy: &str,
    value: &witchy_interp::interpreter::CompilerValue,
) -> Result<String, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let CompilerValue::Constructor { name, fields } = value else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` {policy} media condition is not sealed"
        )));
    };
    let [CompilerValue::String(query)] = fields.as_slice() else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` {policy} media condition has invalid fields"
        )));
    };
    if name != "glamour.MediaQuery" || !valid_media_query(query) {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` has an invalid {policy} media condition"
        )));
    }
    Ok(query.clone())
}

fn static_island_diagnostic_name(
    key: &str,
    value: witchy_interp::interpreter::CompilerValue,
) -> Result<Option<String>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let CompilerValue::Constructor { name, fields } = value else {
        return Err(WebCommandError::failure(format!(
            "static island `{key}` diagnostic name is not an Option"
        )));
    };
    if name == "None" || name.ends_with(".None") {
        if fields.is_empty() {
            return Ok(None);
        }
    } else if name == "Some" || name.ends_with(".Some") {
        if let [CompilerValue::String(value)] = fields.as_slice() {
            if valid_island_key(value) {
                return Ok(Some(value.clone()));
            }
        }
    }
    Err(WebCommandError::failure(format!(
        "static island `{key}` has an invalid diagnostic name"
    )))
}

fn valid_island_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_island_source_identity(key: &str, value: &str) -> bool {
    if value == format!("low-level:{key}") {
        return true;
    }
    value
        .strip_prefix("interactive-origin1-")
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
}

fn valid_media_query(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 {
        return false;
    }
    let mut depth = 0_i32;
    for byte in value.bytes() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            byte
                if byte.is_ascii_alphanumeric()
                    || matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'-' | b':' | b'.' | b'/' | b'_' | b'%') => {}
            _ => return false,
        }
    }
    depth == 0
}

fn validate_static_fragment(key: &str, html: &str) -> Result<(), WebCommandError> {
    const PREFIX: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body>";
    validate_static_document(key, &format!("{PREFIX}{html}</body></html>"))
}

fn static_actions_from_values(
    values: Vec<witchy_interp::interpreter::CompilerValue>,
) -> Result<Vec<StaticAction>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let mut actions = Vec::with_capacity(values.len());
    let mut action_ids = BTreeSet::new();
    for value in values {
        let CompilerValue::Constructor { name, fields } = value else {
            return Err(WebCommandError::failure(
                "static action registry contains a non-action value",
            ));
        };
        if name != "glamour.StaticAction" || fields.len() != 4 {
            return Err(WebCommandError::failure(format!(
                "static action registry contains `{name}` instead of `glamour.StaticAction`"
            )));
        }
        let mut fields = fields.into_iter();
        let Some(CompilerValue::String(id)) = fields.next() else {
            return Err(WebCommandError::failure("static action id is not text"));
        };
        let Some(CompilerValue::String(method)) = fields.next() else {
            return Err(WebCommandError::failure("static action method is not text"));
        };
        let Some(CompilerValue::String(action)) = fields.next() else {
            return Err(WebCommandError::failure("static action URL is not text"));
        };
        let Some(CompilerValue::List(field_values)) = fields.next() else {
            return Err(WebCommandError::failure(
                "static action fields are not a closed list",
            ));
        };
        if !valid_form_identity(&id) || !action_ids.insert(id.clone()) {
            return Err(WebCommandError::failure(format!(
                "static action has invalid or duplicate identity `{id}`"
            )));
        }
        if !matches!(method.as_str(), "GET" | "POST") {
            return Err(WebCommandError::failure(format!(
                "static action `{id}` has unsupported method `{method}`"
            )));
        }
        if !valid_static_form_url(&action) {
            return Err(WebCommandError::failure(format!(
                "static action `{id}` has an unsafe URL"
            )));
        }
        let mut parsed_fields = Vec::with_capacity(field_values.len());
        let mut field_names = BTreeSet::new();
        for field in field_values {
            let CompilerValue::Constructor { name, fields } = field else {
                return Err(WebCommandError::failure(format!(
                    "static action `{id}` contains a non-field value"
                )));
            };
            if name != "glamour.StaticActionField" || fields.len() != 4 {
                return Err(WebCommandError::failure(format!(
                    "static action `{id}` contains `{name}` instead of `glamour.StaticActionField`"
                )));
            }
            let mut fields = fields.into_iter();
            let Some(CompilerValue::String(name)) = fields.next() else {
                return Err(WebCommandError::failure("static field name is not text"));
            };
            let Some(CompilerValue::String(label)) = fields.next() else {
                return Err(WebCommandError::failure("static field label is not text"));
            };
            let Some(CompilerValue::String(kind)) = fields.next() else {
                return Err(WebCommandError::failure("static field kind is not text"));
            };
            let Some(CompilerValue::Bool(required)) = fields.next() else {
                return Err(WebCommandError::failure(
                    "static field required flag is not boolean",
                ));
            };
            if !valid_static_identifier(&name) || !field_names.insert(name.clone()) {
                return Err(WebCommandError::failure(format!(
                    "static action `{id}` has invalid or duplicate field `{name}`"
                )));
            }
            if !matches!(
                kind.as_str(),
                "text" | "email" | "number" | "checkbox" | "secret"
            ) {
                return Err(WebCommandError::failure(format!(
                    "static action `{id}` field `{name}` has unsupported kind `{kind}`"
                )));
            }
            if method == "GET" && kind == "secret" {
                return Err(WebCommandError::failure(format!(
                    "static action `{id}` secret field `{name}` requires POST"
                )));
            }
            parsed_fields.push(StaticActionField {
                name,
                label,
                kind,
                required,
            });
        }
        actions.push(StaticAction {
            id,
            method,
            action,
            fields: parsed_fields,
            input_schema: 0,
            result_schema: 0,
        });
    }
    actions.sort_by(|left, right| left.id.cmp(&right.id));
    super::authenticate_action_schemas(&mut actions)?;
    Ok(actions)
}

pub(super) fn valid_form_identity(value: &str) -> bool {
    value
        .strip_prefix("glamour-form1-")
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(super) fn valid_static_form_url(value: &str) -> bool {
    if unsafe_static_url(value) {
        return false;
    }
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.starts_with("https://") || normalized.starts_with("http://") {
        return true;
    }
    match normalized.find(':') {
        None => true,
        Some(colon) => ['/', '?', '#']
            .into_iter()
            .filter_map(|delimiter| normalized.find(delimiter))
            .any(|position| position < colon),
    }
}

pub(super) fn valid_static_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(super) fn validate_action_bindings(
    pages: &[StaticPage],
    actions: &[StaticAction],
) -> Result<(), WebCommandError> {
    let known = actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut used = BTreeSet::new();
    for page in pages {
        let mut cursor = 0;
        const MARKER: &str = "data-glamour-form=\"";
        while let Some(relative_start) = page.html[cursor..].find(MARKER) {
            let marker_start = cursor + relative_start;
            let value_start = marker_start + MARKER.len();
            let end = page.html[value_start..].find('"').ok_or_else(|| {
                WebCommandError::failure(format!(
                    "static route `{}` contains a malformed form identity",
                    page.route
                ))
            })?;
            let id = &page.html[value_start..value_start + end];
            let action = actions.iter().find(|action| action.id == id).ok_or_else(|| {
                WebCommandError::failure(format!(
                    "static route `{}` references undeclared action `{id}`",
                    page.route
                ))
            })?;
            let form_start = page.html[..marker_start].rfind("<form").ok_or_else(|| {
                WebCommandError::failure(format!(
                    "static route `{}` places action `{id}` outside a form",
                    page.route
                ))
            })?;
            let form_end = page.html[marker_start..]
                .find('>')
                .map(|end| marker_start + end)
                .ok_or_else(|| {
                    WebCommandError::failure(format!(
                        "static route `{}` has an unterminated form",
                        page.route
                    ))
                })?;
            let opening = &page.html[form_start..form_end];
            let method = format!(" method=\"{}\"", action.method);
            let destination =
                format!(" action=\"{}\"", escape_static_attribute(&action.action));
            if !opening.contains(&method) || !opening.contains(&destination) {
                return Err(WebCommandError::failure(format!(
                    "static route `{}` form `{id}` disagrees with its method or action contract",
                    page.route
                )));
            }
            used.insert(id);
            cursor = value_start + end + 1;
        }
    }
    if let Some(unused) = known.into_iter().find(|id| !used.contains(id)) {
        return Err(WebCommandError::failure(format!(
            "static action `{unused}` is never attached to a rendered form"
        )));
    }
    Ok(())
}

fn escape_static_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn validate_static_document(route: &str, html: &str) -> Result<(), WebCommandError> {
    const PREFIX: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"></head><body>";
    const SUFFIX: &str = "</body></html>";
    let body = html
        .strip_prefix(PREFIX)
        .and_then(|html| html.strip_suffix(SUFFIX))
        .ok_or_else(|| {
            WebCommandError::failure(format!(
                "static route `{route}` is not a canonical Glamour document"
            ))
        })?;
    let allowed_tags = [
        "a", "article", "aside", "blockquote", "br", "button", "caption", "code",
        "dd", "details", "div", "dl", "dt", "em", "fieldset", "figcaption",
        "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6",
        "header", "hr", "iframe", "img", "input", "label", "legend", "li", "main", "nav",
        "ol", "option", "p", "pre", "section", "select", "small", "span",
        "strong", "summary", "table", "tbody", "td", "textarea", "tfoot", "th",
        "thead", "tr", "ul",
    ];
    let mut stack = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let close = rest.find('>').ok_or_else(|| {
            WebCommandError::failure(format!(
                "static route `{route}` contains an unterminated HTML tag"
            ))
        })?;
        let token = &rest[..close];
        rest = &rest[close + 1..];
        if token.is_empty() || token.starts_with('!') || token.starts_with('?') {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains unsupported markup"
            )));
        }
        if let Some(name) = token.strip_prefix('/') {
            if name.is_empty()
                || name.contains(char::is_whitespace)
                || stack.pop().as_deref() != Some(name)
            {
                return Err(WebCommandError::failure(format!(
                    "static route `{route}` contains mismatched HTML tags"
                )));
            }
            continue;
        }
        let name_end = token
            .find(char::is_whitespace)
            .unwrap_or(token.len());
        let name = &token[..name_end];
        if !allowed_tags.contains(&name) {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains disallowed element `<{name}>`"
            )));
        }
        validate_static_attributes(route, name, &token[name_end..])?;
        if name == "iframe"
            && (!token.contains(" sandbox=\"allow-scripts\"")
                || !token.contains(" class=\"glamour-compartment\"")
                || !token.contains(" data-glamour-frame-renderer=\"document.v1\"")
                || !token.contains(" data-glamour-frame-event=\"")
                || token.contains("allow-same-origin"))
        {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains an unauthenticated frame compartment"
            )));
        }
        stack.push(name.to_string());
    }
    if !stack.is_empty() {
        return Err(WebCommandError::failure(format!(
            "static route `{route}` contains unclosed HTML elements"
        )));
    }
    Ok(())
}

fn validate_static_attributes(
    route: &str,
    element: &str,
    mut attributes: &str,
) -> Result<(), WebCommandError> {
    let mut names = BTreeSet::new();
    while !attributes.is_empty() {
        let trimmed = attributes.trim_start_matches(char::is_whitespace);
        if trimmed.len() == attributes.len() {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains malformed attributes on `<{element}>`"
            )));
        }
        attributes = trimmed;
        if attributes.is_empty() {
            break;
        }
        let Some(separator) = attributes.find("=\"") else {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains an unquoted attribute on `<{element}>`"
            )));
        };
        let name = &attributes[..separator];
        if name.is_empty()
            || !name.bytes().next().is_some_and(|byte| byte.is_ascii_alphabetic())
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
            })
            || name.starts_with("on")
            || matches!(name, "srcdoc" | "formaction")
            || !names.insert(name)
        {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains disallowed attribute `{name}`"
            )));
        }
        attributes = &attributes[separator + 2..];
        let Some(end) = attributes.find('"') else {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains an unterminated `{name}` value"
            )));
        };
        let value = &attributes[..end];
        if name == "style" && !valid_static_custom_properties(value) {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains an unsafe style attribute"
            )));
        }
        if matches!(name, "href" | "src" | "action") && unsafe_static_url(value) {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains an unsafe `{name}` URL"
            )));
        }
        attributes = &attributes[end + 1..];
    }
    Ok(())
}

pub(super) fn valid_static_custom_properties(value: &str) -> bool {
    if value.is_empty() || value.len() > 4096 {
        return false;
    }
    let mut names = BTreeSet::new();
    for declaration in value.split(';') {
        let Some((name, token)) = declaration.split_once(':') else {
            return false;
        };
        let Some(local) = name.strip_prefix("--glamour-") else {
            return false;
        };
        if local.is_empty()
            || !local
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !local
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !names.insert(name)
            || !valid_static_css_token(token)
        {
            return false;
        }
    }
    true
}

fn valid_static_css_token(value: &str) -> bool {
    if matches!(
        value,
        "transparent"
            | "currentcolor"
            | "black"
            | "white"
            | "red"
            | "green"
            | "blue"
            | "rebeccapurple"
    ) {
        return true;
    }
    if let Some(hex) = value.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8)
            && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if let Some(name) = value
        .strip_prefix("var(--glamour-")
        .and_then(|value| value.strip_suffix(')'))
    {
        return !name.is_empty()
            && name
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    }
    if let Some(number) = value.strip_suffix("px") {
        return valid_static_css_integer(number, 1_000_000);
    }
    if let Some(number) = value.strip_suffix("rem") {
        return valid_static_css_integer(number, 10_000);
    }
    if let Some(number) = value.strip_suffix('%') {
        return valid_static_css_integer(number, 100_000);
    }
    if let Some(number) = value.strip_suffix("deg") {
        return valid_static_css_integer(number, 360_000);
    }
    if let Some(number) = value.strip_suffix("ms") {
        return valid_static_css_integer(number, 3_600_000);
    }
    valid_static_css_integer(value, 1_000_000)
}

fn valid_static_css_category_token(kind: &str, value: &str) -> bool {
    if valid_static_css_var_reference(value) {
        return true;
    }
    match kind {
        "css-color" => {
            matches!(
                value,
                "transparent"
                    | "currentcolor"
                    | "black"
                    | "white"
                    | "red"
                    | "green"
                    | "blue"
                    | "rebeccapurple"
            ) || value.strip_prefix('#').is_some_and(|hex| {
                matches!(hex.len(), 3 | 4 | 6 | 8)
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        }
        "css-length" => value
            .strip_suffix("px")
            .is_some_and(|number| valid_static_css_integer(number, 1_000_000))
            || value
                .strip_suffix("rem")
                .is_some_and(|number| valid_static_css_integer(number, 10_000)),
        "css-number" => valid_static_css_integer(value, 1_000_000),
        "css-percentage" => value
            .strip_suffix('%')
            .is_some_and(|number| valid_static_css_integer(number, 100_000)),
        "css-angle" => value
            .strip_suffix("deg")
            .is_some_and(|number| valid_static_css_integer(number, 360_000)),
        "css-time" => value
            .strip_suffix("ms")
            .is_some_and(|number| valid_static_css_integer(number, 3_600_000)),
        _ => false,
    }
}

fn valid_static_css_var_reference(value: &str) -> bool {
    let Some(name) = value
        .strip_prefix("var(--glamour-")
        .and_then(|value| value.strip_suffix(')'))
    else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_static_css_integer(value: &str, maximum: i64) -> bool {
    let Ok(parsed) = value.parse::<i64>() else {
        return false;
    };
    parsed.unsigned_abs() <= maximum as u64 && parsed.to_string() == value
}

fn unsafe_static_url(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.starts_with("javascript:")
        || normalized.starts_with("vbscript:")
        || normalized.starts_with("data:text/html")
        || normalized.starts_with("//")
        || normalized.contains('\n')
        || normalized.contains('\r')
        || normalized.contains('\t')
}

fn static_route_output(route: &str) -> Result<PathBuf, WebCommandError> {
    if !route.starts_with('/')
        || route.contains('\\')
        || route.contains('%')
        || route.contains('?')
        || route.contains('#')
        || route.split('/').any(|part| part == "." || part == "..")
    {
        return Err(WebCommandError::failure(format!(
            "static route `{route}` must be a canonical absolute path without escapes, queries, or fragments"
        )));
    }
    if route != "/" && route.ends_with('/') {
        return Err(WebCommandError::failure(format!(
            "static route `{route}` must omit its trailing slash"
        )));
    }
    let mut output = PathBuf::new();
    for part in route.split('/').filter(|part| !part.is_empty()) {
        if !part
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        {
            return Err(WebCommandError::failure(format!(
                "static route `{route}` contains unsupported path characters"
            )));
        }
        output.push(part);
    }
    output.push("index.html");
    Ok(output)
}

pub(super) fn write_static_production(
    checked: &CheckedStaticSite,
    output: &Path,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    publish_artifacts(output, &checked.project.root, |staging| {
        populate_static_artifacts(checked, staging)
    })
}

fn csp_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn static_inline_style_sources(html: &str) -> Result<Vec<String>, WebCommandError> {
    let mut sources = BTreeSet::new();
    let mut cursor = 0;
    while let Some(start) = html[cursor..].find("<style") {
        let start = cursor + start;
        let open = html[start..].find('>').map(|offset| start + offset + 1)
            .ok_or_else(|| WebCommandError::failure("static style element is unterminated"))?;
        let close = html[open..].find("</style>").map(|offset| open + offset)
            .ok_or_else(|| WebCommandError::failure("static style element is unterminated"))?;
        let digest = Sha256::digest(&html.as_bytes()[open..close]);
        sources.insert(format!("'sha256-{}'", csp_base64(&digest)));
        cursor = close + "</style>".len();
    }
    Ok(sources.into_iter().collect())
}

fn static_style_attribute_sources(html: &str) -> Result<Vec<String>, WebCommandError> {
    let mut sources = BTreeSet::new();
    let mut cursor = 0;
    const MARKER: &str = " style=\"";
    while let Some(start) = html[cursor..].find(MARKER) {
        let value_start = cursor + start + MARKER.len();
        let value_end = html[value_start..].find('"').map(|offset| value_start + offset)
            .ok_or_else(|| WebCommandError::failure("static style attribute is unterminated"))?;
        let digest = Sha256::digest(&html.as_bytes()[value_start..value_end]);
        sources.insert(format!("'sha256-{}'", csp_base64(&digest)));
        cursor = value_end + 1;
    }
    Ok(sources.into_iter().collect())
}

fn static_policy_url_source(url: &str) -> String {
    let normalized = url.trim();
    for scheme in ["https://", "http://"] {
        if let Some(rest) = normalized.strip_prefix(scheme) {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            return format!("{scheme}{authority}");
        }
    }
    if let Some(colon) = normalized.find(':') {
        return normalized[..=colon].to_string();
    }
    "'self'".to_string()
}

fn static_route_browser_policy(
    checked: &CheckedStaticSite,
    page: &StaticPage,
) -> Value {
    let actions = static_page_actions(page, &checked.actions);
    let mut lists = [
        ("fetch", BTreeMap::<String, Value>::new()),
        ("navigation", BTreeMap::new()),
        ("timers", BTreeMap::new()),
        ("ports", BTreeMap::new()),
        ("secretFields", BTreeMap::new()),
        ("frames", BTreeMap::new()),
        ("workers", BTreeMap::new()),
        ("storage", BTreeMap::new()),
    ];
    for action in &actions {
        for field in action.fields.iter().filter(|field| field.kind == "secret") {
            let value = json!({"form": action.id, "field": field.name});
            lists[4].1.insert(
                serde_json::to_string(&value).expect("secret policy value serializes"),
                value,
            );
        }
    }
    for plan in checked
        .island_plans
        .iter()
        .filter(|plan| page.island_keys.iter().any(|key| key == &plan.key))
    {
        let policy = static_island_browser_policy(plan, &actions);
        for (name, values) in &mut lists {
            for value in policy[*name].as_array().expect("browser policy list") {
                values.insert(
                    serde_json::to_string(value).expect("browser policy value serializes"),
                    value.clone(),
                );
            }
        }
    }
    json!({
        "schema": "witchy.glamour.route-browser-policy.v1",
        "route": page.route,
        "capabilities": lists.into_iter().map(|(name, values)| (
            name.to_string(),
            Value::Array(values.into_values().collect()),
        )).collect::<serde_json::Map<_, _>>(),
        "staticControls": static_control_projection(&actions),
    })
}

fn static_route_permissions_policy(authority: &Value) -> String {
    let _ = authority;
    "camera=(), microphone=(), geolocation=(), publickey-credentials-get=(), publickey-credentials-create=()".to_string()
}

fn static_route_content_security_policy(
    checked: &CheckedStaticSite,
    page: &StaticPage,
    html: &str,
) -> Result<String, WebCommandError> {
    let interactive = !page.island_keys.is_empty();
    let actions = static_page_actions(page, &checked.actions);
    let interactive_actions = checked
        .islands
        .iter()
        .filter(|island| page.island_keys.contains(&island.key))
        .flat_map(|island| static_actions_in_html(&island.html, &checked.actions))
        .map(|action| (action.id.clone(), action))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    let mut style_sources = BTreeSet::new();
    if checked.project.css.is_some()
        || checked.styles.iter().any(|style| style.routes.contains(&page.route))
    {
        style_sources.insert("'self'".to_string());
    }
    style_sources.extend(static_inline_style_sources(html)?);
    let style_attribute_sources = static_style_attribute_sources(html)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut form_sources = actions
        .iter()
        .map(|action| static_policy_url_source(&action.action))
        .collect::<BTreeSet<_>>();
    for plan in checked
        .island_plans
        .iter()
        .filter(|plan| page.island_keys.iter().any(|key| key == &plan.key))
    {
        for event in &plan.events {
            if let Some(StaticIslandProgressiveFallback::Submit { action, .. }) =
                &event.fallback
            {
                form_sources.insert(static_policy_url_source(action));
            }
        }
    }
    let mut connect_sources = if interactive {
        BTreeSet::from(["'self'".to_string()])
    } else {
        BTreeSet::new()
    };
    if interactive {
        connect_sources.extend(
            interactive_actions
                .iter()
                .map(|action| static_policy_url_source(&action.action)),
        );
    }
    let sources = |values: &BTreeSet<String>| {
        if values.is_empty() {
            "'none'".to_string()
        } else {
            values.iter().cloned().collect::<Vec<_>>().join(" ")
        }
    };
    if form_sources.is_empty() {
        form_sources.insert("'none'".to_string());
    }
    let route_policy = static_route_browser_policy(checked, page);
    let worker_source = if route_policy["capabilities"]["workers"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        "'none'"
    } else {
        "'self'"
    };
    let frame_source = if route_policy["capabilities"]["frames"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        "'none'"
    } else {
        "'self'"
    };
    Ok([
        "default-src 'none'".to_string(),
        "base-uri 'none'".to_string(),
        "object-src 'none'".to_string(),
        "frame-ancestors 'none'".to_string(),
        format!(
            "script-src {}",
            if interactive {
                "'self' 'wasm-unsafe-eval'"
            } else {
                "'none'"
            },
        ),
        format!("style-src {}", sources(&style_sources)),
        format!(
            "style-src-attr {}",
            if style_attribute_sources.is_empty() {
                "'none'".to_string()
            } else {
                format!("'unsafe-hashes' {}", sources(&style_attribute_sources))
            },
        ),
        format!("connect-src {}", sources(&connect_sources)),
        "img-src 'self' data:".to_string(),
        "font-src 'self'".to_string(),
        "media-src 'self'".to_string(),
        format!("frame-src {frame_source}"),
        format!("worker-src {worker_source}"),
        format!("form-action {}", sources(&form_sources)),
        "require-trusted-types-for 'script'".to_string(),
        "trusted-types 'none'".to_string(),
    ].join("; "))
}

fn static_meta_content_security_policy(policy: &str) -> String {
    policy
        .split(';')
        .map(str::trim)
        .filter(|directive| !directive.starts_with("frame-ancestors "))
        .collect::<Vec<_>>()
        .join("; ")
}

fn static_route_security_policies(
    checked: &CheckedStaticSite,
    staging: &Path,
) -> Result<Vec<Value>, WebCommandError> {
    checked.pages.iter().map(|page| {
        let html = std::fs::read_to_string(staging.join(&page.output))
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        let content_security_policy =
            static_route_content_security_policy(checked, page, &html)?;
        let authority = static_route_browser_policy(checked, page);
        Ok(json!({
            "route": page.route,
            "contentSecurityPolicy": content_security_policy,
            "metaContentSecurityPolicy": static_meta_content_security_policy(&content_security_policy),
            "permissionsPolicy": static_route_permissions_policy(&authority),
            "unavailableInMeta": ["frame-ancestors", "permissions-policy"],
            "hosting": {
                "profile": checked.project.hosting.name(),
                "responseHeadersRequired": checked.project.hosting.response_headers_required(),
                "enforcement": if checked.project.hosting.response_headers_required() {
                    "required"
                } else {
                    "degraded"
                },
            },
            "trustedTypes": {"required": true, "policies": []},
            "authority": authority,
        }))
    }).collect()
}

fn write_static_meta_policies(
    checked: &CheckedStaticSite,
    staging: &Path,
    route_policies: &[Value],
) -> Result<(), WebCommandError> {
    for page in &checked.pages {
        let policy = route_policies
            .iter()
            .find(|policy| policy["route"] == page.route)
            .ok_or_else(|| {
                WebCommandError::failure(format!(
                    "static route `{}` has no browser policy",
                    page.route,
                ))
            })?;
        let meta_policy = policy["metaContentSecurityPolicy"]
            .as_str()
            .ok_or_else(|| WebCommandError::failure("static route meta CSP is missing"))?;
        let path = staging.join(&page.output);
        let html = std::fs::read_to_string(&path)
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        if html.contains("http-equiv=\"Content-Security-Policy\"") {
            return Err(WebCommandError::failure(format!(
                "static route `{}` already contains a Content-Security-Policy meta element",
                page.route,
            )));
        }
        if !html.contains("<head>") {
            return Err(WebCommandError::failure(format!(
                "static route `{}` has no canonical document head",
                page.route,
            )));
        }
        let meta = format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
            escape_static_attribute(meta_policy),
        );
        write_file(
            &path,
            html.replacen("<head>", &format!("<head>{meta}"), 1)
                .as_bytes(),
        )?;
    }
    Ok(())
}

fn validate_static_meta_policies(
    checked: &CheckedStaticSite,
    root: &Path,
    route_policies: &[Value],
) -> Result<(), WebCommandError> {
    for page in &checked.pages {
        let policy = route_policies
            .iter()
            .find(|policy| policy["route"] == page.route)
            .expect("checked route policy");
        let meta = format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
            escape_static_attribute(
                policy["metaContentSecurityPolicy"]
                    .as_str()
                    .expect("checked meta CSP"),
            ),
        );
        let html = std::fs::read_to_string(root.join(&page.output))
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        if html.matches(&meta).count() != 1 || !html.starts_with(&format!("<!doctype html><html lang=\"en\"><head>{meta}")) {
            return Err(WebCommandError::failure(format!(
                "static route `{}` does not contain its exact leading meta CSP",
                page.route,
            )));
        }
    }
    Ok(())
}

fn static_security_headers(route_policies: &[Value], wasm: bool, frames: bool) -> String {
    let mut headers = String::from(
        "/*\n  Referrer-Policy: no-referrer\n  X-Content-Type-Options: nosniff\n",
    );
    for policy in route_policies {
        headers.push_str(&format!(
            "\n{}\n  Content-Security-Policy: {}\n  Permissions-Policy: {}\n",
            policy["route"].as_str().expect("route policy route"),
            policy["contentSecurityPolicy"].as_str().expect("route CSP"),
            policy["permissionsPolicy"].as_str().expect("route permissions policy"),
        ));
    }
    if wasm {
        headers.push_str("\n/assets/*.wasm\n  Content-Type: application/wasm\n  Cache-Control: public, max-age=31536000, immutable\n");
    }
    if frames {
        headers.push_str("\n/assets/frame-*.html\n  Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'none'; img-src 'none'; media-src 'none'; font-src 'none'; form-action 'none'; frame-ancestors 'self'\n");
    }
    headers.push_str("\n/assets/*\n  Cache-Control: public, max-age=31536000, immutable\n");
    headers
}

fn populate_static_artifacts(
    checked: &CheckedStaticSite,
    staging: &Path,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    if checked.island_publication.is_some() {
        return populate_static_island_artifacts(checked, staging);
    }
    let typed_sources = static_asset_sources(&checked.project.public, &checked.assets)?;
    if checked.project.public.exists() {
        copy_public_except(&checked.project.public, staging, &typed_sources)?;
    }
    let assets = staging.join("assets");
    std::fs::create_dir(&assets)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let typed_assets =
        publish_static_assets(&checked.project.public, &assets, &checked.assets)?;
    let resolved_preloads = resolve_static_preloads(&checked.preloads, &typed_assets);
    let css_name = if let Some(css) = &checked.project.css {
        let bytes = std::fs::read(css).map_err(|error| {
            WebCommandError::failure(format!("cannot read `{}`: {error}", css.display()))
        })?;
        if bytes.is_empty() {
            None
        } else {
            let name = content_name("app", "css", &bytes);
            write_file(&assets.join(&name), &bytes)?;
            Some(name)
        }
    } else {
        None
    };
    let style_texts = rewritten_static_style_texts(&checked.styles, &typed_assets)?;
    let style_assets = static_style_assets(&checked.styles, &style_texts);
    for style in &checked.styles {
        if let Some(name) = style_assets.get(&style.id) {
            write_file(
                &assets.join(name),
                style_texts
                    .get(&style.id)
                    .expect("every checked style has rewritten text")
                    .as_bytes(),
            )?;
        }
    }
    for preload in &resolved_preloads {
        validate_emitted_preload(staging, preload)?;
    }

    for page in &checked.pages {
        let destination = staging.join(&page.output);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                WebCommandError::failure(format!(
                    "cannot create static route directory `{}`: {error}",
                    parent.display()
                ))
            })?;
        }
        let mut head = String::new();
        for preload in resolved_preloads
            .iter()
            .filter(|preload| preload.route == page.route)
        {
            let crossorigin = if preload.kind == "font" {
                " crossorigin=\"anonymous\""
            } else {
                ""
            };
            head.push_str(&format!(
                "<link rel=\"preload\" href=\"{}\" as=\"{}\"{}>",
                preload.href, preload.kind, crossorigin
            ));
        }
        for style in checked
            .styles
            .iter()
            .filter(|style| style.routes.contains(&page.route))
        {
            if style.critical_routes.contains(&page.route) {
                head.push_str(&format!(
                    "<style data-glamour-style=\"{}\">{}</style>",
                    style.id,
                    style_texts
                        .get(&style.id)
                        .expect("every checked style has rewritten text")
                ));
            } else {
                let asset = style_assets.get(&style.id).ok_or_else(|| {
                    WebCommandError::failure(format!(
                        "static style `{}` has no extracted asset",
                        style.id
                    ))
                })?;
                head.push_str(&format!(
                    "<link rel=\"stylesheet\" href=\"/assets/{asset}\" data-glamour-style=\"{}\">",
                    style.id
                ));
            }
        }
        if let Some(css) = &css_name {
            head.push_str(&format!(
                "<link rel=\"stylesheet\" href=\"/assets/{css}\">"
            ));
        }
        let html = rewrite_static_asset_urls(&page.html, &typed_assets)
            .replacen("</head>", &format!("{head}</head>"), 1);
        write_file(&destination, html.as_bytes())?;
    }

    let browser_policy = static_route_security_policies(checked, staging)?;
    write_static_meta_policies(checked, staging, &browser_policy)?;
    if static_route_security_policies(checked, staging)? != browser_policy {
        return Err(WebCommandError::failure(
            "static route policy changed after meta CSP insertion",
        ));
    }
    let initial_records = artifact_records(staging)?;
    let routes = checked
        .pages
        .iter()
        .map(|page| {
            json!({
                "path": page.route,
                "file": slash_path(&page.output),
            })
        })
        .collect::<Vec<_>>();
    let identity_input = serde_json::to_vec(&json!({
        "application": {
            "name": checked.project.name,
            "version": checked.project.version,
        },
        "routes": routes,
        "actions": checked.actions,
        "styles": checked.styles,
        "preloads": resolved_preloads,
        "publicAssets": checked.assets,
        "contentInputs": checked.content_inputs,
        "browserPolicy": browser_policy,
        "artifacts": initial_records,
    }))
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let build_identity = sha256(&identity_input);
    let manifest = json!({
        "schema": "witchy.web.static-manifest.v1",
        "application": {
            "name": checked.project.name,
            "version": checked.project.version,
        },
        "buildIdentity": build_identity,
        "delivery": "static",
        "routes": routes,
        "actions": checked.actions,
        "styles": static_style_manifest(&checked.styles, &style_assets),
        "preloads": resolved_preloads,
        "publicAssets": checked.assets.iter().map(|asset| json!({
            "href": asset.href,
            "emitted": typed_assets.get(&asset.href)
                .map(|name| format!("/assets/{name}")),
        })).collect::<Vec<_>>(),
        "contentInputs": checked.content_inputs,
        "browserPolicy": browser_policy,
        "assets": css_name.iter().cloned()
            .chain(style_assets.values().cloned())
            .chain(typed_assets.values().cloned())
            .collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
        "runtime": {
            "javascript": false,
            "wasm": false,
        },
    });
    write_file(
        &staging.join("witchy-web-manifest.json"),
        &pretty_json(&manifest)?,
    )?;
    write_file(
        &staging.join("_headers"),
        static_security_headers(
            &browser_policy,
            !checked.islands.is_empty(),
            checked.island_plans.iter().any(|plan| !plan.frames.is_empty()),
        )
        .as_bytes(),
    )?;
    let components = checked
        .packages
        .iter()
        .map(|package| {
            json!({
                "bom-ref": format!(
                    "witchy:{}:{}@{}",
                    package.source, package.name, package.version
                ),
                "type": "library",
                "name": package.name,
                "version": package.version,
                "properties": [
                    {"name": "witchy.package.source", "value": package.source}
                ],
            })
        })
        .collect::<Vec<_>>();
    let component_references = components
        .iter()
        .filter_map(|component| component["bom-ref"].as_str())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let application_reference =
        format!("witchy-app:{}@{}", checked.project.name, checked.project.version);
    let dependencies = std::iter::once(json!({
        "ref": application_reference,
        "dependsOn": component_references,
    }))
    .chain(
        component_references
            .iter()
            .map(|reference| json!({"ref": reference, "dependsOn": []})),
    )
    .collect::<Vec<_>>();
    let sbom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "bom-ref": application_reference,
                "type": "application",
                "name": checked.project.name,
                "version": checked.project.version,
            },
        },
        "components": components,
        "dependencies": dependencies,
    });
    write_file(
        &staging.join("witchy-sbom.cdx.json"),
        &pretty_json(&sbom)?,
    )?;
    let records = artifact_records(staging)?;
    let report = json!({
        "schema": "witchy.web.build-report.v1",
        "mode": "production",
        "delivery": "static",
        "application": {
            "name": checked.project.name,
            "version": checked.project.version,
        },
        "buildIdentity": build_identity,
        "compiler": {
            "version": env!("CARGO_PKG_VERSION"),
            "commit": option_env!("WITCHY_BUILD_COMMIT"),
        },
        "capabilities": {"runtime": [], "build": [], "user": []},
        "runtime": {"javascript": false, "wasm": false},
        "actions": {
            "count": checked.actions.len(),
            "secretFields": checked.actions.iter().flat_map(|action| action.fields.iter())
                .filter(|field| field.kind == "secret").count(),
        },
        "styles": {
            "count": checked.styles.len(),
            "criticalRouteBindings": checked.styles.iter()
                .map(|style| style.critical_routes.len()).sum::<usize>(),
            "extractedAssets": style_assets.len(),
            "globalRules": static_global_rule_owners(&checked.styles),
        },
        "preloads": {"count": checked.preloads.len()},
        "publicAssets": {"count": checked.assets.len()},
        "contentInputs": checked.content_inputs,
        "browserPolicy": browser_policy,
        "packages": checked.packages,
        "artifacts": records,
    });
    write_file(
        &staging.join("witchy-build-report.json"),
        &pretty_json(&report)?,
    )?;
    audit_static_artifacts(staging, checked)?;
    artifact_records(staging)
}

fn populate_static_island_artifacts(
    checked: &CheckedStaticSite,
    staging: &Path,
) -> Result<Vec<ArtifactRecord>, WebCommandError> {
    let publication = checked
        .island_publication
        .as_ref()
        .expect("interactive publication was checked");
    if checked.island_plans.len() != checked.islands.len() {
        return Err(WebCommandError::failure(
            "interactive publication does not cover every checked island plan",
        ));
    }
    let mut base = checked.clone();
    base.island_publication = None;
    for page in &mut base.pages {
        page.html = publication
            .pages
            .get(&page.route)
            .cloned()
            .ok_or_else(|| WebCommandError::failure(format!(
                "interactive publication omitted route `{}`",
                page.route,
            )))?;
    }
    populate_static_artifacts(&base, staging)?;
    let assets = staging.join("assets");
    for artifact in &publication.artifacts {
        if content_name("island", "wasm", &artifact.wasm) != artifact.file {
            return Err(WebCommandError::failure(format!(
                "interactive artifact `{}` is not named from its final bytes",
                artifact.identity,
            )));
        }
        write_file(&assets.join(&artifact.file), &artifact.wasm)?;
    }
    for worker in &publication.workers {
        if content_name("worker", "wasm", &worker.wasm) != worker.file {
            return Err(WebCommandError::failure(format!(
                "worker artifact `{}` is not named from its final bytes",
                worker.identity,
            )));
        }
        write_file(&assets.join(&worker.file), &worker.wasm)?;
    }
    for frame in &publication.frames {
        if content_name("frame", "html", &frame.html) != frame.file {
            return Err(WebCommandError::failure(format!(
                "frame artifact `{}` is not named from its final bytes",
                frame.identity,
            )));
        }
        write_file(&assets.join(&frame.file), &frame.html)?;
    }
    let runtime_modules = static_island_runtime_modules();
    let boot = runtime_modules
        .iter()
        .find(|(name, _)| name.starts_with("glamour-island-boot-"))
        .map(|(name, _)| name.clone())
        .ok_or_else(|| WebCommandError::failure("static island runtime has no boot module"))?;
    for (name, bytes) in &runtime_modules {
        write_file(&assets.join(name), bytes)?;
    }
    write_file(
        &staging.join("witchy-islands-manifest.json"),
        &pretty_json(&publication.manifest)?,
    )?;
    write_file(
        &staging.join("witchy-island-artifacts.json"),
        &pretty_json(&publication.artifact_manifest)?,
    )?;
    for route in publication.route_manifests.values() {
        write_file(&staging.join(&route.file), &pretty_json(&route.manifest)?)?;
    }
    for page in checked.pages.iter().filter(|page| !page.island_keys.is_empty()) {
        let path = staging.join(&page.output);
        let html = std::fs::read_to_string(&path)
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        let route_manifest = publication
            .route_manifests
            .get(&page.route)
            .expect("interactive route has a checked route manifest");
        let script = format!(
            "<script type=\"module\" src=\"/assets/{boot}\" data-witchy-islands data-witchy-islands-manifest=\"{}\"></script>",
            route_manifest.file,
        );
        if !html.contains("</body>") {
            return Err(WebCommandError::failure(format!(
                "interactive route `{}` has no body for its loader",
                page.route,
            )));
        }
        write_file(&path, html.replacen("</body>", &format!("{script}</body>"), 1).as_bytes())?;
    }
    let mut manifest = read_static_json(staging, "witchy-web-manifest.json")?;
    let manifest_object = manifest.as_object_mut().expect("base static manifest is an object");
    manifest_object.insert("buildIdentity".into(), json!(publication.build_identity));
    manifest_object.insert("mountGrant".into(), publication.manifest["mountGrant"].clone());
    manifest_object.insert("islands".into(), json!({
        "manifest": "witchy-islands-manifest.json",
        "artifacts": "witchy-island-artifacts.json",
        "routes": publication.route_manifests.iter().map(|(route, manifest)| (
            route.clone(),
            manifest.file.clone(),
        )).collect::<BTreeMap<_, _>>(),
    }));
    manifest_object.insert("runtime".into(), json!({"javascript": true, "wasm": true}));
    manifest_object.insert(
        "assets".into(),
        json!(artifact_records(&assets)?.into_iter().map(|record| record.path).collect::<Vec<_>>()),
    );
    write_file(&staging.join("witchy-web-manifest.json"), &pretty_json(&manifest)?)?;
    let runtime_component_names = runtime_modules
        .iter()
        .map(|(name, _)| name.clone())
        .chain(publication.artifacts.iter().map(|artifact| artifact.file.clone()))
        .chain(publication.workers.iter().map(|worker| worker.file.clone()))
        .chain(publication.frames.iter().map(|frame| frame.file.clone()))
        .collect::<Vec<_>>();
    let mut sbom = read_static_json(staging, "witchy-sbom.cdx.json")?;
    let sbom_object = sbom.as_object_mut().expect("base static SBOM is an object");
    let components = sbom_object["components"]
        .as_array_mut()
        .expect("base static SBOM components are an array");
    let mut runtime_references = Vec::with_capacity(runtime_component_names.len());
    for name in &runtime_component_names {
        let bytes = std::fs::read(assets.join(name))
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        let reference = format!("witchy-web-artifact:{name}");
        runtime_references.push(reference.clone());
        components.push(json!({
            "bom-ref": reference,
            "type": "file",
            "name": name,
            "hashes": [{"alg": "SHA-256", "content": sha256(&bytes)}],
            "properties": [
                {"name": "witchy.web.generated", "value": "true"},
            ],
        }));
    }
    let dependencies = sbom_object["dependencies"]
        .as_array_mut()
        .expect("base static SBOM dependencies are an array");
    let application = dependencies
        .first_mut()
        .and_then(Value::as_object_mut)
        .expect("base static SBOM has an application dependency");
    let depends_on = application["dependsOn"]
        .as_array_mut()
        .expect("base static SBOM application dependencies are an array");
    depends_on.extend(runtime_references.iter().cloned().map(Value::String));
    dependencies.extend(
        runtime_references
            .iter()
            .map(|reference| json!({"ref": reference, "dependsOn": []})),
    );
    write_file(&staging.join("witchy-sbom.cdx.json"), &pretty_json(&sbom)?)?;
    let mut report = read_static_json(staging, "witchy-build-report.json")?;
    let report_object = report.as_object_mut().expect("base static report is an object");
    report_object.insert("buildIdentity".into(), json!(publication.build_identity));
    report_object.insert("capabilities".into(), json!({"runtime": [], "build": [], "user": ["UiRoot"]}));
    report_object.insert("runtime".into(), json!({
        "javascript": true,
        "wasm": true,
        "islands": checked.islands.len(),
        "islandArtifacts": publication.artifacts.len(),
        "workerArtifacts": publication.workers.len(),
        "frameArtifacts": publication.frames.len(),
        "modules": runtime_modules.len(),
    }));
    let records = artifact_records(staging)?
        .into_iter()
        .filter(|record| record.path != "witchy-build-report.json")
        .collect::<Vec<_>>();
    report_object.insert("artifacts".into(), json!(records));
    write_file(&staging.join("witchy-build-report.json"), &pretty_json(&report)?)?;
    audit_static_island_artifacts(staging, checked)?;
    artifact_records(staging)
}

fn read_static_json(root: &Path, name: &str) -> Result<Value, WebCommandError> {
    serde_json::from_slice(
        &std::fs::read(root.join(name))
            .map_err(|error| WebCommandError::failure(error.to_string()))?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))
}

pub(super) fn audit_static_island_artifacts(
    root: &Path,
    checked: &CheckedStaticSite,
) -> Result<(), WebCommandError> {
    let publication = checked
        .island_publication
        .as_ref()
        .expect("interactive publication was checked");
    let emitted_manifest = read_static_json(root, "witchy-islands-manifest.json")?;
    let emitted_artifacts = read_static_json(root, "witchy-island-artifacts.json")?;
    if emitted_manifest != publication.manifest
        || emitted_artifacts != publication.artifact_manifest
    {
        return Err(WebCommandError::failure(
            "published island manifests differ from the authenticated graph",
        ));
    }
    for route in publication.route_manifests.values() {
        if read_static_json(root, &route.file)? != route.manifest {
            return Err(WebCommandError::failure(format!(
                "published route manifest `{}` differs from the authenticated graph",
                route.file,
            )));
        }
    }
    let web_manifest = read_static_json(root, "witchy-web-manifest.json")?;
    let report = read_static_json(root, "witchy-build-report.json")?;
    if web_manifest["buildIdentity"] != publication.build_identity
        || report["buildIdentity"] != publication.build_identity
        || web_manifest["runtime"] != json!({"javascript": true, "wasm": true})
        || report["runtime"]["javascript"] != true
        || report["runtime"]["wasm"] != true
    {
        return Err(WebCommandError::failure(
            "interactive static manifest and report do not share the authenticated runtime identity",
        ));
    }
    let assets = root.join("assets");
    for artifact in &publication.artifacts {
        let bytes = std::fs::read(assets.join(&artifact.file)).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot audit island artifact `{}`: {error}",
                artifact.file,
            ))
        })?;
        if bytes != artifact.wasm || content_name("island", "wasm", &bytes) != artifact.file {
            return Err(WebCommandError::failure(format!(
                "published island artifact `{}` changed after authentication",
                artifact.identity,
            )));
        }
        let mut mount_grants = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::CustomSection(section) =
                payload.map_err(|error| WebCommandError::failure(error.to_string()))?
            {
                if section.name() == "witchy.web.mount-grant" {
                    mount_grants.push(
                        serde_json::from_slice::<Value>(section.data())
                            .map_err(|error| WebCommandError::failure(error.to_string()))?,
                    );
                }
            }
        }
        let expected_artifact_grant = publication.artifact_manifest["artifacts"]
            .as_array()
            .and_then(|records| records.iter().find(|record| {
                record["artifact"] == artifact.identity
            }))
            .map(|record| &record["grantProjection"])
            .ok_or_else(|| WebCommandError::failure(format!(
                "published island artifact `{}` has no authenticated grant projection",
                artifact.identity,
            )))?;
        if mount_grants.len() != 1
            || mount_grants[0]["grant"] != publication.manifest["mountGrant"]
            || mount_grants[0]["artifact"] != artifact.identity
            || mount_grants[0]["artifactGrant"] != *expected_artifact_grant
        {
            return Err(WebCommandError::failure(format!(
                "published island artifact `{}` has the wrong embedded mount grant",
                artifact.identity,
            )));
        }
    }
    for worker in &publication.workers {
        let bytes = std::fs::read(assets.join(&worker.file)).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot audit worker artifact `{}`: {error}",
                worker.file,
            ))
        })?;
        if bytes != worker.wasm || content_name("worker", "wasm", &bytes) != worker.file {
            return Err(WebCommandError::failure(format!(
                "published worker artifact `{}` changed after authentication",
                worker.identity,
            )));
        }
        let mut forbidden_imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            if let wasmparser::Payload::ImportSection(section) =
                payload.map_err(|error| WebCommandError::failure(error.to_string()))?
            {
                for import in section.into_imports() {
                    let import = import
                        .map_err(|error| WebCommandError::failure(error.to_string()))?;
                    let permitted = import.module == "witchy"
                        && witchy_wir::wir_prelude::abi_import_info(import.name)
                            .is_some_and(|info| info.browser && info.authorities.is_empty());
                    if !permitted {
                        forbidden_imports.push(format!("{}::{}", import.module, import.name));
                    }
                }
            }
        }
        if !forbidden_imports.is_empty() {
            return Err(WebCommandError::failure(format!(
                "published worker artifact `{}` imports ambient host authority: {}",
                worker.identity, forbidden_imports.join(", "),
            )));
        }
    }
    for frame in &publication.frames {
        let bytes = std::fs::read(assets.join(&frame.file)).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot audit frame artifact `{}`: {error}",
                frame.file,
            ))
        })?;
        if bytes != frame.html || content_name("frame", "html", &bytes) != frame.file {
            return Err(WebCommandError::failure(format!(
                "published frame artifact `{}` changed after authentication",
                frame.identity,
            )));
        }
    }
    for (name, expected) in static_island_runtime_modules() {
        let emitted = std::fs::read(assets.join(&name)).map_err(|error| {
            WebCommandError::failure(format!("cannot audit island runtime `{name}`: {error}"))
        })?;
        if emitted != expected {
            return Err(WebCommandError::failure(format!(
                "published island runtime `{name}` changed after naming",
            )));
        }
    }
    let emitted_asset_names = artifact_records(&assets)?
        .into_iter()
        .map(|record| record.path)
        .collect::<BTreeSet<_>>();
    let manifest_asset_names = web_manifest["assets"]
        .as_array()
        .ok_or_else(|| WebCommandError::failure("interactive manifest has no asset graph"))?
        .iter()
        .map(|asset| {
            asset.as_str().map(str::to_string).ok_or_else(|| {
                WebCommandError::failure("interactive manifest asset identity is not text")
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if manifest_asset_names != emitted_asset_names {
        return Err(WebCommandError::failure(
            "interactive manifest asset graph does not match emitted files",
        ));
    }
    let runtime_modules = static_island_runtime_modules();
    let runtime_names = runtime_modules
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    for (name, source) in &runtime_modules {
        let source = std::str::from_utf8(source)
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        let mut import = String::new();
        for line in source.lines().map(str::trim) {
            if import.is_empty() && !line.starts_with("import ") {
                continue;
            }
            import.push_str(line);
            if !line.ends_with(';') {
                continue;
            }
            let Some((_, target)) = import.split_once("from \"./") else {
                return Err(WebCommandError::failure(format!(
                    "published island runtime `{name}` has an unsupported import",
                )));
            };
            let target = target.split('"').next().unwrap_or_default();
            if !runtime_names.contains(target) {
                return Err(WebCommandError::failure(format!(
                    "published island runtime `{name}` imports absent module `{target}`",
                )));
            }
            import.clear();
        }
        if !import.is_empty() {
            return Err(WebCommandError::failure(format!(
                "published island runtime `{name}` has an unterminated import",
            )));
        }
    }
    let sbom = read_static_json(root, "witchy-sbom.cdx.json")?;
    let sbom_names = sbom["components"]
        .as_array()
        .ok_or_else(|| WebCommandError::failure("interactive SBOM has no components"))?
        .iter()
        .filter_map(|component| component["name"].as_str())
        .collect::<BTreeSet<_>>();
    for name in runtime_names.into_iter().chain(
        publication
            .artifacts
            .iter()
            .map(|artifact| artifact.file.as_str()),
    )
    .chain(publication.workers.iter().map(|worker| worker.file.as_str()))
    .chain(publication.frames.iter().map(|frame| frame.file.as_str()))
    {
        if !sbom_names.contains(name) {
            return Err(WebCommandError::failure(format!(
                "interactive SBOM omits generated artifact `{name}`",
            )));
        }
    }
    for page in &checked.pages {
        let html = std::fs::read_to_string(root.join(&page.output))
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        let scripts = html
            .matches(" data-witchy-islands data-witchy-islands-manifest=")
            .count();
        if scripts != usize::from(!page.island_keys.is_empty()) {
            return Err(WebCommandError::failure(format!(
                "interactive route `{}` has the wrong loader count",
                page.route,
            )));
        }
        if let Some(route) = publication.route_manifests.get(&page.route) {
            let marker = format!(" data-witchy-islands-manifest=\"{}\"", route.file);
            if html.matches(&marker).count() != 1 {
                return Err(WebCommandError::failure(format!(
                    "interactive route `{}` does not select its authenticated island manifest",
                    page.route,
                )));
            }
        }
    }
    let headers = std::fs::read_to_string(root.join("_headers"))
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if !headers.contains("script-src 'self' 'wasm-unsafe-eval'")
        || !headers.contains("Content-Type: application/wasm")
    {
        return Err(WebCommandError::failure(
            "interactive static headers do not admit only the published runtime",
        ));
    }
    let expected_records = artifact_records(root)?
        .into_iter()
        .filter(|record| record.path != "witchy-build-report.json")
        .collect::<Vec<_>>();
    let recorded: Vec<ArtifactRecord> = serde_json::from_value(
        report
            .get("artifacts")
            .cloned()
            .ok_or_else(|| WebCommandError::failure("interactive report has no artifact graph"))?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if recorded != expected_records {
        return Err(WebCommandError::failure(
            "interactive report artifact graph does not match emitted files",
        ));
    }
    let absolute_project = checked.project.root.to_string_lossy();
    for record in artifact_records(root)? {
        let bytes = std::fs::read(root.join(&record.path))
            .map_err(|error| WebCommandError::failure(error.to_string()))?;
        if !absolute_project.is_empty()
            && bytes
                .windows(absolute_project.len())
                .any(|window| window == absolute_project.as_bytes())
        {
            return Err(WebCommandError::failure(format!(
                "interactive artifact `{}` contains an absolute project path",
                record.path,
            )));
        }
    }
    Ok(())
}

fn rewritten_static_style_texts(
    styles: &[StaticStyle],
    assets: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, WebCommandError> {
    styles
        .iter()
        .map(|style| {
            Ok((
                style.id.clone(),
                rewrite_static_css_assets(&style.text, assets)?,
            ))
        })
        .collect()
}

fn static_style_assets(
    styles: &[StaticStyle],
    texts: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    styles
        .iter()
        .filter(|style| {
            style
                .routes
                .iter()
                .any(|route| !style.critical_routes.contains(route))
        })
        .map(|style| {
            (
                style.id.clone(),
                content_name(
                    "style",
                    "css",
                    texts
                        .get(&style.id)
                        .expect("every checked style has rewritten text")
                        .as_bytes(),
                ),
            )
        })
        .collect()
}

fn resolve_static_preloads(
    preloads: &[StaticPreload],
    assets: &BTreeMap<String, String>,
) -> Vec<StaticPreload> {
    preloads
        .iter()
        .map(|preload| StaticPreload {
            route: preload.route.clone(),
            href: assets
                .get(&preload.href)
                .map(|name| format!("/assets/{name}"))
                .unwrap_or_else(|| preload.href.clone()),
            kind: preload.kind.clone(),
        })
        .collect()
}

fn rewrite_static_asset_urls(html: &str, assets: &BTreeMap<String, String>) -> String {
    assets
        .iter()
        .fold(html.to_string(), |document, (href, name)| {
            document.replace(
                &format!("=\"{href}\""),
                &format!("=\"/assets/{name}\""),
            )
        })
}

fn static_style_manifest(
    styles: &[StaticStyle],
    assets: &BTreeMap<String, String>,
) -> Vec<Value> {
    styles
        .iter()
        .map(|style| {
            json!({
                "id": style.id,
                "scope": style.scope,
                "global": style.scope == "global",
                "origin": style.origin,
                "classes": style.classes,
                "routes": style.routes,
                "criticalRoutes": style.critical_routes,
                "asset": assets.get(&style.id),
            })
        })
        .collect()
}

fn static_global_rule_owners(styles: &[StaticStyle]) -> Vec<Value> {
    styles
        .iter()
        .filter(|style| style.scope == "global")
        .flat_map(|style| {
            style.text.split('}').filter_map(move |rule| {
                let (selectors, _declarations) = rule.trim().split_once('{')?;
                Some(selectors.split(',').map(move |selector| {
                    json!({
                        "sheet": style.id,
                        "selector": selector.trim(),
                        "owner": style.origin,
                        "routes": style.routes,
                    })
                }))
            })
        })
        .flatten()
        .collect()
}

pub(super) fn audit_static_artifacts(
    root: &Path,
    checked: &CheckedStaticSite,
) -> Result<(), WebCommandError> {
    let absolute_project = checked.project.root.to_string_lossy();
    for record in artifact_records(root)? {
        if matches!(
            Path::new(&record.path).extension().and_then(|value| value.to_str()),
            Some("js" | "mjs" | "wasm")
        ) {
            return Err(WebCommandError::failure(format!(
                "zero-runtime static build emitted executable browser artifact `{}`",
                record.path
            )));
        }
        let bytes = std::fs::read(root.join(&record.path)).map_err(|error| {
            WebCommandError::failure(format!(
                "cannot audit static artifact `{}`: {error}",
                record.path
            ))
        })?;
        if !absolute_project.is_empty()
            && bytes
                .windows(absolute_project.len())
                .any(|window| window == absolute_project.as_bytes())
        {
            return Err(WebCommandError::failure(format!(
                "static artifact `{}` contains an absolute project path",
                record.path
            )));
        }
    }
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(root.join("witchy-web-manifest.json"))
            .map_err(|error| WebCommandError::failure(error.to_string()))?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    let report: Value = serde_json::from_slice(
        &std::fs::read(root.join("witchy-build-report.json"))
            .map_err(|error| WebCommandError::failure(error.to_string()))?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if manifest["delivery"] != "static"
        || manifest["runtime"]["javascript"] != false
        || manifest["runtime"]["wasm"] != false
        || report["delivery"] != "static"
        || report["runtime"]["javascript"] != false
        || report["runtime"]["wasm"] != false
    {
        return Err(WebCommandError::failure(
            "static manifest/report does not prove zero-runtime delivery",
        ));
    }
    if manifest["buildIdentity"] != report["buildIdentity"] {
        return Err(WebCommandError::failure(
            "static manifest and build report identities differ",
        ));
    }
    let expected_browser_policy = static_route_security_policies(checked, root)?;
    if manifest["browserPolicy"] != json!(expected_browser_policy)
        || report["browserPolicy"] != json!(expected_browser_policy)
    {
        return Err(WebCommandError::failure(
            "static browser policy differs from the checked route graph",
        ));
    }
    let headers = std::fs::read_to_string(root.join("_headers"))
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if headers
        != static_security_headers(
            &expected_browser_policy,
            !checked.islands.is_empty(),
            checked.island_plans.iter().any(|plan| !plan.frames.is_empty()),
        )
    {
        return Err(WebCommandError::failure(
            "static headers differ from the checked route browser policy",
        ));
    }
    validate_static_meta_policies(checked, root, &expected_browser_policy)?;
    let manifest_routes = manifest["routes"].as_array().ok_or_else(|| {
        WebCommandError::failure("static manifest route graph is missing")
    })?;
    if manifest_routes.len() != checked.pages.len() {
        return Err(WebCommandError::failure(
            "static manifest route graph is incomplete",
        ));
    }
    let expected_actions = serde_json::to_value(&checked.actions)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if manifest["actions"] != expected_actions {
        return Err(WebCommandError::failure(
            "static manifest action graph is incomplete",
        ));
    }
    let typed_assets = checked
        .assets
        .iter()
        .map(|asset| {
            let path = checked
                .project
                .public
                .join(asset.href.trim_start_matches('/'));
            let bytes = std::fs::read(&path).map_err(|error| {
                WebCommandError::failure(format!(
                    "cannot audit typed public asset `{}`: {error}",
                    path.display()
                ))
            })?;
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    WebCommandError::failure(format!(
                        "typed public asset `{}` has no filename",
                        asset.href
                    ))
                })?;
            let digest = &sha256(&bytes)[..16];
            let name = match path.extension().and_then(|value| value.to_str()) {
                Some(extension) => format!("{stem}-{digest}.{extension}"),
                None => format!("{stem}-{digest}"),
            };
            Ok((asset.href.clone(), name))
        })
        .collect::<Result<BTreeMap<_, _>, WebCommandError>>()?;
    let style_texts = rewritten_static_style_texts(&checked.styles, &typed_assets)?;
    let style_assets = static_style_assets(&checked.styles, &style_texts);
    let expected_styles = serde_json::to_value(static_style_manifest(
        &checked.styles,
        &style_assets,
    ))
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if manifest["styles"] != expected_styles {
        return Err(WebCommandError::failure(
            "static manifest style graph is incomplete",
        ));
    }
    let resolved_preloads = resolve_static_preloads(&checked.preloads, &typed_assets);
    let expected_preloads = serde_json::to_value(&resolved_preloads)
        .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if manifest["preloads"] != expected_preloads {
        return Err(WebCommandError::failure(
            "static manifest preload graph is incomplete",
        ));
    }
    for preload in &resolved_preloads {
        validate_emitted_preload(root, preload)?;
    }
    let expected_public_assets = checked
        .assets
        .iter()
        .map(|asset| {
            json!({
                "href": asset.href,
                "emitted": typed_assets.get(&asset.href)
                    .map(|name| format!("/assets/{name}")),
            })
        })
        .collect::<Vec<_>>();
    if manifest["publicAssets"] != json!(expected_public_assets) {
        return Err(WebCommandError::failure(
            "static manifest public asset graph is incomplete",
        ));
    }
    let mut expected_asset_names = style_assets.values().cloned().collect::<BTreeSet<_>>();
    expected_asset_names.extend(typed_assets.values().cloned());
    if let Some(css) = &checked.project.css {
        let bytes = std::fs::read(css).map_err(|error| {
            WebCommandError::failure(format!("cannot audit `{}`: {error}", css.display()))
        })?;
        if !bytes.is_empty() {
            expected_asset_names.insert(content_name("app", "css", &bytes));
        }
    }
    let manifest_asset_names = manifest["assets"]
        .as_array()
        .ok_or_else(|| WebCommandError::failure("static manifest asset graph is missing"))?
        .iter()
        .map(|asset| {
            asset.as_str().map(str::to_string).ok_or_else(|| {
                WebCommandError::failure("static manifest asset identity is not text")
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if manifest_asset_names != expected_asset_names {
        return Err(WebCommandError::failure(
            "static manifest asset graph is incomplete",
        ));
    }
    for name in &manifest_asset_names {
        if Path::new(name).components().count() != 1 {
            return Err(WebCommandError::failure(
                "static manifest asset identity is not a basename",
            ));
        }
        let bytes = std::fs::read(root.join("assets").join(name)).map_err(|error| {
            WebCommandError::failure(format!("static manifest asset `{name}` is absent: {error}"))
        })?;
        if !name.contains(&sha256(&bytes)[..16]) {
            return Err(WebCommandError::failure(format!(
                "static asset `{name}` is not content-addressed"
            )));
        }
    }
    for page in &checked.pages {
        if !root.join(&page.output).is_file() {
            return Err(WebCommandError::failure(format!(
                "static route `{}` is missing `{}`",
                page.route,
                page.output.display()
            )));
        }
    }
    let expected = artifact_records(root)?
        .into_iter()
        .filter(|record| record.path != "witchy-build-report.json")
        .collect::<Vec<_>>();
    let recorded: Vec<ArtifactRecord> = serde_json::from_value(
        report
            .get("artifacts")
            .cloned()
            .ok_or_else(|| WebCommandError::failure("static report has no artifact graph"))?,
    )
    .map_err(|error| WebCommandError::failure(error.to_string()))?;
    if recorded != expected {
        return Err(WebCommandError::failure(
            "static report artifact graph does not match emitted files",
        ));
    }
    Ok(())
}
