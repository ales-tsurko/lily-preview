//! Processor registry.

use std::borrow::Cow;
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::instrument::{
    BUILTIN_NONE_ID, EffectRuntimeSpec, InstrumentRuntimeContext, InstrumentRuntimeSpec,
    ProcessorDescriptor, ProcessorKind, RuntimeFactoryError, SlotState,
};

/// Stable processor identifier type used by the registry catalog.
pub type Id = &'static str;

/// Processor role in the mixer graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Instrument slot processor.
    Instrument,
    /// Effect slot processor.
    Effect,
}

/// Backend family that provides the processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// Built into the application.
    BuiltIn,
    /// CLAP plugin backend.
    Clap,
    /// VST3 plugin backend.
    Vst3,
}

/// One discoverable processor entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable processor id.
    pub id: Cow<'static, str>,
    /// User-visible processor name.
    pub name: Cow<'static, str>,
    /// Processor role.
    pub role: Role,
    /// Backend family.
    pub backend: Backend,
    /// Static processor descriptor.
    pub descriptor: &'static ProcessorDescriptor,
    factory: Factory,
}

#[derive(Debug, Clone, Copy)]
struct Factory {
    is_empty: bool,
    create_instrument: Option<CreateInstrument>,
    create_effect: Option<CreateEffect>,
}

/// Instrument runtime factory callback.
pub type CreateInstrument = fn(
    &SlotState,
    &InstrumentRuntimeContext<'_>,
) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError>;
/// Effect runtime factory callback.
pub type CreateEffect = fn(&SlotState) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError>;

impl Entry {
    /// Creates an empty built-in instrument entry.
    #[must_use]
    pub const fn empty_builtin(
        id: Id,
        name: &'static str,
        descriptor: &'static ProcessorDescriptor,
    ) -> Self {
        Self {
            id: Cow::Borrowed(id),
            name: Cow::Borrowed(name),
            role: Role::Instrument,
            backend: Backend::BuiltIn,
            descriptor,
            factory: Factory {
                is_empty: true,
                create_instrument: None,
                create_effect: None,
            },
        }
    }

    /// Creates a built-in instrument entry.
    #[must_use]
    pub const fn builtin_instrument(
        id: Id,
        name: &'static str,
        descriptor: &'static ProcessorDescriptor,
        create: CreateInstrument,
    ) -> Self {
        Self {
            id: Cow::Borrowed(id),
            name: Cow::Borrowed(name),
            role: Role::Instrument,
            backend: Backend::BuiltIn,
            descriptor,
            factory: Factory {
                is_empty: false,
                create_instrument: Some(create),
                create_effect: None,
            },
        }
    }

    /// Creates a built-in instrument catalog entry without a runtime factory.
    #[must_use]
    pub const fn builtin_instrument_descriptor(
        id: Id,
        name: &'static str,
        descriptor: &'static ProcessorDescriptor,
    ) -> Self {
        Self {
            id: Cow::Borrowed(id),
            name: Cow::Borrowed(name),
            role: Role::Instrument,
            backend: Backend::BuiltIn,
            descriptor,
            factory: Factory {
                is_empty: false,
                create_instrument: None,
                create_effect: None,
            },
        }
    }

    /// Creates a built-in effect entry.
    #[must_use]
    pub const fn builtin_effect(
        id: Id,
        name: &'static str,
        descriptor: &'static ProcessorDescriptor,
        create: CreateEffect,
    ) -> Self {
        Self {
            id: Cow::Borrowed(id),
            name: Cow::Borrowed(name),
            role: Role::Effect,
            backend: Backend::BuiltIn,
            descriptor,
            factory: Factory {
                is_empty: false,
                create_instrument: None,
                create_effect: Some(create),
            },
        }
    }

    /// Creates a dynamically discovered plugin effect entry.
    #[must_use]
    pub fn plugin_effect(
        id: String,
        name: String,
        backend: Backend,
        descriptor: &'static ProcessorDescriptor,
        create: CreateEffect,
    ) -> Self {
        Self {
            id: Cow::Owned(id),
            name: Cow::Owned(name),
            role: Role::Effect,
            backend,
            descriptor,
            factory: Factory {
                is_empty: false,
                create_instrument: None,
                create_effect: Some(create),
            },
        }
    }

    /// Creates a dynamically discovered plugin instrument entry.
    #[must_use]
    pub fn plugin_instrument(
        id: String,
        name: String,
        backend: Backend,
        descriptor: &'static ProcessorDescriptor,
        create: CreateInstrument,
    ) -> Self {
        Self {
            id: Cow::Owned(id),
            name: Cow::Owned(name),
            role: Role::Instrument,
            backend,
            descriptor,
            factory: Factory {
                is_empty: false,
                create_instrument: Some(create),
                create_effect: None,
            },
        }
    }
}

const NONE_DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
    name: "None",
    params: &[],
    editor: None,
};

const NONE: Entry = Entry::empty_builtin(BUILTIN_NONE_ID, "None", &NONE_DESCRIPTOR);

