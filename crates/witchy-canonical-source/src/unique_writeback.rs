use witchy_syntax::ast::Type;

pub fn scalar_list(ty: &Type) -> bool {
    matches!(ty, Type::Named(name, arguments) if name == "List"
        && matches!(arguments.as_slice(), [Type::Named(element, parameters)]
            if parameters.is_empty() && matches!(element.as_str(), "Int" | "Bool" | "Float")))
}
