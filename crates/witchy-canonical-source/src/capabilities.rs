//! Representation selection from canonical checked capability declarations.
//! This projects source types; only host-minted externrefs carry authority.

use std::collections::{BTreeMap, BTreeSet};
use witchy_syntax::ast::{Type, TypeDef};

/// The migrated host capabilities represented as unforgeable `externref`
/// values on the compiled backend.
pub fn externref_cap_name(name: &str) -> Option<&'static str> {
    match name {
        "Dir" => Some("Dir"),
        "File" => Some("File"),
        "Env" => Some("Env"),
        "Exec" => Some("Exec"),
        "Net" => Some("Net"),
        "Fetch" => Some("Fetch"),
        "Socket" => Some("Socket"),
        "Listener" => Some("Listener"),
        "Secret" => Some("Secret"),
        _ => None,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityKind { Console, Dir, Env, File, Net, Listener, Socket, BuildOut, BuildRead, BuildEnv, BuildExec, BuildNet, Exec, Fetch, Clock, SecretStore, Secret }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityRepresentation {
    Direct(CapabilityKind),
    Nullable(CapabilityKind),
}

pub fn representation(
    source: &Type,
    definitions: &BTreeMap<String, TypeDef>,
) -> Option<CapabilityRepresentation> {
    if let Type::Named(name, arguments) = source.unqualified()
        && name == "Option" && arguments.len() == 1
    {
        // Nested Option has more than two states and cannot use one nullable
        // reference. Do not recursively flatten nullable representations.
        return direct_kind(&arguments[0], definitions, &mut BTreeSet::new())
            .map(CapabilityRepresentation::Nullable);
    }
    direct_kind(source, definitions, &mut BTreeSet::new())
        .map(CapabilityRepresentation::Direct)
}

pub fn brand_payload<'a>(
    source: &Type,
    definitions: &'a BTreeMap<String, TypeDef>,
) -> Option<&'a Type> {
    let Type::Named(name, arguments) = source.unqualified() else { return None };
    if !arguments.is_empty() { return None; }
    let definition = definitions.get(name)?;
    if !definition.sealed || !definition.is_capability || !definition.params.is_empty()
        || definition.variants.len() != 1
    {
        return None;
    }
    let variant = &definition.variants[0];
    if variant.name != definition.name || !variant.field_names.is_empty()
        || variant.fields.len() != 1
    {
        return None;
    }
    Some(&variant.fields[0])
}

