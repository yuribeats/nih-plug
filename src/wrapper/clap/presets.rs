//! Factory presets for CLAP hosts: a preset-discovery factory with one provider per plugin that
//! has [`Plugin::factory_presets()`][crate::prelude::Plugin::factory_presets()]. Each provider
//! declares one location inside the plugin (`CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN`) and lists
//! the presets with their index as the load key; the plugin's `clap.preset-load` extension (in
//! the wrapper) loads them by that key. clap-wrapper reads the same factory to publish the
//! presets as Audio Unit factory presets.

use clap_sys::factory::preset_discovery::{
    clap_preset_discovery_factory, clap_preset_discovery_indexer,
    clap_preset_discovery_location, clap_preset_discovery_location_kind,
    clap_preset_discovery_metadata_receiver, clap_preset_discovery_provider,
    clap_preset_discovery_provider_descriptor, CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
    CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
};
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use clap_sys::version::CLAP_VERSION;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

use crate::prelude::ClapPlugin;

/// One plugin's presets, as C strings that live as long as the library.
pub struct PresetProvider {
    descriptor: clap_preset_discovery_provider_descriptor,
    // The descriptor points into these.
    _id: CString,
    name: CString,
    _vendor: CString,
    plugin_id: CString,
    /// (name, load key) per preset.
    presets: Vec<(CString, CString)>,
}

// SAFETY: the raw pointers in `descriptor` point into the `CString`s of the same object, which
// are never changed after it is made.
unsafe impl Send for PresetProvider {}
unsafe impl Sync for PresetProvider {}

impl PresetProvider {
    /// The provider of `P`'s factory presets, or `None` when it has none.
    pub fn for_plugin<P: ClapPlugin>() -> Option<Self> {
        let presets = P::factory_presets();
        if presets.is_empty() {
            return None;
        }
        let cstr = |s: &str| CString::new(s.replace('\0', "")).unwrap_or_default();
        let id = cstr(&format!("{}.factory-presets", P::CLAP_ID));
        let name = cstr(&format!("{} factory presets", P::NAME));
        let vendor = cstr(P::VENDOR);
        let descriptor = clap_preset_discovery_provider_descriptor {
            clap_version: CLAP_VERSION,
            id: id.as_ptr(),
            name: name.as_ptr(),
            vendor: vendor.as_ptr(),
        };
        Some(PresetProvider {
            descriptor,
            _id: id,
            name,
            _vendor: vendor,
            plugin_id: cstr(P::CLAP_ID),
            presets: presets
                .iter()
                .enumerate()
                .map(|(i, p)| (cstr(&p.name), cstr(&i.to_string())))
                .collect(),
        })
    }
}

static PROVIDERS: OnceLock<Vec<PresetProvider>> = OnceLock::new();

/// The library's preset-discovery factory, made from `providers` on first use. Returns null when
/// no plugin of the library has factory presets.
pub fn factory(providers: fn() -> Vec<Option<PresetProvider>>) -> *const c_void {
    let all = PROVIDERS.get_or_init(|| providers().into_iter().flatten().collect());
    if all.is_empty() {
        std::ptr::null()
    } else {
        &FACTORY as *const _ as *const c_void
    }
}

fn providers() -> &'static [PresetProvider] {
    PROVIDERS.get().map(Vec::as_slice).unwrap_or(&[])
}

static FACTORY: clap_preset_discovery_factory = clap_preset_discovery_factory {
    count: Some(count),
    get_descriptor: Some(get_descriptor),
    create: Some(create),
};

unsafe extern "C" fn count(_factory: *const clap_preset_discovery_factory) -> u32 {
    providers().len() as u32
}

unsafe extern "C" fn get_descriptor(
    _factory: *const clap_preset_discovery_factory,
    index: u32,
) -> *const clap_preset_discovery_provider_descriptor {
    match providers().get(index as usize) {
        Some(p) => &p.descriptor,
        None => std::ptr::null(),
    }
}

/// A provider made for an indexer; `provider` must stay the first field (the host's pointer to
/// it is also the pointer to this).
#[repr(C)]
struct Instance {
    provider: clap_preset_discovery_provider,
    indexer: *const clap_preset_discovery_indexer,
    entry: &'static PresetProvider,
}

unsafe extern "C" fn create(
    _factory: *const clap_preset_discovery_factory,
    indexer: *const clap_preset_discovery_indexer,
    provider_id: *const c_char,
) -> *const clap_preset_discovery_provider {
    if indexer.is_null() || provider_id.is_null() {
        return std::ptr::null();
    }
    let wanted = CStr::from_ptr(provider_id);
    let Some(entry) = providers()
        .iter()
        .find(|p| CStr::from_ptr(p.descriptor.id) == wanted)
    else {
        return std::ptr::null();
    };
    let instance = Box::new(Instance {
        provider: clap_preset_discovery_provider {
            desc: &entry.descriptor,
            provider_data: std::ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            get_metadata: Some(get_metadata),
            get_extension: Some(get_extension),
        },
        indexer,
        entry,
    });
    let raw = Box::into_raw(instance);
    (*raw).provider.provider_data = raw as *mut c_void;
    &(*raw).provider
}

unsafe fn instance<'a>(provider: *const clap_preset_discovery_provider) -> Option<&'a Instance> {
    if provider.is_null() || (*provider).provider_data.is_null() {
        return None;
    }
    Some(&*((*provider).provider_data as *const Instance))
}

unsafe extern "C" fn init(provider: *const clap_preset_discovery_provider) -> bool {
    let Some(me) = instance(provider) else {
        return false;
    };
    let indexer = &*me.indexer;
    let Some(declare) = indexer.declare_location else {
        return false;
    };
    let location = clap_preset_discovery_location {
        flags: CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
        name: me.entry.name.as_ptr(),
        kind: CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
        location: std::ptr::null(),
    };
    declare(me.indexer, &location)
}

unsafe extern "C" fn destroy(provider: *const clap_preset_discovery_provider) {
    if provider.is_null() || (*provider).provider_data.is_null() {
        return;
    }
    drop(Box::from_raw((*provider).provider_data as *mut Instance));
}

unsafe extern "C" fn get_metadata(
    provider: *const clap_preset_discovery_provider,
    location_kind: clap_preset_discovery_location_kind,
    _location: *const c_char,
    receiver: *const clap_preset_discovery_metadata_receiver,
) -> bool {
    let Some(me) = instance(provider) else {
        return false;
    };
    if receiver.is_null() || location_kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN {
        return false;
    }
    let r = &*receiver;
    let (Some(begin), Some(add_plugin_id)) = (r.begin_preset, r.add_plugin_id) else {
        return false;
    };
    let abi = c"clap";
    for (name, key) in &me.entry.presets {
        if !begin(receiver, name.as_ptr(), key.as_ptr()) {
            break;
        }
        let id = clap_universal_plugin_id {
            abi: abi.as_ptr(),
            id: me.entry.plugin_id.as_ptr(),
        };
        add_plugin_id(receiver, &id);
        if let Some(set_flags) = r.set_flags {
            set_flags(receiver, CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT);
        }
    }
    true
}

unsafe extern "C" fn get_extension(
    _provider: *const clap_preset_discovery_provider,
    _extension_id: *const c_char,
) -> *const c_void {
    std::ptr::null()
}
