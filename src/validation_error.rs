use thiserror::Error;

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Left hand side of condition branch is missing comparison.")]
    ConditionMissingComparison,
    #[error("A computed property key may only be a well known symbol.")]
    ComputedPropertyNameNotWellKnownSymbol,
}

impl ValidationError {
    pub(crate) fn to_code(&self) -> &str {
        match self {
            ValidationError::ConditionMissingComparison => "E0001",
            ValidationError::ComputedPropertyNameNotWellKnownSymbol => "E0002",
        }
    }

    pub(crate) fn to_help(&self) -> Option<&str> {
        match self {
            ValidationError::ComputedPropertyNameNotWellKnownSymbol => Some(
                "See https://www.typescriptlang.org/docs/handbook/symbols.html#well-known-symbols",
            ),
            _ => None,
        }
    }
}
