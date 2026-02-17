use facet::Facet;

#[derive(Facet)]
pub struct FuncSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}
