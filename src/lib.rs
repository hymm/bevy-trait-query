#![allow(clippy::all)]

use bevy::{
    ecs::{
        component::{ComponentId, StorageType},
        query::{Fetch, FetchState, ReadOnlyWorldQuery, WorldQuery, WorldQueryGats},
        storage::{ComponentSparseSet, SparseSets, Table},
    },
    prelude::*,
    ptr::{Ptr, PtrMut, ThinSlicePtr},
};

#[cfg(test)]
mod tests;

pub mod change_detection;

pub trait DynQuery: 'static {}

pub trait DynQueryMarker<Trait: ?Sized + DynQuery> {
    type Covered: Component;
    /// Casts an untyped pointer to a trait object pointer,
    /// with a vtable corresponding to `Self::Covered`.
    fn cast(_: *mut u8) -> *mut Trait;
}

pub trait RegisterExt {
    fn register_component_as<Trait: ?Sized + DynQuery, C: Component>(&mut self) -> &mut Self
    where
        (C,): DynQueryMarker<Trait, Covered = C>;
}

impl RegisterExt for World {
    fn register_component_as<Trait: ?Sized + DynQuery, C: Component>(&mut self) -> &mut Self
    where
        (C,): DynQueryMarker<Trait, Covered = C>,
    {
        let component_id = self.init_component::<C>();
        let registry = self
            .get_resource_or_insert_with::<TraitImplRegistry<Trait>>(default)
            .into_inner();
        let meta = TraitImplMeta {
            size_bytes: std::mem::size_of::<C>(),
            dyn_ctor: DynCtor { cast: <(C,)>::cast },
        };
        registry.register::<C>(component_id, meta);
        self
    }
}

impl RegisterExt for App {
    fn register_component_as<Trait: ?Sized + DynQuery, C: Component>(&mut self) -> &mut Self
    where
        (C,): DynQueryMarker<Trait, Covered = C>,
    {
        self.world.register_component_as::<Trait, C>();
        self
    }
}

struct TraitImplRegistry<Trait: ?Sized> {
    // Component IDs are stored contiguously so that we can search them quickly.
    components: Vec<ComponentId>,
    meta: Vec<TraitImplMeta<Trait>>,

    table_components: Vec<ComponentId>,
    table_meta: Vec<TraitImplMeta<Trait>>,

    sparse_components: Vec<ComponentId>,
    sparse_meta: Vec<TraitImplMeta<Trait>>,

    sealed: bool,
}

impl<T: ?Sized> Default for TraitImplRegistry<T> {
    #[inline]
    fn default() -> Self {
        Self {
            components: vec![],
            meta: vec![],
            table_components: vec![],
            table_meta: vec![],
            sparse_components: vec![],
            sparse_meta: vec![],
            sealed: false,
        }
    }
}

impl<Trait: ?Sized + DynQuery> TraitImplRegistry<Trait> {
    fn register<C: Component>(&mut self, component: ComponentId, meta: TraitImplMeta<Trait>) {
        if self.sealed {
            // It is not possible to update the `FetchState` for a given system after the game has started,
            // so for explicitness, let's panic instead of having a trait impl silently get forgotten.
            panic!("Cannot register new trait impls after the game has started");
        }
        self.components.push(component);
        self.meta.push(meta);

        use bevy::ecs::component::ComponentStorage;
        match <C as Component>::Storage::STORAGE_TYPE {
            StorageType::Table => {
                self.table_components.push(component);
                self.table_meta.push(meta);
            }
            StorageType::SparseSet => {
                self.sparse_components.push(component);
                self.sparse_meta.push(meta);
            }
        }
    }
    fn seal(&mut self) {
        self.sealed = true;
    }
}

/// Stores data about an impl of a trait
struct TraitImplMeta<Trait: ?Sized> {
    size_bytes: usize,
    dyn_ctor: DynCtor<Trait>,
}

impl<T: ?Sized> Copy for TraitImplMeta<T> {}
impl<T: ?Sized> Clone for TraitImplMeta<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[doc(hidden)]
pub mod imports {
    pub use bevy::ecs::{
        component::{Component, TableStorage},
        query::{ReadOnlyWorldQuery, WorldQuery, WorldQueryGats},
    };
    pub use bevy::ptr::{Ptr, PtrMut};
}