fn direct_kind(
    source: &Type,
    definitions: &BTreeMap<String, TypeDef>,
    seen: &mut BTreeSet<String>,
) -> Option<CapabilityKind> {
    let Type::Named(name, arguments) = source.unqualified() else { return None };
    match name.as_str() {
        "Exec" if arguments.is_empty() => return Some(CapabilityKind::Exec),
        "Fetch" if arguments.is_empty() => return Some(CapabilityKind::Fetch),
        "Clock" if arguments.is_empty() => return Some(CapabilityKind::Clock),
        "SecretStore" if arguments.is_empty() => return Some(CapabilityKind::SecretStore),
        "Secret" => return Some(CapabilityKind::Secret),
        "BuildOut" if arguments.is_empty() => return Some(CapabilityKind::BuildOut),
        "BuildRead" if arguments.is_empty() => return Some(CapabilityKind::BuildRead),
        "BuildEnv" if arguments.is_empty() => return Some(CapabilityKind::BuildEnv),
        "BuildExec" if arguments.is_empty() => return Some(CapabilityKind::BuildExec),
        "BuildNet" if arguments.is_empty() => return Some(CapabilityKind::BuildNet),
        "Console" if arguments.is_empty() => return Some(CapabilityKind::Console),
        "Dir" => return Some(CapabilityKind::Dir),
        "Env" if arguments.is_empty() => return Some(CapabilityKind::Env),
        "File" => return Some(CapabilityKind::File),
        "Net" => return Some(CapabilityKind::Net),
        "Listener" if arguments.is_empty() => return Some(CapabilityKind::Listener),
        "Socket" if arguments.is_empty() => return Some(CapabilityKind::Socket),
        _ => {}
    }
    if !seen.insert(name.clone()) { return None; }
    direct_kind(brand_payload(source, definitions)?, definitions, seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::{ast::Item, parser::parse_module};

    fn definitions(source: &str) -> BTreeMap<String, TypeDef> {
        parse_module(source).expect("source syntax").items.into_iter()
            .filter_map(|item| match item {
                Item::Type(definition) => Some((definition.name.clone(), definition)),
                _ => None,
            }).collect()
    }

    fn named(name: &str) -> Type { Type::Named(name.to_owned(), Vec::new()) }

    #[test]
    fn registry_capabilities_keep_root_kinds_and_nullable_secret_distinct() {
        let definitions = BTreeMap::new();
        for (name, kind) in [("Clock", CapabilityKind::Clock), ("SecretStore", CapabilityKind::SecretStore), ("Secret", CapabilityKind::Secret)] {
            assert_eq!(representation(&named(name), &definitions), Some(CapabilityRepresentation::Direct(kind)));
        }
        let secret = Type::Named("Secret".into(), vec![named("Seal")]);
        assert_eq!(representation(&secret, &definitions), Some(CapabilityRepresentation::Direct(CapabilityKind::Secret)));
        let optional = Type::Named("Option".into(), vec![secret]);
        assert_eq!(representation(&optional, &definitions), Some(CapabilityRepresentation::Nullable(CapabilityKind::Secret)));
        assert_eq!(representation(&Type::Named("Option".into(), vec![optional]), &definitions), None);
        assert_eq!(representation(&Type::Named("SecretStore".into(), vec![named("Int")]), &definitions), None);
    }

    #[test]
    fn checked_capability_representation_keeps_absence_distinct() {
        let definitions = definitions("capability ConfigDir from Dir\ncapability Nested from ConfigDir\n");
        let brand = named("Nested");
        assert_eq!(representation(&brand, &definitions), Some(CapabilityRepresentation::Direct(CapabilityKind::Dir)));
        let optional = Type::Named("Option".into(), vec![brand]);
        assert_eq!(representation(&optional, &definitions), Some(CapabilityRepresentation::Nullable(CapabilityKind::Dir)));
        let nested_optional = Type::Named("Option".into(), vec![optional]);
        assert_eq!(representation(&nested_optional, &definitions), None);
    }

    #[test]
    fn checked_capability_representation_rejects_non_brands_and_cycles() {
        let mut definitions = definitions("capability ConfigDir from Dir\ncapability A from B\ncapability B from A\n");
        assert_eq!(representation(&named("A"), &definitions), None);
        assert_eq!(representation(&named("Missing"), &definitions), None);
        definitions.get_mut("ConfigDir").unwrap().is_capability = false;
        assert_eq!(representation(&named("ConfigDir"), &definitions), None);
        let definition = definitions.get_mut("ConfigDir").unwrap();
        definition.is_capability = true;
        definition.variants[0].fields.push(named("Dir"));
        assert_eq!(representation(&named("ConfigDir"), &definitions), None);
    }

    #[test]
    fn carried_net_record_is_an_aggregate_not_a_host_capability_brand() {
        let definitions = definitions("capability Postgres:\n    net: Net[Connect, Tcp]\n    table: String\ncapability BrandedNet from Net\n");
        let postgres = named("Postgres");
        assert!(definitions["Postgres"].sealed);
        assert!(definitions["Postgres"].is_capability);
        assert_eq!(brand_payload(&postgres, &definitions), None);
        assert_eq!(representation(&postgres, &definitions), None);
        assert_eq!(representation(&Type::Named("Option".into(), vec![postgres.clone()]), &definitions), None);
        assert!(crate::aggregates::is_v1_field_type(&postgres, &definitions));
        let fields = &definitions["Postgres"].variants[0].fields;
        assert_eq!(fields.len(), 2);
        assert_eq!(representation(&fields[0], &definitions), Some(CapabilityRepresentation::Direct(CapabilityKind::Net)));
        assert_eq!(fields[1], named("String"));
        assert_eq!(representation(&named("BrandedNet"), &definitions), Some(CapabilityRepresentation::Direct(CapabilityKind::Net)));
    }
}
