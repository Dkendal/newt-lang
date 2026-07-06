#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_macros)]

extern crate alloc;

#[cfg(test)]
extern crate quickcheck;

#[cfg(test)]
#[macro_use(quickcheck)]
extern crate quickcheck_macros;

pub mod ast;
pub mod corpus;
pub mod extends_result;
pub mod module_loader;
pub mod panic_report;
pub mod parser;
pub mod pretty;
pub mod report;
pub mod runtime;
pub mod test_codegen;
pub mod test_harness;
pub mod typescript;
pub mod validation_error;

#[cfg(test)]
mod test_support;

/// Function composition macro
#[macro_export]
macro_rules! compose {
    ($($rest:expr),+) => {
        |x| { compose!(expand x, $($rest),*) }
    };
    (expand $inner:expr, $function:expr, $($rest:expr),*) => {
        compose!(expand $function($inner), $($rest),*)
    };
    (expand $inner:expr, $function:expr) => {
        $function($inner)
    };
}
