use witchy_syntax::ast::{CallableQualifiers, Expr, Function, Item, Stmt};
use crate::checked_packet_region::PacketRegionSource;
use crate::pipeline::CheckedModule;
use crate::runtime_type::RuntimeDeclarationCatalog;
use crate::packet_region_emitter::{COMPANION, companion};
use crate::scalar_region_preparation::BoundReceipt;
use crate::typeck::{CheckedAnnotation, TypedModule};

#[derive(Debug)]
pub(crate) struct Preparation {
    pub(crate) source: PacketRegionSource,
    original_recipe: Expr,
}


fn root_recipe(module: &witchy_syntax::ast::Module) -> Option<&Expr> {
    let function = module.items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function), _ => None,
    })?;
    let [Stmt::Expr(Expr::Call { name, args })] = function.body.stmts.as_slice() else { return None };
    let [recipe] = args.as_slice() else { return None };
    if name != "task.run" { return None; }
    let Expr::Call { name, args } = recipe else { return None };
    let [Expr::Lambda { params, qualifiers, .. }] = args.as_slice() else { return None };
    (name == "task.lazy" && params.is_empty() && *qualifiers == CallableQualifiers::ORDINARY).then_some(recipe)
}

fn wrapper_arguments(original: &Expr, source: &PacketRegionSource) -> Vec<Expr> {
    let mut arguments = vec![original.clone()];
    arguments.extend(source.root.params.iter().map(|parameter| Expr::Var(parameter.name.clone())));
    arguments
}

fn identical_functions(left: &Function, right: &Function) -> bool {
    left == right && format!("{left:?}") == format!("{right:?}")
}

impl Preparation {
    pub(crate) fn bind(&self, checked: &CheckedModule, typed: &TypedModule,
        proofs: &[witchy_canonical_source::source::TaskAwaitProof], catalog: &RuntimeDeclarationCatalog)
        -> Result<BoundReceipt, String> {
        let rederived = PacketRegionSource::derive(checked, catalog).ok_or("packet source inventory changed before binding")?;
        if rederived != self.source || format!("{rederived:?}") != format!("{:?}", self.source) {
            return Err("packet source receipt is detached from its original checked graph".into());
        }
        crate::checked_scalar_contract::scheduler_contract(checked.module(),
            checked.typed_async_source().ok_or("missing original packet source")?.module(), catalog)?;
        let item = |name: &str| {
            let matching = typed.module().items.iter().enumerate().filter_map(|(index, item)| match item {
                Item::Function(function) if function.name == name => Some((index, function)), _ => None,
            }).collect::<Vec<_>>();
            match matching.as_slice() {
                [(index, function)] => Ok((u32::try_from(*index).map_err(|_| "packet item exceeds u32")?, *function)),
                _ => Err("packet function is missing or duplicated"),
            }
        };
        let (root, root_function) = item("main")?;
        let (companion_item, companion) = item(COMPANION)?;
        let mut covered = Vec::new();
        let mut coordinates = Vec::new();
        for flow in &rederived.flows {
            let mut awaits = flow.operations.iter().filter(|operation|
                operation.kind == crate::async_control_flow::SourceAsyncOperationKind::Await).collect::<Vec<_>>();
            awaits.sort_by_key(|operation| operation.expression);
            for (suspension, _) in awaits.into_iter().enumerate() {
                let suspension = u32::try_from(suspension).map_err(|_| "packet suspension exceeds u32")?;
                let matching = proofs.iter().enumerate().filter(|(_, proof)| proof.source_identity()
                    == (flow.callable.module.as_str(), flow.callable.surface.item, flow.callable.surface.method, suspension))
                    .collect::<Vec<_>>();
                let [(index, proof)] = matching.as_slice() else { return Err("packet receipt lost an original await proof".into()) };
                if proof.source_mode() != flow.mode { return Err("packet receipt changed source execution mode".into()); }
                covered.push(u32::try_from(*index).map_err(|_| "packet proof exceeds u32")?);
                coordinates.push((flow.callable.module.clone(), flow.callable.surface.item, flow.callable.surface.method, suspension));
            }
        }
        let receiver = &rederived.flows[2 - rederived.producer];
        let [control, destructure, ..] = receiver.controls.as_slice() else { return Err("packet receipt lost implicit receive or destructure control".into()) };
        if control.kind != crate::async_control_flow::SourceControlKind::For
            || destructure.kind != (crate::async_control_flow::SourceControlKind::Match { arms: 1 }) {
            return Err("packet receive/destructure source control changed".into());
        }
        let bindings = receiver.bindings_read_by(control.selector)?;
        let [endpoint] = bindings.as_slice() else { return Err("implicit packet receive has ambiguous endpoint binding".into()) };
        Ok(BoundReceipt { kind: witchy_canonical_source::scalar_region::RegionKind::PacketScope, root, companion: companion_item,
            graph_digest: *blake3::hash(format!("{rederived:?}").as_bytes()).as_bytes(),
            companion_digest: *blake3::hash(format!("{companion:?}").as_bytes()).as_bytes(), awaits: covered,
            root_digest: *blake3::hash(format!("{root_function:?}").as_bytes()).as_bytes(),
            coordinates, receive: Some((receiver.callable.module.clone(), receiver.callable.surface.item,
                receiver.callable.surface.method, control.id.0, control.selector.0, endpoint.0)) })
    }
    pub(crate) fn verify(&self, checked: &CheckedModule, candidate: &witchy_syntax::ast::Module,
        catalog: &RuntimeDeclarationCatalog) -> Result<(), String> {
        let rederived = PacketRegionSource::derive(checked, catalog)
            .ok_or("closed packet region lost exact original source coverage")?;
        if rederived != self.source || format!("{rederived:?}") != format!("{:?}", self.source) {
            return Err("closed packet region has stale source binding".into());
        }
        crate::checked_scalar_contract::scheduler_contract(checked.module(),
            checked.typed_async_source().ok_or("missing original packet source")?.module(), catalog)?;
        let original = root_recipe(checked.module()).ok_or("original packet root is not an inert lazy recipe")?;
        if original != &self.original_recipe || format!("{original:?}") != format!("{:?}", self.original_recipe) {
            return Err("closed packet root construction changed".into());
        }
        let expected = companion(&rederived).ok_or("could not rederive closed packet companion")?;
        let definitions = candidate.items.iter().filter_map(|item| match item {
            Item::Function(function) if function.name == COMPANION => Some(function), _ => None,
        }).collect::<Vec<_>>();
        let [actual] = definitions.as_slice() else { return Err("closed packet companion is missing or duplicated".into()) };
        if !identical_functions(actual, &expected) { return Err("closed packet companion differs from original source recipe".into()); }
        let root = candidate.items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function), _ => None,
        }).ok_or("closed packet root is missing")?;
        let expected_body = witchy_syntax::ast::Block {
            stmts: vec![Stmt::Expr(Expr::Call { name: COMPANION.into(), args: wrapper_arguments(original, &rederived) })],
            lines: checked.module().items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function.body.lines.clone()), _ => None,
            }).ok_or("original packet root is missing")?, region: None,
        };
        if root.body != expected_body || format!("{:?}", root.body) != format!("{expected_body:?}") {
            return Err("closed packet VM root differs from its lazy recipe binding".into());
        }
        let mut original_root = checked.module().items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function.clone()), _ => None,
        }).ok_or("original root declaration is missing")?;
        original_root.body = expected_body;
        if !identical_functions(root, &original_root) { return Err("closed packet VM root signature or origin changed".into()); }
        Ok(())
    }
}

