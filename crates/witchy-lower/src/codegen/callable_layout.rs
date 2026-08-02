//! RFC-0111 callable-layout validation at still-unsupported boundaries.
//!
//! This module is diagnostic-only. It compares canonical `LayoutId` signatures
//! before the existing fail-closed boundary guard rejects the call; it grants no
//! permission to select a WIR ABI, marshal, reshape, or alter ownership state.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CallableLayoutClassification {
    Exact,
    Mismatch,
    Unknown(LayoutId),
}

pub(super) fn classify_callable_layouts(
    layouts: &LayoutInterner,
    producer: &CallableLayoutSignature,
    boundary: &CallableLayoutSignature,
) -> CallableLayoutClassification {
    let ids = producer
        .parameters()
        .iter()
        .chain(boundary.parameters())
        .copied()
        .flatten()
        .chain(producer.result())
        .chain(boundary.result());
    if let Some(unknown) = ids.into_iter().find(|id| layouts.get(*id).is_none()) {
        return CallableLayoutClassification::Unknown(unknown);
    }
    if producer == boundary {
        CallableLayoutClassification::Exact
    } else {
        CallableLayoutClassification::Mismatch
    }
}

fn layout_name(layout: Option<LayoutId>) -> String {
    layout.map_or_else(|| "ordinary".to_string(), |id| id.to_hex())
}

fn signature_name(signature: &CallableLayoutSignature) -> String {
    let parameters = signature
        .parameters()
        .iter()
        .copied()
        .map(layout_name)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "params=[{parameters}],result={}",
        layout_name(signature.result())
    )
}

impl Codegen<'_> {
    fn callable_layout_signature_for_type(
        &self,
        ty: &Type,
    ) -> Option<CallableLayoutSignature> {
        let Type::Fn(parameters, result, _) = ty.unqualified() else {
            return None;
        };
        Some(CallableLayoutSignature::new(
            parameters
                .iter()
                .map(|parameter| self.specialized_layout_id(parameter))
                .collect(),
            self.specialized_layout_id(result),
        ))
    }

    fn callable_layout_signature_for_access(
        &self,
        access: &witchy_types::access::AccessSignature,
    ) -> CallableLayoutSignature {
        CallableLayoutSignature::new(
            access
                .params()
                .iter()
                .map(|parameter| self.specialized_layout_id(parameter.ty()))
                .collect(),
            self.specialized_layout_id(access.result().ty()),
        )
    }

    fn callable_layout_signature_for_value(
        &self,
        value: &Expr,
    ) -> Option<CallableLayoutSignature> {
        match value {
            Expr::Var(name) if !self.locals.contains_key(name) => {
                self.callable_layouts.get(name).cloned().or_else(|| {
                    self.ast_type_of_expr(value)
                        .as_ref()
                        .and_then(|ty| self.callable_layout_signature_for_type(ty))
                })
            }
            _ => self
                .ast_type_of_expr(value)
                .as_ref()
                .and_then(|ty| self.callable_layout_signature_for_type(ty)),
        }
    }

    fn callable_layout_signature_for_local(
        &self,
        name: &str,
    ) -> Option<CallableLayoutSignature> {
        self.local_types
            .get(name)
            .and_then(|ty| self.callable_layout_signature_for_type(ty))
    }

    fn callable_layout_comparison_detail(
        &self,
        producer: Option<CallableLayoutSignature>,
        boundary: CallableLayoutSignature,
    ) -> String {
        let Some(producer) = producer else {
            return format!(
                "callable-layout=unresolved boundary={}",
                signature_name(&boundary)
            );
        };
        match classify_callable_layouts(&self.specialized_layouts, &producer, &boundary) {
            CallableLayoutClassification::Exact => format!(
                "callable-layout=exact signature={}",
                signature_name(&boundary)
            ),
            CallableLayoutClassification::Mismatch => format!(
                "callable-layout=mismatch producer={} boundary={}",
                signature_name(&producer),
                signature_name(&boundary)
            ),
            CallableLayoutClassification::Unknown(id) => format!(
                "callable-layout=unknown LayoutId {id} producer={} boundary={}",
                signature_name(&producer),
                signature_name(&boundary)
            ),
        }
    }

    pub(super) fn callable_layout_rejection_detail(
        &self,
        expr: &Expr,
    ) -> Option<String> {
        match expr {
            Expr::Call { name, .. } if self.locals.contains_key(name) => {
                let boundary = self.callable_layout_signature_for_access(
                    self.call_access_signature(expr)?,
                );
                boundary.has_specialized_layout().then(|| {
                    self.callable_layout_comparison_detail(
                        self.callable_layout_signature_for_local(name),
                        boundary,
                    )
                })
            }
            Expr::Apply { func, .. } => {
                let boundary = self.callable_layout_signature_for_access(
                    self.call_access_signature(expr)?,
                );
                boundary.has_specialized_layout().then(|| {
                    self.callable_layout_comparison_detail(
                        self.callable_layout_signature_for_value(func),
                        boundary,
                    )
                })
            }
            Expr::Var(name)
                if !self.locals.contains_key(name)
                    && self
                        .callable_layouts
                        .get(name)
                        .is_some_and(CallableLayoutSignature::has_specialized_layout) =>
            {
                let signature = self.callable_layouts.get(name)?.clone();
                Some(self.callable_layout_comparison_detail(
                    Some(signature.clone()),
                    signature,
                ))
            }
            Expr::MethodCall { .. } => Some("callable-layout=method-unresolved".into()),
            Expr::ExistentialCall { .. } => {
                Some("callable-layout=trait-existential-unresolved".into())
            }
            _ => None,
        }
    }

    pub(super) fn specialized_capture_rejection_detail(
        &self,
        params: &[Param],
        body: &Block,
    ) -> Option<String> {
        scan_lambda(params, body)
            .captures()
            .into_iter()
            .find_map(|name| {
                let layout = self
                    .local_types
                    .get(&name)
                    .and_then(|ty| self.specialized_layout_id(ty))?;
                Some(format!(
                    "specialized capture `{name}` has callable-layout LayoutId {layout}"
                ))
            })
    }
}