static ENTRIES: OnceLock<RwLock<Vec<Entry>>> = OnceLock::new();

fn entries() -> &'static RwLock<Vec<Entry>> {
    ENTRIES.get_or_init(|| RwLock::new(vec![NONE]))
}

fn registry_read() -> std::sync::RwLockReadGuard<'static, Vec<Entry>> {
    entries()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn registry_write() -> std::sync::RwLockWriteGuard<'static, Vec<Entry>> {
    entries()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Registers processor entries supplied by a statically linked backend.
pub fn register(entries_to_register: impl IntoIterator<Item = Entry>) {
    let mut registered = registry_write();
    for entry in entries_to_register {
        if let Some(existing) = registered
            .iter_mut()
            .find(|existing| existing.id == entry.id)
        {
            *existing = entry;
        } else {
            registered.push(entry);
        }
    }
}

/// Returns the full processor catalog.
#[must_use]
pub fn all() -> Vec<Entry> {
    registry_read().clone()
}

/// Returns one catalog entry by stable id.
#[must_use]
pub fn entry(id: &str) -> Option<Entry> {
    registry_read()
        .iter()
        .find(|entry| entry.id.as_ref() == id)
        .cloned()
}

/// Resolves one persisted processor kind into a catalog entry.
#[must_use]
pub fn resolve(kind: &ProcessorKind) -> Option<Entry> {
    match kind {
        ProcessorKind::BuiltIn { processor_id } => entry(processor_id),
        ProcessorKind::Plugin { plugin_id } => entry(plugin_id),
    }
}

#[must_use]
pub(crate) fn is_empty(kind: &ProcessorKind) -> bool {
    resolve(kind).is_some_and(|entry| entry.factory.is_empty)
}

pub(crate) fn create_instrument_runtime(
    slot: &SlotState,
    context: &InstrumentRuntimeContext<'_>,
) -> Result<Option<InstrumentRuntimeSpec>, RuntimeFactoryError> {
    let Some(factory) = resolve(&slot.kind).and_then(|entry| entry.factory.create_instrument)
    else {
        return Ok(None);
    };
    factory(slot, context)
}

pub(crate) fn create_effect_runtime(
    slot: &SlotState,
) -> Result<Option<EffectRuntimeSpec>, RuntimeFactoryError> {
    let Some(factory) = resolve(&slot.kind).and_then(|entry| entry.factory.create_effect) else {
        return Ok(None);
    };
    factory(slot)
}

#[cfg(test)]
mod tests {
    use super::{Backend, Entry, Role, all, entry, is_empty, register};
    use crate::instrument::{
        BUILTIN_GAIN_ID, ProcessorDescriptor, ProcessorKind, ProcessorState, SlotState,
    };

    const TEST_DESCRIPTOR: ProcessorDescriptor = ProcessorDescriptor {
        name: "Test Gain",
        params: &[],
        editor: None,
    };

    #[test]
    fn external_builtins_can_register_in_catalog() {
        register([Entry::builtin_effect(
            BUILTIN_GAIN_ID,
            "Gain",
            &TEST_DESCRIPTOR,
            |_| Ok(None),
        )]);
        let entries = all();

        assert!(entries.iter().any(|entry| entry.id == BUILTIN_GAIN_ID
            && entry.role == Role::Effect
            && entry.backend == Backend::BuiltIn));
    }

    #[test]
    fn built_in_lookup_resolves_from_kind() {
        register([Entry::builtin_effect(
            BUILTIN_GAIN_ID,
            "Gain",
            &TEST_DESCRIPTOR,
            |_| Ok(None),
        )]);
        let kind = ProcessorKind::BuiltIn {
            processor_id: BUILTIN_GAIN_ID.to_string(),
        };

        assert!(!is_empty(&kind));
        assert_eq!(
            entry(BUILTIN_GAIN_ID).map(|entry| entry.name.into_owned()),
            Some("Gain".to_string())
        );
    }

    #[test]
    fn none_remains_the_builtin_empty_slot() {
        let slot = SlotState::default();

        assert!(slot.is_empty());
        assert_eq!(slot.state, ProcessorState::default());
    }

    #[test]
    fn dynamic_plugin_entry_can_be_registered_and_resolved() {
        register([Entry::plugin_effect(
            "clap:/tmp/test.clap#org.test.gain".to_string(),
            "Test Gain".to_string(),
            Backend::Clap,
            &TEST_DESCRIPTOR,
            |_| Ok(None),
        )]);

        let kind = ProcessorKind::Plugin {
            plugin_id: "clap:/tmp/test.clap#org.test.gain".to_string(),
        };
        let resolved = super::resolve(&kind).expect("dynamic plugin should resolve");

        assert_eq!(resolved.id.as_ref(), "clap:/tmp/test.clap#org.test.gain");
        assert_eq!(resolved.name.as_ref(), "Test Gain");
        assert_eq!(resolved.role, Role::Effect);
        assert_eq!(resolved.backend, Backend::Clap);
    }
}
