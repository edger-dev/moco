use crate::spec::CellSpec;

pub trait Cell {
    const SPEC: &'static CellSpec;
}
