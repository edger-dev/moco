use moco_core::deps::Facet;

#[derive(Facet)]
pub struct Spec {
    pub name: String,
    pub version: String,
    pub title: String,
    pub description: String,
}