#[macro_export]
macro_rules! impl_dyn_query {
    ($trait:ident) => {
        impl $crate::DynQuery for dyn $trait {}

        impl<T: $trait + $crate::imports::Component> $crate::DynQueryMarker<dyn $trait> for (T,) {
            type Covered = T;
            fn cast(ptr: *mut u8) -> *mut dyn $trait {
                ptr as *mut T as *mut _
            }
        }

        unsafe impl<'w> $crate::imports::WorldQuery for &'w dyn $trait {
            type ReadOnly = Self;
            type State = $crate::DynQueryState<dyn $trait>;

            fn shrink<'wlong: 'wshort, 'wshort>(
                item: bevy::ecs::query::QueryItem<'wlong, Self>,
            ) -> bevy::ecs::query::QueryItem<'wshort, Self> {
                item
            }
        }

        unsafe impl $crate::imports::ReadOnlyWorldQuery for &dyn $trait {}

        impl<'w> $crate::imports::WorldQueryGats<'w> for &dyn $trait {
            type Fetch = $crate::ReadTraitFetch<'w, dyn $trait>;
            type _State = $crate::DynQueryState<dyn $trait>;
        }

        unsafe impl<'w> $crate::imports::WorldQuery for &'w mut dyn $trait {
            type ReadOnly = &'w dyn $trait;
            type State = $crate::DynQueryState<dyn $trait>;

            fn shrink<'wlong: 'wshort, 'wshort>(
                item: bevy::ecs::query::QueryItem<'wlong, Self>,
            ) -> bevy::ecs::query::QueryItem<'wshort, Self> {
                item
            }
        }

        impl<'w> $crate::imports::WorldQueryGats<'w> for &mut dyn $trait {
            type Fetch = $crate::WriteTraitFetch<'w, dyn $trait>;
            type _State = $crate::DynQueryState<dyn $trait>;
        }
    };
}

#[doc(hidden)]
pub struct DynQueryState<Trait: ?Sized> {
    components: Box<[ComponentId]>,
    meta: Box<[TraitImplMeta<Trait>]>,
}

impl<Trait: ?Sized + DynQuery> FetchState for DynQueryState<Trait> {
    fn init(world: &mut World) -> Self {
        #[cold]
        fn error<T: ?Sized + 'static>() -> ! {
            panic!(
                "no components found matching `{}`, did you forget to register them?",
                std::any::type_name::<T>()
            )
        }

        let mut registry = world
            .get_resource_mut::<TraitImplRegistry<Trait>>()
            .unwrap_or_else(|| error::<Trait>());
        registry.seal();
        Self {
            components: registry.components.clone().into_boxed_slice(),
            meta: registry.meta.clone().into_boxed_slice(),
        }
    }
    fn matches_component_set(&self, set_contains_id: &impl Fn(ComponentId) -> bool) -> bool {
        self.components.iter().copied().any(set_contains_id)
    }
}

/// Turns an untyped pointer into a trait object pointer,
/// for a specific erased concrete type.
struct DynCtor<Trait: ?Sized> {
    cast: unsafe fn(*mut u8) -> *mut Trait,
}

impl<T: ?Sized> Copy for DynCtor<T> {}
impl<T: ?Sized> Clone for DynCtor<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Trait: ?Sized> DynCtor<Trait> {
    unsafe fn cast(self, ptr: Ptr) -> &Trait {
        &*(self.cast)(ptr.as_ptr())
    }
    unsafe fn cast_mut(self, ptr: PtrMut) -> &mut Trait {
        &mut *(self.cast)(ptr.as_ptr())
    }
}

pub struct ReadTraitFetch<'w, Trait: ?Sized + DynQuery> {
    size_bytes: usize,
    dyn_ctor: Option<DynCtor<Trait>>,
    storage: Option<StorageType>,
    // T::Storage = TableStorage
    table_components: Option<Ptr<'w>>,
    entity_table_rows: Option<ThinSlicePtr<'w, usize>>,
    // T::Storage = SparseStorage
    sparse_sets: &'w SparseSets,
    component_sparse_set: Option<&'w ComponentSparseSet>,
    entities: Option<ThinSlicePtr<'w, Entity>>,
}