pub(crate) fn prepare(checked: &CheckedModule, typed: TypedModule, catalog: &RuntimeDeclarationCatalog)
    -> Result<(TypedModule, Option<Preparation>), String> {
    if !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::ClosureElide)
        || checked.module().items.iter().any(|item| matches!(item, Item::Function(function) if function.name == COMPANION)) {
        return Ok((typed, None));
    }
    let Some(source) = PacketRegionSource::derive(checked, catalog) else { return Ok((typed, None)) };
    if crate::checked_scalar_contract::scheduler_contract(checked.module(),
        checked.typed_async_source().ok_or("missing original packet source")?.module(), catalog).is_err() {
        return Ok((typed, None));
    }
    let Some(original_recipe) = root_recipe(checked.module()).cloned() else { return Ok((typed, None)) };
    let Some(companion) = companion(&source) else { return Ok((typed, None)) };
    let preparation = Preparation { source, original_recipe };
    let mut candidate = typed.module().clone();
    let Some(root) = candidate.items.iter_mut().find_map(|item| match item {
        Item::Function(function) if function.name == "main" => Some(function), _ => None,
    }) else { return Err("checked packet root lost runtime projection".into()) };
    root.body.stmts = vec![Stmt::Expr(Expr::Call { name: COMPANION.into(),
        args: wrapper_arguments(&preparation.original_recipe, &preparation.source) })];
    candidate.items.push(Item::Function(companion));

    if candidate.items.len() != typed.module().items.len() + 1 { return Err("packet recipe changed declaration coverage".into()); }
    for (original, candidate) in typed.module().items.iter().zip(&candidate.items) {
        if matches!(original, Item::Function(function) if function.name == "main") { continue; }
        if original != candidate || format!("{original:?}") != format!("{candidate:?}") {
            return Err("packet recipe changed an original executable declaration".into());
        }
    }
    preparation.verify(checked, &candidate, catalog)?;
    let candidate = crate::traits::lower_checked(candidate)?;
    let candidate = typed.reannotate_unless_equal(CheckedAnnotation::Runtime, candidate)
        .map_err(|error| error.to_string())?;
    Ok((candidate, Some(preparation)))
}
