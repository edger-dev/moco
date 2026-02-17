use facet::Facet;

#[derive(Facet)]
pub struct CellSpec {
    pub name: &'static str,
    pub version: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}