unsafe impl<'w, Trait: ?Sized + DynQuery> Fetch<'w> for ReadTraitFetch<'w, Trait> {
    type Item = &'w Trait;
    type State = DynQueryState<Trait>;

    unsafe fn init(
        world: &'w World,
        _state: &Self::State,
        _last_change_tick: u32,
        _change_tick: u32,
    ) -> Self {
        Self {
            size_bytes: 0,
            dyn_ctor: None,
            storage: None,
            table_components: None,
            entity_table_rows: None,
            sparse_sets: &world.storages().sparse_sets,
            component_sparse_set: None,
            entities: None,
        }
    }

    const IS_DENSE: bool = false;
    const IS_ARCHETYPAL: bool = false;

    unsafe fn set_archetype(
        &mut self,
        state: &Self::State,
        archetype: &'w bevy::ecs::archetype::Archetype,
        tables: &'w bevy::ecs::storage::Tables,
    ) {
        let table = &tables[archetype.table_id()];
        for (&component, meta) in std::iter::zip(&*state.components, &*state.meta) {
            if let Some(column) = table.get_column(component) {
                self.storage = Some(StorageType::Table);
                self.size_bytes = meta.size_bytes;
                self.dyn_ctor = Some(meta.dyn_ctor);
                self.entity_table_rows = Some(archetype.entity_table_rows().into());
                self.table_components = Some(column.get_data_ptr());
                return;
            }
        }
        for (&component, meta) in std::iter::zip(&*state.components, &*state.meta) {
            if let Some(sparse_set) = self.sparse_sets.get(component) {
                self.storage = Some(StorageType::SparseSet);
                self.size_bytes = meta.size_bytes;
                self.dyn_ctor = Some(meta.dyn_ctor);
                self.entities = Some(archetype.entities().into());
                self.component_sparse_set = Some(sparse_set);
                return;
            }
        }
        // At least one of the components must be present in the table/sparse set.
        debug_unreachable()
    }

    unsafe fn archetype_fetch(&mut self, archetype_index: usize) -> Self::Item {
        let ptr = match self.storage {
            None => debug_unreachable(),
            Some(StorageType::Table) => {
                let (entity_table_rows, table_components) = self
                    .entity_table_rows
                    .zip(self.table_components)
                    .unwrap_or_else(|| debug_unreachable());
                let table_row = *entity_table_rows.get(archetype_index);
                table_components.byte_add(table_row * self.size_bytes)
            }
            Some(StorageType::SparseSet) => {
                let (entities, sparse_set) = self
                    .entities
                    .zip(self.component_sparse_set)
                    .unwrap_or_else(|| debug_unreachable());
                let entity = *entities.get(archetype_index);
                sparse_set
                    .get(entity)
                    .unwrap_or_else(|| debug_unreachable())
            }
        };
        let dyn_ctor = self.dyn_ctor.unwrap_or_else(|| debug_unreachable());
        dyn_ctor.cast(ptr)
    }

    unsafe fn set_table(&mut self, state: &Self::State, table: &'w bevy::ecs::storage::Table) {
        self.storage = Some(StorageType::Table);
        for (&component, meta) in std::iter::zip(&*state.components, &*state.meta) {
            if let Some(column) = table.get_column(component) {
                self.table_components = Some(column.get_data_ptr());
                self.size_bytes = meta.size_bytes;
                self.dyn_ctor = Some(meta.dyn_ctor);
            }
        }
        // At least one of the components must be present in the table.
        debug_unreachable()
    }

    unsafe fn table_fetch(&mut self, table_row: usize) -> Self::Item {
        let (table_components, dyn_ctor) = self
            .table_components
            .zip(self.dyn_ctor)
            .unwrap_or_else(|| debug_unreachable());
        let ptr = table_components.byte_add(table_row * self.size_bytes);
        dyn_ctor.cast(ptr)
    }

    fn update_component_access(
        state: &Self::State,
        access: &mut bevy::ecs::query::FilteredAccess<ComponentId>,
    ) {
        for &component in &*state.components {
            assert!(
                !access.access().has_write(component),
                "&{} conflicts with a previous access in this query. Shared access cannot coincide with exclusive access.",
                    std::any::type_name::<Trait>(),
            );
            access.add_read(component);
        }
    }

    fn update_archetype_component_access(
        state: &Self::State,
        archetype: &bevy::ecs::archetype::Archetype,
        access: &mut bevy::ecs::query::Access<bevy::ecs::archetype::ArchetypeComponentId>,
    ) {
        for &component in &*state.components {
            if let Some(archetype_component_id) = archetype.get_archetype_component_id(component) {
                access.add_read(archetype_component_id);
            }
        }
    }
}

