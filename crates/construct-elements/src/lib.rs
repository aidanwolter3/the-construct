//! The elements of a content-addressed construct (decision
//! `content-addressed-construct`): canonical JSON and object ids
//! ([`canonical`]), component definitions ([`def`]) and their templates
//! ([`template`]), references ([`refs`]) and materializing an element for the
//! renderer ([`materialize`]). Pure: the pod and the engine supply the objects.

pub mod canonical;
pub mod def;
pub mod materialize;
pub mod refs;
pub mod template;

pub use canonical::{canonical, is_object_id, object_id};
pub use materialize::{
    Keyed, Materialized, Objects, instance_id, keyed, materialize, materialize_with, reach,
};
pub use refs::{EdgeRef, Ref};
