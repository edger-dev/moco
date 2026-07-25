use crate::spec::FuncSpec;

pub trait Func {
    const SPEC: &'static FuncSpec;
}