pub struct WriteTraitFetch<'w, Trait: ?Sized + DynQuery> {
    size_bytes: usize,
    dyn_ctor: Option<DynCtor<Trait>>,
    storage: Option<StorageType>,
    // T::Storage = TableStorage
    table_components: Option<Ptr<'w>>,
    entity_table_rows: Option<ThinSlicePtr<'w, usize>>,
    // T::Storage = SparseStorage
    sparse_sets: &'w SparseSets,
    component_sparse_set: Option<&'w ComponentSparseSet>,
    entities: Option<ThinSlicePtr<'w, Entity>>,
}

unsafe impl<'w, Trait: ?Sized + DynQuery> Fetch<'w> for WriteTraitFetch<'w, Trait> {
    type Item = &'w mut Trait;
    type State = DynQueryState<Trait>;

    unsafe fn init(
        world: &'w World,
        _state: &Self::State,
        _last_change_tick: u32,
        _change_tick: u32,
    ) -> Self {
        Self {
            size_bytes: 0,
            dyn_ctor: None,
            storage: None,
            table_components: None,
            entity_table_rows: None,
            sparse_sets: &world.storages().sparse_sets,
            component_sparse_set: None,
            entities: None,
        }
    }

    const IS_DENSE: bool = false;
    const IS_ARCHETYPAL: bool = false;

    unsafe fn set_archetype(
        &mut self,
        state: &Self::State,
        archetype: &'w bevy::ecs::archetype::Archetype,
        tables: &'w bevy::ecs::storage::Tables,
    ) {
        let table = &tables[archetype.table_id()];
        for (&component, meta) in std::iter::zip(&*state.components, &*state.meta) {
            if let Some(column) = table.get_column(component) {
                self.storage = Some(StorageType::Table);
                self.size_bytes = meta.size_bytes;
                self.dyn_ctor = Some(meta.dyn_ctor);
                self.entity_table_rows = Some(archetype.entity_table_rows().into());
                self.table_components = Some(column.get_data_ptr());
                return;
            }
        }
        for (&component, meta) in std::iter::zip(&*state.components, &*state.meta) {
            if let Some(sparse_set) = self.sparse_sets.get(component) {
                self.storage = Some(StorageType::SparseSet);
                self.size_bytes = meta.size_bytes;
                self.dyn_ctor = Some(meta.dyn_ctor);
                self.entities = Some(archetype.entities().into());
                self.component_sparse_set = Some(sparse_set);
                return;
            }
        }
        // At least one of the components must be present in the table/sparse set.
        debug_unreachable()
    }

    unsafe fn archetype_fetch(&mut self, archetype_index: usize) -> Self::Item {
        let ptr = match self.storage {
            None => debug_unreachable(),
            Some(StorageType::Table) => {
                let (entity_table_rows, table_components) = self
                    .entity_table_rows
                    .zip(self.table_components)
                    .unwrap_or_else(|| debug_unreachable());
                let table_row = *entity_table_rows.get(archetype_index);
                table_components.byte_add(table_row * self.size_bytes)
            }
            Some(StorageType::SparseSet) => {
                let (entities, sparse_set) = self
                    .entities
                    .zip(self.component_sparse_set)
                    .unwrap_or_else(|| debug_unreachable());
                let entity = *entities.get(archetype_index);
                sparse_set
                    .get(entity)
                    .unwrap_or_else(|| debug_unreachable())
            }
        };
        let dyn_ctor = self.dyn_ctor.unwrap_or_else(|| debug_unreachable());
        // Is `assert_unique` correct here??
        dyn_ctor.cast_mut(ptr.assert_unique())
    }

    unsafe fn set_table(&mut self, state: &Self::State, table: &'w bevy::ecs::storage::Table) {
        self.storage = Some(StorageType::Table);
        for (&component, meta) in std::iter::zip(&*state.components, &*state.meta) {
            if let Some(column) = table.get_column(component) {
                self.table_components = Some(column.get_data_ptr());
                self.size_bytes = meta.size_bytes;
                self.dyn_ctor = Some(meta.dyn_ctor);
                return;
            }
        }
        // At least one of the components must be present in the table.
        debug_unreachable()
    }

    unsafe fn table_fetch(&mut self, table_row: usize) -> Self::Item {
        let (table_components, dyn_ctor) = self
            .table_components
            .zip(self.dyn_ctor)
            .unwrap_or_else(|| debug_unreachable());
        let ptr = table_components.byte_add(table_row * self.size_bytes);
        // Is `assert_unique` correct here??
        dyn_ctor.cast_mut(ptr.assert_unique())
    }

    fn update_component_access(
        state: &Self::State,
        access: &mut bevy::ecs::query::FilteredAccess<ComponentId>,
    ) {
        for &component in &*state.components {
            assert!(
                !access.access().has_write(component),
                "&mut {} conflicts with a previous access in this query. Mutable component access must be unique.",
                    std::any::type_name::<Trait>(),
            );
            access.add_write(component);
        }
    }

    fn update_archetype_component_access(
        state: &Self::State,
        archetype: &bevy::ecs::archetype::Archetype,
        access: &mut bevy::ecs::query::Access<bevy::ecs::archetype::ArchetypeComponentId>,
    ) {
        for &component in &*state.components {
            if let Some(archetype_component_id) = archetype.get_archetype_component_id(component) {
                access.add_write(archetype_component_id);
            }
        }
    }
}

