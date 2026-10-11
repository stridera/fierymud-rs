//! Test support for the "first sweep after a boot" benchmarks: a prod world
//! has thousands of archetypes (mobs differ in dozens of optional
//! components), and the first structural move into each builds the target
//! archetype and table. Benchmarks that run over a handful of archetypes
//! hide that cost; [`shape`] spreads entities over ~4k of them.

use bevy_ecs::prelude::*;

/// One const-generic component per bit: gives entities a distinct
/// component set per hash value.
#[derive(Component)]
struct Bit<const N: usize>;
#[derive(Component)]
struct Pad<const N: usize>;

macro_rules! insert_all {
    ($ent:expr, $ty:ident; $($n:literal),*) => {
        $( $ent.insert($ty::<$n>); )*
    };
}

/// Give `e` forty padding components (prod mobs carry that many, which is
/// what makes a table expensive to build) plus a hash-chosen subset of
/// twelve marker components: about 4k distinct archetypes over 5.5k
/// entities.
pub(crate) fn shape(world: &mut World, e: Entity, h: usize) {
    let mut ent = world.entity_mut(e);
    insert_all!(ent, Pad; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39);
    let h = h.wrapping_mul(2_654_435_761) >> 7;
    macro_rules! bits {
        ($($n:literal),*) => { $( if h >> $n & 1 != 0 { ent.insert(Bit::<$n>); } )* };
    }
    bits!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11);
}
