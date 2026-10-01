//! Validate provider uniqueness and compute a deterministic dependency order.
use super::{FrameworkError, LifecycleOptions, ModuleMetadata};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn order(
    options: &LifecycleOptions,
    metadata: &[&ModuleMetadata],
) -> Result<Vec<usize>, FrameworkError> {
    if options.start_timeout.is_zero()
        || options.stop_timeout.is_zero()
        || options.cleanup_timeout.is_zero()
    {
        return Err(FrameworkError::InvalidConfiguration(
            "lifecycle deadlines must be positive".into(),
        ));
    }
    if [
        options.start_timeout,
        options.stop_timeout,
        options.cleanup_timeout,
    ]
    .iter()
    .any(|duration| std::time::Instant::now().checked_add(*duration).is_none())
    {
        return Err(FrameworkError::InvalidConfiguration(
            "lifecycle deadline is out of range".into(),
        ));
    }
    let mut providers = BTreeMap::new();
    for (index, entry) in metadata.iter().enumerate() {
        for service in &entry.provides {
            if providers.insert(service.clone(), index).is_some() {
                return Err(FrameworkError::ProviderConflict(service.clone()));
            }
        }
    }
    let mut dependencies = Vec::new();
    for entry in metadata {
        let mut required = BTreeSet::new();
        for service in &entry.requires {
            let provider =
                providers
                    .get(service)
                    .ok_or_else(|| FrameworkError::MissingDependency {
                        instance: entry.instance.clone(),
                        service: service.clone(),
                    })?;
            required.insert(*provider);
        }
        dependencies.push(required);
    }
    let mut done = BTreeSet::new();
    let mut order = Vec::new();
    while order.len() < metadata.len() {
        let next = (0..metadata.len())
            .find(|index| !done.contains(index) && dependencies[*index].is_subset(&done));
        match next {
            Some(index) => {
                done.insert(index);
                order.push(index);
            }
            None => {
                return Err(FrameworkError::DependencyCycle(
                    (0..metadata.len())
                        .filter(|index| !done.contains(index))
                        .map(|index| metadata[index].instance.clone())
                        .collect(),
                ));
            }
        }
    }
    Ok(order)
}