/// `WorldQuery` adapter that fetches all implementations of a given trait for an entity.
pub struct All<T: ?Sized>(T);

#[doc(hidden)]
pub type CombinedReadTraitsIter<'a, Trait> =
    std::iter::Chain<ReadTableTraitsIter<'a, Trait>, ReadSparseTraitsIter<'a, Trait>>;

#[doc(hidden)]
pub struct ReadTableTraitsIter<'a, Trait: ?Sized> {
    components: std::slice::Iter<'a, ComponentId>,
    meta: std::slice::Iter<'a, TraitImplMeta<Trait>>,
    table: &'a Table,
    table_row: usize,
}

impl<'a, Trait: ?Sized> Iterator for ReadTableTraitsIter<'a, Trait> {
    type Item = &'a Trait;
    fn next(&mut self) -> Option<Self::Item> {
        let (column, meta) = std::iter::zip(&mut self.components, &mut self.meta)
            .find_map(|(&component, meta)| self.table.get_column(component).zip(Some(meta)))?;
        let table_components = column.get_data_ptr();
        let trait_object = unsafe {
            let ptr = table_components.byte_add(self.table_row * meta.size_bytes);
            meta.dyn_ctor.cast(ptr)
        };
        Some(trait_object)
    }
}

#[doc(hidden)]
pub struct ReadSparseTraitsIter<'a, Trait: ?Sized> {
    components: std::slice::Iter<'a, ComponentId>,
    meta: std::slice::Iter<'a, TraitImplMeta<Trait>>,
    entity: Entity,
    sparse_sets: &'a SparseSets,
}

impl<'a, Trait: ?Sized> Iterator for ReadSparseTraitsIter<'a, Trait> {
    type Item = &'a Trait;
    fn next(&mut self) -> Option<Self::Item> {
        let (ptr, meta) = std::iter::zip(&mut self.components, &mut self.meta).find_map(
            |(&component, meta)| {
                self.sparse_sets
                    .get(component)
                    .and_then(|set| set.get(self.entity))
                    .zip(Some(meta))
            },
        )?;
        let trait_object = unsafe { meta.dyn_ctor.cast(ptr) };
        Some(trait_object)
    }
}

#[doc(hidden)]
pub type CombinedWriteTraitsIter<'a, Trait> =
    std::iter::Chain<WriteTableTraitsIter<'a, Trait>, WriteSparseTraitsIter<'a, Trait>>;

#[doc(hidden)]
pub struct WriteTableTraitsIter<'a, Trait: ?Sized + DynQuery> {
    components: std::slice::Iter<'a, ComponentId>,
    meta: std::slice::Iter<'a, TraitImplMeta<Trait>>,
    table: &'a Table,
    table_row: usize,
}

impl<'a, Trait: ?Sized + DynQuery> Iterator for WriteTableTraitsIter<'a, Trait> {
    type Item = &'a mut Trait;
    fn next(&mut self) -> Option<Self::Item> {
        let (column, meta) = std::iter::zip(&mut self.components, &mut self.meta)
            .find_map(|(&component, meta)| self.table.get_column(component).zip(Some(meta)))?;
        let table_components = column.get_data_ptr();
        let trait_object = unsafe {
            let ptr = table_components.byte_add(self.table_row * meta.size_bytes);
            meta.dyn_ctor.cast_mut(ptr.assert_unique())
        };
        Some(trait_object)
    }
}

#[doc(hidden)]
pub struct WriteSparseTraitsIter<'a, Trait: ?Sized> {
    components: std::slice::Iter<'a, ComponentId>,
    meta: std::slice::Iter<'a, TraitImplMeta<Trait>>,
    entity: Entity,
    sparse_sets: &'a SparseSets,
}

