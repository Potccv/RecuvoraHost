//! Bounded metadata/schema validation before extension registration.
use super::{ExtensionDefinition, MONITORING_VIEW_CAPABILITY, MONITORING_VIEW_METHOD};
use super::{ExtensionError, ExtensionKind, ExtensionMetadata};
use crate::protocol;
use std::collections::BTreeSet;

pub(super) fn validate_metadata(
    definition: &ExtensionDefinition,
    metadata: &ExtensionMetadata,
) -> Result<(), ExtensionError> {
    if metadata.contracts.len() > 32
        || metadata.capabilities.len() > 64
        || metadata.workspaces.len() > 64
    {
        return Err(ExtensionError::Protocol("metadata limit exceeded".into()));
    }
    let mut contracts = BTreeSet::new();
    for contract in &metadata.contracts {
        if !protocol::valid_id(&contract.id)
            || contract.version == 0
            || contract.methods.is_empty()
            || contract.methods.len() > 32
            || !contracts.insert((&contract.id, contract.version))
        {
            return Err(ExtensionError::Protocol(
                "invalid or duplicate contract".into(),
            ));
        }
        let mut methods = BTreeSet::new();
        for method in &contract.methods {
            if contract.id == "recuvora.repair"
                && (definition.kind != ExtensionKind::Node
                    || !matches!(
                        method.name.as_str(),
                        "inspect" | "verify" | "reconcile" | "execute_script"
                    )
                    || method.read_only != (method.name != "execute_script"))
            {
                return Err(ExtensionError::Rejected(
                    "invalid repair node method".into(),
                ));
            }
            if !protocol::valid_id(&method.name) || !methods.insert(&method.name) {
                return Err(ExtensionError::Protocol(
                    "invalid or duplicate method".into(),
                ));
            }
            protocol::validate_schema(&method.input_schema)?;
            protocol::validate_schema(&method.output_schema)?;
            // A malformed optional view must not disable the plugin itself.
            // It remains uncallable unless monitoring_view_registration later
            // accepts the unique read-only declaration and trusted allowlist.
            if definition.kind == ExtensionKind::Plugin
                && !method.read_only
                && !(metadata
                    .capabilities
                    .iter()
                    .any(|capability| capability == MONITORING_VIEW_CAPABILITY)
                    && method.name == MONITORING_VIEW_METHOD)
            {
                return Err(ExtensionError::Rejected(
                    "new plugin methods currently must be read-only".into(),
                ));
            }
        }
    }
    for list in [&metadata.capabilities, &metadata.workspaces] {
        let mut seen = BTreeSet::new();
        for value in list {
            if !protocol::valid_id(value) || !seen.insert(value) {
                return Err(ExtensionError::Protocol(
                    "invalid or duplicate capability/workspace".into(),
                ));
            }
        }
    }
    Ok(())
}
