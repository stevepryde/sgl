//! The identities a `Scene` issues for its content. Each holds the index of
//! the content's records and a generation no other content of any scene
//! shares, so an identity of removed content, or one another scene issued,
//! names nothing. Only a scene issues them.

/// Crate access to an identity's parts, for the scene that issues it.
pub(crate) trait Identity: Copy {
    fn issue(index: usize, generation: u64) -> Self;
    fn index(self) -> usize;
    fn generation(self) -> u64;
}

macro_rules! identity {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name {
            index: u32,
            generation: u64,
        }

        impl Identity for $name {
            fn issue(index: usize, generation: u64) -> Self {
                Self {
                    index: u32::try_from(index).expect("content indices fit in u32"),
                    generation,
                }
            }
            fn index(self) -> usize {
                self.index as usize
            }
            fn generation(self) -> u64 {
                self.generation
            }
        }
    };
}

identity!(
    /// A material of a scene: its surface values and the textures they sample.
    MaterialId
);
identity!(
    /// A model of a scene: an ordered list of meshes, each with one material.
    ModelId
);
identity!(
    /// An instance of a scene: a model placed in the world.
    InstanceId
);
identity!(
    /// A point or spot light of a scene.
    LightId
);
identity!(
    /// An environment map of a scene.
    EnvironmentId
);
identity!(
    /// An image a scene's decals project.
    DecalImageId
);
identity!(
    /// A decal of a scene: a box that projects images onto surfaces.
    DecalId
);
identity!(
    /// A shader of a scene: a game's WGSL module its materials name.
    ShaderId
);