impl<'a, Trait: ?Sized> Iterator for WriteSparseTraitsIter<'a, Trait> {
    type Item = &'a mut Trait;
    fn next(&mut self) -> Option<Self::Item> {
        let (ptr, meta) = std::iter::zip(&mut self.components, &mut self.meta).find_map(
            |(&component, meta)| {
                self.sparse_sets
                    .get(component)
                    .and_then(|set| set.get(self.entity))
                    .zip(Some(meta))
            },
        )?;
        let trait_object = unsafe { meta.dyn_ctor.cast_mut(ptr.assert_unique()) };
        Some(trait_object)
    }
}

#[doc(hidden)]
pub struct ReadAllTraitsFetch<'w, Trait: ?Sized + DynQuery> {
    registry: &'w TraitImplRegistry<Trait>,
    // T::Storage = TableStorage
    entity_table_rows: Option<ThinSlicePtr<'w, usize>>,
    table: Option<&'w Table>,
    // T::Storage = SparseStorage
    sparse_sets: &'w SparseSets,
}

#[doc(hidden)]
pub struct WriteAllTraitsFetch<'w, Trait: ?Sized + DynQuery> {
    registry: &'w TraitImplRegistry<Trait>,
    // T::Storage = TableStorage
    entity_table_rows: Option<ThinSlicePtr<'w, usize>>,
    table: Option<&'w Table>,
    // T::Storage = SparseStorage
    sparse_sets: &'w SparseSets,
}

/// Read-access to all components implementing a trait for a given entity.
pub struct ReadTraits<'a, Trait: ?Sized + DynQuery> {
    registry: &'a TraitImplRegistry<Trait>,
    // T::Storage = TableStorage
    table: &'a Table,
    table_row: usize,
    // T::Storage = SparseStorage
    sparse_sets: &'a SparseSets,
}

/// Write-access to all components implementing a trait for a given entity.
pub struct WriteTraits<'a, Trait: ?Sized + DynQuery> {
    registry: &'a TraitImplRegistry<Trait>,
    // T::Storage = TableStorage
    table: &'a Table,
    table_row: usize,
    // T::Storage = SparseStorage
    sparse_sets: &'a SparseSets,
}

unsafe impl<'w, Trait: ?Sized + DynQuery> Fetch<'w> for ReadAllTraitsFetch<'w, Trait> {
    type Item = ReadTraits<'w, Trait>;
    type State = DynQueryState<Trait>;

    unsafe fn init(
        world: &'w World,
        _state: &Self::State,
        _last_change_tick: u32,
        _change_tick: u32,
    ) -> Self {
        Self {
            entity_table_rows: None,
            // Nothing will conflict with this resource reference,
            // since no one outside of this crate can access the registry type.
            registry: world.get_resource().unwrap_or_else(|| debug_unreachable()),
            table: None,
            sparse_sets: &world.storages().sparse_sets,
        }
    }

    const IS_DENSE: bool = false;
    const IS_ARCHETYPAL: bool = false;

    unsafe fn set_archetype(
        &mut self,
        _state: &Self::State,
        archetype: &'w bevy::ecs::archetype::Archetype,
        tables: &'w bevy::ecs::storage::Tables,
    ) {
        self.entity_table_rows = Some(archetype.entity_table_rows().into());
        self.table = Some(&tables[archetype.table_id()]);
    }

    unsafe fn archetype_fetch(&mut self, archetype_index: usize) -> Self::Item {
        let entity_table_rows = self
            .entity_table_rows
            .unwrap_or_else(|| debug_unreachable());
        let table_row = *entity_table_rows.get(archetype_index);
        let table = self.table.unwrap_or_else(|| debug_unreachable());

        ReadTraits {
            registry: self.registry,
            table,
            table_row,
            sparse_sets: self.sparse_sets,
        }
    }

    unsafe fn set_table(&mut self, _state: &Self::State, table: &'w bevy::ecs::storage::Table) {
        self.table = Some(table);
    }

    unsafe fn table_fetch(&mut self, table_row: usize) -> Self::Item {
        let table = self.table.unwrap_or_else(|| debug_unreachable());

        ReadTraits {
            registry: self.registry,
            table,
            table_row: table_row,
            sparse_sets: self.sparse_sets,
        }
    }

    fn update_component_access(
        state: &Self::State,
        access: &mut bevy::ecs::query::FilteredAccess<ComponentId>,
    ) {
        for &component in &*state.components {
            assert!(
                !access.access().has_write(component),
                "&{} conflicts with a previous access in this query. Shared access cannot coincide with exclusive access.",
                    std::any::type_name::<Trait>(),
            );
            access.add_read(component);
        }
    }

    fn update_archetype_component_access(
        state: &Self::State,
        archetype: &bevy::ecs::archetype::Archetype,
        access: &mut bevy::ecs::query::Access<bevy::ecs::archetype::ArchetypeComponentId>,
    ) {
        for &component in &*state.components {
            if let Some(archetype_component_id) = archetype.get_archetype_component_id(component) {
                access.add_read(archetype_component_id);
            }
        }
    }
}

unsafe impl<'w, Trait: ?Sized + DynQuery> Fetch<'w> for WriteAllTraitsFetch<'w, Trait> {
    type Item = WriteTraits<'w, Trait>;
    type State = DynQueryState<Trait>;

    unsafe fn init(
        world: &'w World,
        _state: &Self::State,
        _last_change_tick: u32,
        _change_tick: u32,
    ) -> Self {
        Self {
            entity_table_rows: None,
            // Nothing will conflict with this resource reference,
            // since no one outside of this crate can access the registry type.
            registry: world.get_resource().unwrap_or_else(|| debug_unreachable()),
            table: None,
            sparse_sets: &world.storages().sparse_sets,
        }
    }

    const IS_DENSE: bool = false;
    const IS_ARCHETYPAL: bool = false;

    unsafe fn set_archetype(
        &mut self,
        _state: &Self::State,
        archetype: &'w bevy::ecs::archetype::Archetype,
        tables: &'w bevy::ecs::storage::Tables,
    ) {
        self.entity_table_rows = Some(archetype.entity_table_rows().into());
        self.table = Some(&tables[archetype.table_id()]);
    }

    unsafe fn archetype_fetch(&mut self, archetype_index: usize) -> Self::Item {
        let entity_table_rows = self
            .entity_table_rows
            .unwrap_or_else(|| debug_unreachable());
        let table_row = *entity_table_rows.get(archetype_index);
        let table = self.table.unwrap_or_else(|| debug_unreachable());

        WriteTraits {
            registry: self.registry,
            table,
            table_row,
            sparse_sets: self.sparse_sets,
        }
    }

    unsafe fn set_table(&mut self, _state: &Self::State, table: &'w bevy::ecs::storage::Table) {
        self.table = Some(table);
    }

    unsafe fn table_fetch(&mut self, table_row: usize) -> Self::Item {
        let table = self.table.unwrap_or_else(|| debug_unreachable());

        WriteTraits {
            registry: self.registry,
            table,
            table_row: table_row,
            sparse_sets: self.sparse_sets,
        }
    }

    fn update_component_access(
        state: &Self::State,
        access: &mut bevy::ecs::query::FilteredAccess<ComponentId>,
    ) {
        for &component in &*state.components {
            assert!(
                !access.access().has_write(component),
                "&mut {} conflicts with a previous access in this query. Mutable component access must be unique.",
                    std::any::type_name::<Trait>(),
            );
            access.add_write(component);
        }
    }

    fn update_archetype_component_access(
        state: &Self::State,
        archetype: &bevy::ecs::archetype::Archetype,
        access: &mut bevy::ecs::query::Access<bevy::ecs::archetype::ArchetypeComponentId>,
    ) {
        for &component in &*state.components {
            if let Some(archetype_component_id) = archetype.get_archetype_component_id(component) {
                access.add_write(archetype_component_id);
            }
        }
    }
}

impl<'w, Trait: ?Sized + DynQuery> IntoIterator for ReadTraits<'w, Trait> {
    type Item = &'w Trait;
    type IntoIter = CombinedReadTraitsIter<'w, Trait>;
    fn into_iter(self) -> Self::IntoIter {
        let table = ReadTableTraitsIter {
            components: self.registry.table_components.iter(),
            meta: self.registry.table_meta.iter(),
            table: self.table,
            table_row: self.table_row,
        };
        let sparse = ReadSparseTraitsIter {
            components: self.registry.sparse_components.iter(),
            meta: self.registry.sparse_meta.iter(),
            entity: self.table.entities()[self.table_row],
            sparse_sets: self.sparse_sets,
        };
        table.chain(sparse)
    }
}

impl<'w, Trait: ?Sized + DynQuery> IntoIterator for &ReadTraits<'w, Trait> {
    type Item = &'w Trait;
    type IntoIter = CombinedReadTraitsIter<'w, Trait>;
    fn into_iter(self) -> Self::IntoIter {
        let table = ReadTableTraitsIter {
            components: self.registry.table_components.iter(),
            meta: self.registry.table_meta.iter(),
            table: self.table,
            table_row: self.table_row,
        };
        let sparse = ReadSparseTraitsIter {
            components: self.registry.sparse_components.iter(),
            meta: self.registry.sparse_meta.iter(),
            entity: self.table.entities()[self.table_row],
            sparse_sets: self.sparse_sets,
        };
        table.chain(sparse)
    }
}

impl<'w, Trait: ?Sized + DynQuery> IntoIterator for WriteTraits<'w, Trait> {
    type Item = &'w mut Trait;
    type IntoIter = CombinedWriteTraitsIter<'w, Trait>;
    fn into_iter(self) -> Self::IntoIter {
        let table = WriteTableTraitsIter {
            components: self.registry.table_components.iter(),
            meta: self.registry.table_meta.iter(),
            table: self.table,
            table_row: self.table_row,
        };
        let sparse = WriteSparseTraitsIter {
            components: self.registry.sparse_components.iter(),
            meta: self.registry.sparse_meta.iter(),
            entity: self.table.entities()[self.table_row],
            sparse_sets: self.sparse_sets,
        };
        table.chain(sparse)
    }
}

impl<'world, 'local, Trait: ?Sized + DynQuery> IntoIterator for &'local WriteTraits<'world, Trait> {
    type Item = &'local Trait;
    type IntoIter = CombinedReadTraitsIter<'local, Trait>;
    fn into_iter(self) -> Self::IntoIter {
        let table = ReadTableTraitsIter {
            components: self.registry.table_components.iter(),
            meta: self.registry.table_meta.iter(),
            table: self.table,
            table_row: self.table_row,
        };
        let sparse = ReadSparseTraitsIter {
            components: self.registry.sparse_components.iter(),
            meta: self.registry.sparse_meta.iter(),
            entity: self.table.entities()[self.table_row],
            sparse_sets: self.sparse_sets,
        };
        table.chain(sparse)
    }
}

impl<'world, 'local, Trait: ?Sized + DynQuery> IntoIterator
    for &'local mut WriteTraits<'world, Trait>
{
    type Item = &'local mut Trait;
    type IntoIter = CombinedWriteTraitsIter<'local, Trait>;
    fn into_iter(self) -> Self::IntoIter {
        let table = WriteTableTraitsIter {
            components: self.registry.table_components.iter(),
            meta: self.registry.table_meta.iter(),
            table: self.table,
            table_row: self.table_row,
        };
        let sparse = WriteSparseTraitsIter {
            components: self.registry.sparse_components.iter(),
            meta: self.registry.sparse_meta.iter(),
            entity: self.table.entities()[self.table_row],
            sparse_sets: self.sparse_sets,
        };
        table.chain(sparse)
    }
}

unsafe impl<'w, Trait: ?Sized + DynQuery> WorldQuery for All<&'w Trait> {
    type ReadOnly = Self;
    type State = DynQueryState<Trait>;

    fn shrink<'wlong: 'wshort, 'wshort>(
        item: bevy::ecs::query::QueryItem<'wlong, Self>,
    ) -> bevy::ecs::query::QueryItem<'wshort, Self> {
        item
    }
}

unsafe impl<Trait: ?Sized + DynQuery> ReadOnlyWorldQuery for All<&Trait> {}

impl<'w, Trait: ?Sized + DynQuery> WorldQueryGats<'w> for All<&Trait> {
    type Fetch = ReadAllTraitsFetch<'w, Trait>;
    type _State = DynQueryState<Trait>;
}

unsafe impl<'w, Trait: ?Sized + DynQuery> WorldQuery for All<&'w mut Trait> {
    type ReadOnly = All<&'w Trait>;
    type State = DynQueryState<Trait>;

    fn shrink<'wlong: 'wshort, 'wshort>(
        item: bevy::ecs::query::QueryItem<'wlong, Self>,
    ) -> bevy::ecs::query::QueryItem<'wshort, Self> {
        item
    }
}

impl<'w, Trait: ?Sized + DynQuery> WorldQueryGats<'w> for All<&mut Trait> {
    type Fetch = WriteAllTraitsFetch<'w, Trait>;
    type _State = DynQueryState<Trait>;
}

#[track_caller]
#[inline(always)]
unsafe fn debug_unreachable() -> ! {
    #[cfg(debug_assertions)]
    unreachable!();

    #[cfg(not(debug_assertions))]
    std::hint::unreachable_unchecked();
}
